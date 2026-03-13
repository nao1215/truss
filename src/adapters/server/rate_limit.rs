//! Per-IP token-bucket rate limiter with sharded locking.
//!
//! When `TRUSS_RATE_LIMIT_RPS` is set to a positive value, each client IP
//! address is allocated a token bucket that refills at the configured rate.
//! Requests that arrive when the bucket is empty receive HTTP 429 Too Many
//! Requests.  The limiter is disabled (all requests allowed) when the RPS
//! value is zero or unset.
//!
//! The bucket map is split across [`NUM_SHARDS`] independent mutexes so that
//! concurrent worker threads rarely contend on the same lock.  Cleanup sweeps
//! run per-shard, avoiding a global stop-the-world pause.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

/// Number of independent shards.  Must be a power of two for fast modulo.
const NUM_SHARDS: usize = 16;

/// Per-shard entry count before a cleanup sweep is triggered.
const CLEANUP_THRESHOLD_PER_SHARD: usize = 1_000;

/// Idle duration (in seconds) after which an IP entry is evicted.
const CLEANUP_IDLE_SECS: f64 = 300.0;

/// A shared, thread-safe per-IP rate limiter.
///
/// Each IP address gets an independent token bucket.  The bucket map is
/// partitioned into [`NUM_SHARDS`] shards keyed by the hash of the IP
/// address, so concurrent threads rarely contend on the same mutex.
///
/// A lazy cleanup is performed per-shard once its entry count exceeds
/// [`CLEANUP_THRESHOLD_PER_SHARD`]: entries idle for longer than
/// [`CLEANUP_IDLE_SECS`] are evicted.
pub struct RateLimiter {
    /// Maximum tokens (burst capacity).
    burst: f64,
    /// Tokens added per second (refill rate).
    rate: f64,
    /// Sharded per-IP bucket state.
    shards: [Mutex<HashMap<IpAddr, Bucket>>; NUM_SHARDS],
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// Returns the shard index for a given IP address.
fn shard_index(ip: IpAddr) -> usize {
    let mut hasher = DefaultHasher::new();
    ip.hash(&mut hasher);
    hasher.finish() as usize & (NUM_SHARDS - 1)
}

impl RateLimiter {
    /// Creates a new rate limiter.
    ///
    /// - `rate` — tokens added per second (requests per second per IP).
    /// - `burst` — maximum tokens in the bucket. When a client has not sent
    ///   requests for a while the bucket fills up to this value, allowing
    ///   short bursts above the sustained rate.
    pub fn new(rate: f64, burst: f64) -> Self {
        Self {
            burst,
            rate,
            shards: std::array::from_fn(|_| Mutex::new(HashMap::new())),
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    ///
    /// Each call consumes one token from the bucket for `ip`. If the bucket
    /// is empty the request is rejected.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let idx = shard_index(ip);
        let mut shard = self.shards[idx]
            .lock()
            .expect("rate limiter shard lock poisoned");

        // Lazy cleanup when this shard grows large.
        if shard.len() > CLEANUP_THRESHOLD_PER_SHARD {
            shard.retain(|_, bucket| {
                now.saturating_duration_since(bucket.last_refill)
                    .as_secs_f64()
                    < CLEANUP_IDLE_SECS
            });
        }

        let bucket = shard.entry(ip).or_insert_with(|| Bucket {
            tokens: self.burst,
            last_refill: now,
        });

        // Refill tokens based on elapsed time.
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.last_refill = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn allows_requests_within_burst() {
        let limiter = RateLimiter::new(10.0, 5.0);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // First 5 requests should be allowed (burst = 5).
        for i in 0..5 {
            assert!(limiter.check(ip), "request {i} should be allowed");
        }

        // 6th request should be rejected.
        assert!(!limiter.check(ip), "request 6 should be rejected");
    }

    #[test]
    fn refills_tokens_over_time() {
        let limiter = RateLimiter::new(10.0, 2.0);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // Exhaust the burst.
        assert!(limiter.check(ip));
        assert!(limiter.check(ip));
        assert!(!limiter.check(ip));

        // Wait for ~1 token to refill (100ms at 10 rps = 1 token).
        thread::sleep(Duration::from_millis(120));

        assert!(limiter.check(ip), "should have refilled at least 1 token");
    }

    #[test]
    fn independent_per_ip() {
        let limiter = RateLimiter::new(10.0, 1.0);
        let ip_a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ip_b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        assert!(limiter.check(ip_a));
        assert!(!limiter.check(ip_a));

        // ip_b should still have its own bucket.
        assert!(limiter.check(ip_b));
    }

    #[test]
    fn ipv6_addresses_work() {
        let limiter = RateLimiter::new(10.0, 1.0);
        let ip = IpAddr::V6(Ipv6Addr::LOCALHOST);

        assert!(limiter.check(ip));
        assert!(!limiter.check(ip));
    }

    #[test]
    fn tokens_cap_at_burst() {
        let limiter = RateLimiter::new(1000.0, 3.0);
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

        // Wait a bit to ensure tokens would exceed burst if uncapped.
        thread::sleep(Duration::from_millis(50));

        // Should only get 3 (burst cap), not 50+ (rate * elapsed).
        assert!(limiter.check(ip));
        assert!(limiter.check(ip));
        assert!(limiter.check(ip));
        assert!(!limiter.check(ip));
    }

    #[test]
    fn cleanup_removes_idle_entries() {
        let limiter = RateLimiter::new(10.0, 1.0);

        // On some platforms (Windows), Instant cannot represent times before
        // process start, so checked_sub may return None.  Skip the test in
        // that case rather than panicking.
        let Some(old) = Instant::now().checked_sub(Duration::from_secs(600)) else {
            return; // Platform does not support backward Instant arithmetic.
        };

        // Insert more than CLEANUP_THRESHOLD_PER_SHARD entries into one shard
        // by using IPs that hash to the same shard.  We pick a fixed IP as
        // the "target shard" and fill only that shard.
        let target_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let target_shard = shard_index(target_ip);

        {
            // Fill the target shard with old entries.
            let mut shard = limiter.shards[target_shard].lock().unwrap();
            for i in 0..(CLEANUP_THRESHOLD_PER_SHARD + 100) {
                // Generate IPs; we insert directly into the shard regardless
                // of their actual hash since we're manipulating internal state.
                let ip = IpAddr::V4(Ipv4Addr::from((i as u32).to_be_bytes()));
                shard.insert(
                    ip,
                    Bucket {
                        tokens: 0.0,
                        last_refill: old,
                    },
                );
            }
            assert!(shard.len() > CLEANUP_THRESHOLD_PER_SHARD);
        }

        // A check call on an IP in the same shard should trigger cleanup.
        limiter.check(target_ip);

        let shard = limiter.shards[target_shard].lock().unwrap();
        // Only the fresh IP should remain (old ones had idle > 300s).
        assert_eq!(shard.len(), 1);
    }

    #[test]
    fn shard_index_distributes_across_shards() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..256u32 {
            let ip = IpAddr::V4(Ipv4Addr::from(i.to_be_bytes()));
            seen.insert(shard_index(ip));
        }
        // With 256 distinct IPs we should hit most of the 16 shards.
        assert!(seen.len() >= NUM_SHARDS / 2, "poor distribution: {seen:?}");
    }

    #[test]
    fn concurrent_access_does_not_panic() {
        use std::sync::Arc;

        let limiter = Arc::new(RateLimiter::new(100.0, 10.0));
        let handles: Vec<_> = (0..8)
            .map(|t| {
                let limiter = Arc::clone(&limiter);
                thread::spawn(move || {
                    for i in 0..100u32 {
                        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, t, i as u8));
                        limiter.check(ip);
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
    }
}
