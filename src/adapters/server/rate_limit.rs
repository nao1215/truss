//! Per-IP token-bucket rate limiter.
//!
//! When `TRUSS_RATE_LIMIT_RPS` is set to a positive value, each client IP
//! address is allocated a token bucket that refills at the configured rate.
//! Requests that arrive when the bucket is empty receive HTTP 429 Too Many
//! Requests.  The limiter is disabled (all requests allowed) when the RPS
//! value is zero or unset.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

/// A shared, thread-safe per-IP rate limiter.
///
/// Each IP address gets an independent token bucket. A background-style
/// cleanup is performed lazily during `check` calls: once the number of
/// tracked IPs exceeds `CLEANUP_THRESHOLD`, entries that have been idle
/// for longer than `CLEANUP_IDLE_SECS` are evicted.
pub struct RateLimiter {
    /// Maximum tokens (burst capacity).
    burst: f64,
    /// Tokens added per second (refill rate).
    rate: f64,
    /// Per-IP bucket state, protected by a mutex.
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

/// Number of tracked IPs before a cleanup sweep is triggered.
const CLEANUP_THRESHOLD: usize = 10_000;
/// Idle duration (in seconds) after which an IP entry is evicted.
const CLEANUP_IDLE_SECS: f64 = 300.0;

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
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the request is allowed, `false` if rate-limited.
    ///
    /// Each call consumes one token from the bucket for `ip`. If the bucket
    /// is empty the request is rejected.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");

        // Lazy cleanup when the map grows large.
        if buckets.len() > CLEANUP_THRESHOLD {
            buckets.retain(|_, bucket| {
                now.duration_since(bucket.last_refill).as_secs_f64() < CLEANUP_IDLE_SECS
            });
        }

        let bucket = buckets.entry(ip).or_insert_with(|| Bucket {
            tokens: self.burst,
            last_refill: now,
        });

        // Refill tokens based on elapsed time.
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
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

        // Insert more than CLEANUP_THRESHOLD entries with old timestamps.
        {
            let mut buckets = limiter.buckets.lock().unwrap();
            let old = Instant::now() - Duration::from_secs(600);
            for i in 0..CLEANUP_THRESHOLD + 100 {
                let ip = IpAddr::V4(Ipv4Addr::from((i as u32).to_be_bytes()));
                buckets.insert(ip, Bucket {
                    tokens: 0.0,
                    last_refill: old,
                });
            }
        }

        // A check call should trigger cleanup and evict all old entries.
        let fresh_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        limiter.check(fresh_ip);

        let buckets = limiter.buckets.lock().unwrap();
        // Only the fresh IP should remain (old ones had idle > 300s).
        assert_eq!(buckets.len(), 1);
    }
}
