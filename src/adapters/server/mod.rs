mod auth;
#[cfg(feature = "azure")]
pub mod azure;
mod cache;
mod config;
#[cfg(feature = "gcs")]
pub mod gcs;
mod http_parse;
mod metrics;
mod multipart;
mod negotiate;
mod remote;
mod response;
#[cfg(feature = "s3")]
pub mod s3;

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub use config::StorageBackend;
use config::StorageBackendLabel;
pub use config::{DEFAULT_BIND_ADDR, DEFAULT_STORAGE_ROOT, LogHandler, ServerConfig};

use auth::{
    authorize_request, authorize_request_headers, authorize_signed_request,
    canonical_query_without_signature, extend_transform_query, parse_optional_bool_query,
    parse_optional_float_query, parse_optional_integer_query, parse_optional_u8_query,
    parse_query_params, required_query_param, signed_source_query, url_authority,
    validate_public_query_names,
};
use cache::{
    CacheLookup, TransformCache, compute_cache_key, compute_watermark_identity,
    try_versioned_cache_lookup,
};
use http_parse::{
    HttpRequest, parse_named, parse_optional_named, read_request_body, read_request_headers,
    request_has_json_content_type,
};
use metrics::{
    CACHE_HITS_TOTAL, CACHE_MISSES_TOTAL, RouteMetric, record_http_metrics,
    record_http_request_duration, record_storage_duration, record_transform_duration,
    record_transform_error, record_watermark_transform, render_metrics_text, status_code,
    storage_backend_index_from_config, uptime_seconds,
};
use multipart::{parse_multipart_boundary, parse_upload_request};
use negotiate::{
    CacheHitStatus, ImageResponsePolicy, PublicSourceKind, build_image_etag,
    build_image_response_headers, if_none_match_matches, negotiate_output_format,
};
use remote::{read_remote_watermark_bytes, resolve_source_bytes};
use response::{
    HttpResponse, NOT_FOUND_BODY, bad_request_response, service_unavailable_response,
    transform_error_response, unsupported_media_type_response, write_response,
    write_response_compressed,
};

use crate::{
    CropRegion, Fit, MediaType, Position, RawArtifact, Rgba8, Rotation, TransformOptions,
    TransformRequest, WatermarkInput, sniff_artifact, transform_raster, transform_svg,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;

/// Writes a line to stderr using a raw file-descriptor/handle write, bypassing
/// Rust's `std::io::Stderr` type whose internal `ReentrantLock` can interfere
/// with `MutexGuard` drop ordering in Rust 2024 edition, breaking HTTP
/// keep-alive.
pub(crate) fn stderr_write(msg: &str) {
    use std::io::Write;

    let bytes = msg.as_bytes();
    let mut buf = Vec::with_capacity(bytes.len() + 1);
    buf.extend_from_slice(bytes);
    buf.push(b'\n');

    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        // SAFETY: fd 2 (stderr) is always valid for the lifetime of the process.
        let mut f = unsafe { std::fs::File::from_raw_fd(2) };
        let _ = f.write_all(&buf);
        // Do not drop `f` — that would close fd 2 (stderr).
        std::mem::forget(f);
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::FromRawHandle;

        unsafe extern "system" {
            fn GetStdHandle(nStdHandle: u32) -> *mut std::ffi::c_void;
        }

        const STD_ERROR_HANDLE: u32 = (-12_i32) as u32;
        // SAFETY: GetStdHandle(STD_ERROR_HANDLE) returns the stderr handle
        // which is always valid for the lifetime of the process.
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        let mut f = unsafe { std::fs::File::from_raw_handle(handle) };
        let _ = f.write_all(&buf);
        // Do not drop `f` — that would close the stderr handle.
        std::mem::forget(f);
    }
}

const SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(60);
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Number of worker threads for handling incoming connections concurrently.
const WORKER_THREADS: usize = 8;
type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy)]
struct PublicCacheControl {
    max_age: u32,
    stale_while_revalidate: u32,
}

#[derive(Clone, Copy)]
struct ImageResponseConfig {
    disable_accept_negotiation: bool,
    public_cache_control: PublicCacheControl,
    transform_deadline: Duration,
}

/// RAII guard that holds a concurrency slot for an in-flight image transform.
///
/// The counter is incremented on successful acquisition and decremented when
/// the guard is dropped, ensuring the slot is always released even if the
/// caller returns early or panics.
struct TransformSlot {
    counter: Arc<AtomicU64>,
}

impl TransformSlot {
    fn try_acquire(counter: &Arc<AtomicU64>, limit: u64) -> Option<Self> {
        let prev = counter.fetch_add(1, Ordering::Relaxed);
        if prev >= limit {
            counter.fetch_sub(1, Ordering::Relaxed);
            None
        } else {
            Some(Self {
                counter: Arc::clone(counter),
            })
        }
    }
}

impl Drop for TransformSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Source selector used when generating a signed public transform URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignedUrlSource {
    /// Generates a signed `GET /images/by-path` URL.
    Path {
        /// The storage-relative source path.
        path: String,
        /// An optional source version token.
        version: Option<String>,
    },
    /// Generates a signed `GET /images/by-url` URL.
    Url {
        /// The remote source URL.
        url: String,
        /// An optional source version token.
        version: Option<String>,
    },
}

/// Builds a signed public transform URL for the server adapter.
///
/// The resulting URL targets either `GET /images/by-path` or `GET /images/by-url` depending on
/// `source`. `base_url` must be an absolute `http` or `https` URL that points at the externally
/// visible server origin. The helper applies the same canonical query and HMAC-SHA256 signature
/// scheme that the server adapter verifies at request time.
///
/// The helper serializes only explicitly requested transform options and omits fields that would
/// resolve to the documented defaults on the server side.
///
/// # Errors
///
/// Returns an error string when `base_url` is not an absolute `http` or `https` URL, when the
/// visible authority cannot be determined, or when the HMAC state cannot be initialized.
///
/// # Examples
///
/// ```
/// use truss::adapters::server::{sign_public_url, SignedUrlSource};
/// use truss::{MediaType, TransformOptions};
///
/// let url = sign_public_url(
///     "https://cdn.example.com",
///     SignedUrlSource::Path {
///         path: "/image.png".to_string(),
///         version: None,
///     },
///     &TransformOptions {
///         format: Some(MediaType::Jpeg),
///         ..TransformOptions::default()
///     },
///     "public-dev",
///     "secret-value",
///     4_102_444_800,
///     None,
///     None,
/// )
/// .unwrap();
///
/// assert!(url.starts_with("https://cdn.example.com/images/by-path?"));
/// assert!(url.contains("keyId=public-dev"));
/// assert!(url.contains("signature="));
/// ```
/// Optional watermark parameters for signed URL generation.
#[derive(Debug, Default)]
pub struct SignedWatermarkParams {
    pub url: String,
    pub position: Option<String>,
    pub opacity: Option<u8>,
    pub margin: Option<u32>,
}

#[allow(clippy::too_many_arguments)]
pub fn sign_public_url(
    base_url: &str,
    source: SignedUrlSource,
    options: &TransformOptions,
    key_id: &str,
    secret: &str,
    expires: u64,
    watermark: Option<&SignedWatermarkParams>,
    preset: Option<&str>,
) -> Result<String, String> {
    let base_url = Url::parse(base_url).map_err(|error| format!("base URL is invalid: {error}"))?;
    match base_url.scheme() {
        "http" | "https" => {}
        _ => return Err("base URL must use the http or https scheme".to_string()),
    }

    let route_path = match source {
        SignedUrlSource::Path { .. } => "/images/by-path",
        SignedUrlSource::Url { .. } => "/images/by-url",
    };
    let mut endpoint = base_url
        .join(route_path)
        .map_err(|error| format!("failed to resolve the public endpoint URL: {error}"))?;
    let authority = url_authority(&endpoint)?;
    let mut query = signed_source_query(source);
    if let Some(name) = preset {
        query.insert("preset".to_string(), name.to_string());
    }
    extend_transform_query(&mut query, options);
    if let Some(wm) = watermark {
        query.insert("watermarkUrl".to_string(), wm.url.clone());
        if let Some(ref pos) = wm.position {
            query.insert("watermarkPosition".to_string(), pos.clone());
        }
        if let Some(opacity) = wm.opacity {
            query.insert("watermarkOpacity".to_string(), opacity.to_string());
        }
        if let Some(margin) = wm.margin {
            query.insert("watermarkMargin".to_string(), margin.to_string());
        }
    }
    query.insert("keyId".to_string(), key_id.to_string());
    query.insert("expires".to_string(), expires.to_string());

    let canonical = format!(
        "GET\n{}\n{}\n{}",
        authority,
        endpoint.path(),
        canonical_query_without_signature(&query)
    );
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| format!("failed to initialize signed URL HMAC: {error}"))?;
    mac.update(canonical.as_bytes());
    query.insert(
        "signature".to_string(),
        hex::encode(mac.finalize().into_bytes()),
    );

    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in query {
        serializer.append_pair(&name, &value);
    }
    endpoint.set_query(Some(&serializer.finish()));
    Ok(endpoint.into())
}

/// Returns the bind address for the HTTP server adapter.
///
/// The adapter reads `TRUSS_BIND_ADDR` when it is present. Otherwise it falls back to
/// [`DEFAULT_BIND_ADDR`].
pub fn bind_addr() -> String {
    env::var("TRUSS_BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
}

/// Serves requests until the listener stops producing connections.
///
/// This helper loads [`ServerConfig`] from the process environment and then delegates to
/// [`serve_with_config`]. Health endpoints remain available even when the private API is not
/// configured, but authenticated transform requests will return `503 Service Unavailable`
/// unless `TRUSS_BEARER_TOKEN` is set.
///
/// # Errors
///
/// Returns an [`io::Error`] when the storage root cannot be resolved, when accepting the next
/// connection fails, or when a response cannot be written to the socket.
pub fn serve(listener: TcpListener) -> io::Result<()> {
    let config = ServerConfig::from_env()?;

    // Fail fast: verify the storage backend is reachable before accepting
    // connections so that configuration errors are surfaced immediately.
    for (ok, name) in storage_health_check(&config) {
        if !ok {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!(
                    "storage connectivity check failed for `{name}` — verify the backend \
                     endpoint, credentials, and container/bucket configuration"
                ),
            ));
        }
    }

    serve_with_config(listener, config)
}

/// Serves requests with an explicit server configuration.
///
/// This is the adapter entry point for tests and embedding scenarios that want deterministic
/// configuration instead of environment-variable lookup.
///
/// # Errors
///
/// Returns an [`io::Error`] when accepting the next connection fails or when a response cannot
/// be written to the socket.
pub fn serve_with_config(listener: TcpListener, config: ServerConfig) -> io::Result<()> {
    let config = Arc::new(config);
    let (sender, receiver) = std::sync::mpsc::channel::<TcpStream>();

    // Spawn a pool of worker threads sized to the configured concurrency limit
    // (with a minimum of WORKER_THREADS to leave headroom for non-transform
    // requests such as health checks and metrics).  Each thread pulls connections
    // from the shared channel and handles them independently, so a slow request
    // no longer blocks all other clients.
    let receiver = Arc::new(std::sync::Mutex::new(receiver));
    let pool_size = (config.max_concurrent_transforms as usize).max(WORKER_THREADS);
    let mut workers = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        let rx = Arc::clone(&receiver);
        let cfg = Arc::clone(&config);
        workers.push(std::thread::spawn(move || {
            loop {
                let stream = {
                    let guard = rx.lock().expect("worker lock poisoned");
                    match guard.recv() {
                        Ok(stream) => stream,
                        Err(_) => break,
                    }
                }; // MutexGuard dropped here — before handle_stream runs.
                if let Err(err) = handle_stream(stream, &cfg) {
                    cfg.log(&format!("failed to handle connection: {err}"));
                }
            }
        }));
    }

    // Install signal handler for graceful shutdown.  The handler sets the
    // shared `draining` flag (so /health/ready returns 503 immediately) and
    // writes a byte to a self-pipe to wake the accept loop.
    let (shutdown_read_fd, shutdown_write_fd) = create_shutdown_pipe()?;
    install_signal_handler(Arc::clone(&config.draining), shutdown_write_fd);

    // Set the listener to non-blocking so we can multiplex between incoming
    // connections and the shutdown pipe.
    listener.set_nonblocking(true)?;

    loop {
        // Wait for activity on the listener or shutdown pipe. On Unix we use
        // poll(2) to block efficiently; on Windows we fall back to polling the
        // draining flag with a short sleep.
        wait_for_accept_or_shutdown(&listener, shutdown_read_fd, &config.draining);

        // Check the shutdown pipe first.
        if poll_shutdown_pipe(shutdown_read_fd) {
            break;
        }

        // Also check the draining flag directly (needed on Windows where the
        // shutdown pipe is not available).
        if config.draining.load(Ordering::SeqCst) {
            break;
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                // Accepted connections are always blocking for the workers.
                let _ = stream.set_nonblocking(false);
                if sender.send(stream).is_err() {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Spurious wakeup — retry.
            }
            Err(err) => return Err(err),
        }
    }

    // --- Drain phase ---
    let drain_secs = config.shutdown_drain_secs;
    config.log(&format!(
        "shutdown: drain started, waiting {drain_secs}s for load balancers"
    ));
    if drain_secs > 0 {
        std::thread::sleep(Duration::from_secs(drain_secs));
    }
    config.log("shutdown: drain complete, closing listener");

    // Stop dispatching new connections to workers.
    drop(sender);
    // Worker drain deadline: 15s so that total shutdown (drain + worker drain)
    // fits within Kubernetes default terminationGracePeriodSeconds of 30s.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    for worker in workers {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            stderr_write("shutdown: timed out waiting for worker threads");
            break;
        }
        // Park the main thread until the worker finishes or the deadline
        // elapses. We cannot interrupt a blocked worker, but the socket
        // read/write timeouts ensure workers do not block forever.
        let worker_done =
            std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let wd = std::sync::Arc::clone(&worker_done);
        std::thread::spawn(move || {
            let _ = worker.join();
            let (lock, cvar) = &*wd;
            *lock.lock().expect("shutdown notify lock") = true;
            cvar.notify_one();
        });
        let (lock, cvar) = &*worker_done;
        let mut done = lock.lock().expect("shutdown wait lock");
        while !*done {
            let (guard, timeout) = cvar
                .wait_timeout(done, remaining)
                .expect("shutdown condvar wait");
            done = guard;
            if timeout.timed_out() {
                stderr_write("shutdown: timed out waiting for a worker thread");
                break;
            }
        }
    }

    config.log("shutdown: complete");
    close_shutdown_pipe(shutdown_read_fd, shutdown_write_fd);
    Ok(())
}

// ---------------------------------------------------------------------------
// Shutdown pipe helpers — a minimal self-pipe for waking the accept loop from
// a signal handler without requiring async I/O.
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn create_shutdown_pipe() -> io::Result<(i32, i32)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // Make both ends non-blocking: the read end so `poll_shutdown_pipe` never
    // stalls, and the write end so the signal handler never blocks.
    unsafe {
        libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK);
        libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK);
    }
    Ok((fds[0], fds[1]))
}

#[cfg(windows)]
fn create_shutdown_pipe() -> io::Result<(i32, i32)> {
    // On Windows we fall back to a polling approach using the draining flag.
    Ok((-1, -1))
}

#[cfg(unix)]
fn poll_shutdown_pipe(read_fd: i32) -> bool {
    let mut buf = [0u8; 1];
    let n = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), 1) };
    n > 0
}

#[cfg(windows)]
fn poll_shutdown_pipe(_read_fd: i32) -> bool {
    false
}

/// Block until the listener socket or the shutdown pipe has data ready.
/// On Unix this uses `poll(2)` for zero-CPU-cost waiting; on Windows it falls
/// back to a short sleep since the shutdown pipe is not available.
#[cfg(unix)]
fn wait_for_accept_or_shutdown(
    listener: &std::net::TcpListener,
    shutdown_read_fd: i32,
    _draining: &AtomicBool,
) {
    use std::os::unix::io::AsRawFd;
    let listener_fd = listener.as_raw_fd();
    let mut fds = [
        libc::pollfd {
            fd: listener_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown_read_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // Block indefinitely (-1 timeout). Signal delivery will interrupt with
    // EINTR, which is fine — we just re-check the shutdown conditions.
    unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
}

#[cfg(windows)]
fn wait_for_accept_or_shutdown(
    _listener: &std::net::TcpListener,
    _shutdown_read_fd: i32,
    draining: &AtomicBool,
) {
    // On Windows, poll(2) is not available for the listener socket. Sleep
    // briefly and let the caller check the draining flag.
    if !draining.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn close_shutdown_pipe(read_fd: i32, write_fd: i32) {
    unsafe {
        libc::close(read_fd);
        libc::close(write_fd);
    }
}

#[cfg(windows)]
fn close_shutdown_pipe(_read_fd: i32, _write_fd: i32) {}

/// Global write-end of the shutdown pipe, written to from the signal handler.
static SHUTDOWN_PIPE_WR: AtomicI32 = AtomicI32::new(-1);
/// Global draining flag set by the signal handler.
static GLOBAL_DRAINING: std::sync::atomic::AtomicPtr<AtomicBool> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(unix)]
fn install_signal_handler(draining: Arc<AtomicBool>, write_fd: i32) {
    // Store the write fd and draining pointer in globals accessible from the
    // async-signal-safe handler.
    SHUTDOWN_PIPE_WR.store(write_fd, Ordering::SeqCst);
    // SAFETY: `Arc::into_raw` leaks intentionally — the pointer remains valid
    // for the process lifetime.  The signal handler only calls `AtomicBool::store`
    // and `libc::write`, both of which are async-signal-safe.
    let ptr = Arc::into_raw(draining).cast_mut();
    GLOBAL_DRAINING.store(ptr, Ordering::SeqCst);

    // Use sigaction instead of signal to avoid SysV semantics where the handler
    // is reset to SIG_DFL after the first invocation. SA_RESTART ensures that
    // interrupted syscalls are automatically restarted.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = signal_handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        sa.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGTERM, &sa, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &sa, std::ptr::null_mut());
    }
}

#[cfg(unix)]
extern "C" fn signal_handler(_sig: libc::c_int) {
    // Set the draining flag — async-signal-safe (atomic store).
    let ptr = GLOBAL_DRAINING.load(Ordering::SeqCst);
    if !ptr.is_null() {
        unsafe { (*ptr).store(true, Ordering::SeqCst) };
    }
    // Wake the accept loop by writing to the self-pipe.
    let fd = SHUTDOWN_PIPE_WR.load(Ordering::SeqCst);
    if fd >= 0 {
        let byte: u8 = 1;
        unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

#[cfg(windows)]
fn install_signal_handler(draining: Arc<AtomicBool>, _write_fd: i32) {
    // Store the draining pointer in the global so the signal handler can set it.
    let ptr = Arc::into_raw(draining).cast_mut();
    GLOBAL_DRAINING.store(ptr, Ordering::SeqCst);

    // On Windows, register a SIGINT handler (Ctrl+C) via the C runtime.
    // The accept loop checks `draining` in the WouldBlock branch.
    unsafe {
        libc::signal(libc::SIGINT, windows_signal_handler as libc::sighandler_t);
    }
}

#[cfg(windows)]
extern "C" fn windows_signal_handler(_sig: libc::c_int) {
    let ptr = GLOBAL_DRAINING.load(Ordering::SeqCst);
    if !ptr.is_null() {
        unsafe { (*ptr).store(true, Ordering::SeqCst) };
    }
    // Re-register the handler since Windows resets to SIG_DFL after each signal.
    unsafe {
        libc::signal(libc::SIGINT, windows_signal_handler as libc::sighandler_t);
    }
}

/// Serves exactly one request using configuration loaded from the environment.
///
/// This helper is primarily useful in tests that want to drive the server over a real TCP
/// socket but do not need a long-running loop.
///
/// # Errors
///
/// Returns an [`io::Error`] when the storage root cannot be resolved, when accepting the next
/// connection fails, or when a response cannot be written to the socket.
pub fn serve_once(listener: TcpListener) -> io::Result<()> {
    let config = ServerConfig::from_env()?;
    serve_once_with_config(listener, config)
}

/// Serves exactly one request with an explicit server configuration.
///
/// # Errors
///
/// Returns an [`io::Error`] when accepting the next connection fails or when a response cannot
/// be written to the socket.
pub fn serve_once_with_config(listener: TcpListener, config: ServerConfig) -> io::Result<()> {
    let (stream, _) = listener.accept()?;
    handle_stream(stream, &config)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransformImageRequestPayload {
    source: TransformSourcePayload,
    #[serde(default)]
    options: TransformOptionsPayload,
    #[serde(default)]
    watermark: Option<WatermarkPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum TransformSourcePayload {
    Path {
        path: String,
        version: Option<String>,
    },
    Url {
        url: String,
        version: Option<String>,
    },
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    Storage {
        bucket: Option<String>,
        key: String,
        version: Option<String>,
    },
}

impl TransformSourcePayload {
    /// Computes a stable source hash from the reference and version, avoiding the
    /// need to read the full source bytes when a version tag is present. Returns
    /// `None` when no version is available, in which case the caller must fall back
    /// to the content-hash approach.
    /// Computes a stable source hash that includes the instance configuration
    /// boundaries (storage root, allow_insecure_url_sources) so that cache entries
    /// cannot be reused across instances with different security settings sharing
    /// the same cache directory.
    fn versioned_source_hash(&self, config: &ServerConfig) -> Option<String> {
        let (kind, reference, version): (&str, std::borrow::Cow<'_, str>, Option<&str>) = match self
        {
            Self::Path { path, version } => ("path", path.as_str().into(), version.as_deref()),
            Self::Url { url, version } => ("url", url.as_str().into(), version.as_deref()),
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            Self::Storage {
                bucket,
                key,
                version,
            } => {
                let (scheme, effective_bucket) =
                    storage_scheme_and_bucket(bucket.as_deref(), config);
                let effective_bucket = effective_bucket?;
                (
                    "storage",
                    format!("{scheme}://{effective_bucket}/{key}").into(),
                    version.as_deref(),
                )
            }
        };
        let version = version?;
        // Use newline separators so that values containing colons cannot collide
        // with different (reference, version) pairs. Include configuration boundaries
        // to prevent cross-instance cache poisoning.
        let mut id = String::new();
        id.push_str(kind);
        id.push('\n');
        id.push_str(&reference);
        id.push('\n');
        id.push_str(version);
        id.push('\n');
        id.push_str(config.storage_root.to_string_lossy().as_ref());
        id.push('\n');
        id.push_str(if config.allow_insecure_url_sources {
            "insecure"
        } else {
            "strict"
        });
        #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
        {
            id.push('\n');
            id.push_str(storage_backend_label(config));
            #[cfg(feature = "s3")]
            if let Some(ref ctx) = config.s3_context
                && let Some(ref endpoint) = ctx.endpoint_url
            {
                id.push('\n');
                id.push_str(endpoint);
            }
            #[cfg(feature = "gcs")]
            if let Some(ref ctx) = config.gcs_context
                && let Some(ref endpoint) = ctx.endpoint_url
            {
                id.push('\n');
                id.push_str(endpoint);
            }
            #[cfg(feature = "azure")]
            if let Some(ref ctx) = config.azure_context {
                id.push('\n');
                id.push_str(&ctx.endpoint_url);
            }
        }
        Some(hex::encode(Sha256::digest(id.as_bytes())))
    }

    /// Returns the storage backend label for metrics based on the source kind,
    /// rather than the server config default.  Path → Filesystem, Storage →
    /// whatever the config backend is, Url → None (no storage backend).
    fn metrics_backend_label(&self, _config: &ServerConfig) -> Option<StorageBackendLabel> {
        match self {
            Self::Path { .. } => Some(StorageBackendLabel::Filesystem),
            Self::Url { .. } => None,
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            Self::Storage { .. } => Some(_config.storage_backend_label()),
        }
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
fn storage_scheme_and_bucket<'a>(
    explicit_bucket: Option<&'a str>,
    config: &'a ServerConfig,
) -> (&'static str, Option<&'a str>) {
    match config.storage_backend {
        #[cfg(feature = "s3")]
        StorageBackend::S3 => {
            let bucket = explicit_bucket.or(config
                .s3_context
                .as_ref()
                .map(|ctx| ctx.default_bucket.as_str()));
            ("s3", bucket)
        }
        #[cfg(feature = "gcs")]
        StorageBackend::Gcs => {
            let bucket = explicit_bucket.or(config
                .gcs_context
                .as_ref()
                .map(|ctx| ctx.default_bucket.as_str()));
            ("gcs", bucket)
        }
        StorageBackend::Filesystem => ("fs", explicit_bucket),
        #[cfg(feature = "azure")]
        StorageBackend::Azure => {
            let bucket = explicit_bucket.or(config
                .azure_context
                .as_ref()
                .map(|ctx| ctx.default_container.as_str()));
            ("azure", bucket)
        }
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
fn is_object_storage_backend(config: &ServerConfig) -> bool {
    match config.storage_backend {
        StorageBackend::Filesystem => false,
        #[cfg(feature = "s3")]
        StorageBackend::S3 => true,
        #[cfg(feature = "gcs")]
        StorageBackend::Gcs => true,
        #[cfg(feature = "azure")]
        StorageBackend::Azure => true,
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
fn storage_backend_label(config: &ServerConfig) -> &'static str {
    match config.storage_backend {
        StorageBackend::Filesystem => "fs-backend",
        #[cfg(feature = "s3")]
        StorageBackend::S3 => "s3-backend",
        #[cfg(feature = "gcs")]
        StorageBackend::Gcs => "gcs-backend",
        #[cfg(feature = "azure")]
        StorageBackend::Azure => "azure-backend",
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformOptionsPayload {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: Option<String>,
    pub position: Option<String>,
    pub format: Option<String>,
    pub quality: Option<u8>,
    pub background: Option<String>,
    pub rotate: Option<u16>,
    pub auto_orient: Option<bool>,
    pub strip_metadata: Option<bool>,
    pub preserve_exif: Option<bool>,
    pub crop: Option<String>,
    pub blur: Option<f32>,
    pub sharpen: Option<f32>,
}

impl TransformOptionsPayload {
    /// Merges per-request overrides on top of preset defaults.
    /// Each field in `overrides` takes precedence when set (`Some`).
    fn with_overrides(self, overrides: &TransformOptionsPayload) -> Self {
        Self {
            width: overrides.width.or(self.width),
            height: overrides.height.or(self.height),
            fit: overrides.fit.clone().or(self.fit),
            position: overrides.position.clone().or(self.position),
            format: overrides.format.clone().or(self.format),
            quality: overrides.quality.or(self.quality),
            background: overrides.background.clone().or(self.background),
            rotate: overrides.rotate.or(self.rotate),
            auto_orient: overrides.auto_orient.or(self.auto_orient),
            strip_metadata: overrides.strip_metadata.or(self.strip_metadata),
            preserve_exif: overrides.preserve_exif.or(self.preserve_exif),
            crop: overrides.crop.clone().or(self.crop),
            blur: overrides.blur.or(self.blur),
            sharpen: overrides.sharpen.or(self.sharpen),
        }
    }

    fn into_options(self) -> Result<TransformOptions, HttpResponse> {
        let defaults = TransformOptions::default();

        Ok(TransformOptions {
            width: self.width,
            height: self.height,
            fit: parse_optional_named(self.fit.as_deref(), "fit", Fit::from_str)?,
            position: parse_optional_named(
                self.position.as_deref(),
                "position",
                Position::from_str,
            )?,
            format: parse_optional_named(self.format.as_deref(), "format", MediaType::from_str)?,
            quality: self.quality,
            background: parse_optional_named(
                self.background.as_deref(),
                "background",
                Rgba8::from_hex,
            )?,
            rotate: match self.rotate {
                Some(value) => parse_named(&value.to_string(), "rotate", Rotation::from_str)?,
                None => defaults.rotate,
            },
            auto_orient: self.auto_orient.unwrap_or(defaults.auto_orient),
            strip_metadata: self.strip_metadata.unwrap_or(defaults.strip_metadata),
            preserve_exif: self.preserve_exif.unwrap_or(defaults.preserve_exif),
            crop: parse_optional_named(self.crop.as_deref(), "crop", CropRegion::from_str)?,
            blur: self.blur,
            sharpen: self.sharpen,
            deadline: defaults.deadline,
        })
    }
}

/// Overall request deadline for outbound fetches (source + watermark combined).
const REQUEST_DEADLINE_SECS: u64 = 60;

const WATERMARK_DEFAULT_POSITION: Position = Position::BottomRight;
const WATERMARK_DEFAULT_OPACITY: u8 = 50;
const WATERMARK_DEFAULT_MARGIN: u32 = 10;
const WATERMARK_MAX_MARGIN: u32 = 9999;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct WatermarkPayload {
    url: Option<String>,
    position: Option<String>,
    opacity: Option<u8>,
    margin: Option<u32>,
}

/// Validated watermark parameters ready for fetching. No network I/O performed.
struct ValidatedWatermarkPayload {
    url: String,
    position: Position,
    opacity: u8,
    margin: u32,
}

impl ValidatedWatermarkPayload {
    fn cache_identity(&self) -> String {
        compute_watermark_identity(
            &self.url,
            self.position.as_name(),
            self.opacity,
            self.margin,
        )
    }
}

/// Validates watermark payload fields without performing network I/O.
fn validate_watermark_payload(
    payload: Option<&WatermarkPayload>,
) -> Result<Option<ValidatedWatermarkPayload>, HttpResponse> {
    let Some(wm) = payload else {
        return Ok(None);
    };
    let url = wm.url.as_deref().filter(|u| !u.is_empty()).ok_or_else(|| {
        bad_request_response("watermark.url is required when watermark is present")
    })?;

    let position = parse_optional_named(
        wm.position.as_deref(),
        "watermark.position",
        Position::from_str,
    )?
    .unwrap_or(WATERMARK_DEFAULT_POSITION);

    let opacity = wm.opacity.unwrap_or(WATERMARK_DEFAULT_OPACITY);
    if opacity == 0 || opacity > 100 {
        return Err(bad_request_response(
            "watermark.opacity must be between 1 and 100",
        ));
    }
    let margin = wm.margin.unwrap_or(WATERMARK_DEFAULT_MARGIN);
    if margin > WATERMARK_MAX_MARGIN {
        return Err(bad_request_response(
            "watermark.margin must be at most 9999",
        ));
    }

    Ok(Some(ValidatedWatermarkPayload {
        url: url.to_string(),
        position,
        opacity,
        margin,
    }))
}

/// Fetches watermark image and builds WatermarkInput. Called after try_acquire.
fn fetch_watermark(
    validated: ValidatedWatermarkPayload,
    config: &ServerConfig,
    deadline: Option<Instant>,
) -> Result<WatermarkInput, HttpResponse> {
    let bytes = read_remote_watermark_bytes(&validated.url, config, deadline)?;
    let artifact = sniff_artifact(RawArtifact::new(bytes, None))
        .map_err(|error| bad_request_response(&format!("watermark image is invalid: {error}")))?;
    if !artifact.media_type.is_raster() {
        return Err(bad_request_response(
            "watermark image must be a raster format (not SVG)",
        ));
    }
    Ok(WatermarkInput {
        image: artifact,
        position: validated.position,
        opacity: validated.opacity,
        margin: validated.margin,
    })
}

fn resolve_multipart_watermark(
    bytes: Vec<u8>,
    position: Option<String>,
    opacity: Option<u8>,
    margin: Option<u32>,
) -> Result<WatermarkInput, HttpResponse> {
    let artifact = sniff_artifact(RawArtifact::new(bytes, None))
        .map_err(|error| bad_request_response(&format!("watermark image is invalid: {error}")))?;
    if !artifact.media_type.is_raster() {
        return Err(bad_request_response(
            "watermark image must be a raster format (not SVG)",
        ));
    }
    let position = parse_optional_named(
        position.as_deref(),
        "watermark_position",
        Position::from_str,
    )?
    .unwrap_or(WATERMARK_DEFAULT_POSITION);
    let opacity = opacity.unwrap_or(WATERMARK_DEFAULT_OPACITY);
    if opacity == 0 || opacity > 100 {
        return Err(bad_request_response(
            "watermark_opacity must be between 1 and 100",
        ));
    }
    let margin = margin.unwrap_or(WATERMARK_DEFAULT_MARGIN);
    if margin > WATERMARK_MAX_MARGIN {
        return Err(bad_request_response(
            "watermark_margin must be at most 9999",
        ));
    }
    Ok(WatermarkInput {
        image: artifact,
        position,
        opacity,
        margin,
    })
}

struct AccessLogEntry<'a> {
    request_id: &'a str,
    method: &'a str,
    path: &'a str,
    route: &'a str,
    status: &'a str,
    start: Instant,
    cache_status: Option<&'a str>,
    watermark: bool,
}

/// Extracts the `X-Request-Id` header value from request headers.
/// Returns `None` if the header is absent, empty, or contains
/// characters unsafe for HTTP headers (CR, LF, NUL).
fn extract_request_id(headers: &[(String, String)]) -> Option<String> {
    headers.iter().find_map(|(name, value)| {
        if name != "x-request-id" || value.is_empty() {
            return None;
        }
        if value
            .bytes()
            .any(|b| b == b'\r' || b == b'\n' || b == b'\0')
        {
            return None;
        }
        Some(value.clone())
    })
}

/// Classifies the `Cache-Status` response header as `"hit"` or `"miss"`.
/// Returns `None` when the header is absent.
fn extract_cache_status(headers: &[(String, String)]) -> Option<&'static str> {
    headers
        .iter()
        .find_map(|(name, value)| (name == "Cache-Status").then_some(value.as_str()))
        .map(|v| if v.contains("hit") { "hit" } else { "miss" })
}

/// Extracts and removes the internal `X-Truss-Watermark` header, returning whether it was set.
fn extract_watermark_flag(headers: &mut Vec<(String, String)>) -> bool {
    let pos = headers
        .iter()
        .position(|(name, _)| name == "X-Truss-Watermark");
    if let Some(idx) = pos {
        headers.swap_remove(idx);
        true
    } else {
        false
    }
}

fn emit_access_log(config: &ServerConfig, entry: &AccessLogEntry<'_>) {
    config.log(
        &json!({
            "kind": "access_log",
            "request_id": entry.request_id,
            "method": entry.method,
            "path": entry.path,
            "route": entry.route,
            "status": entry.status,
            "latency_ms": entry.start.elapsed().as_millis() as u64,
            "cache_status": entry.cache_status,
            "watermark": entry.watermark,
        })
        .to_string(),
    );
}

fn handle_stream(mut stream: TcpStream, config: &ServerConfig) -> io::Result<()> {
    // Prevent slow or stalled clients from blocking the accept loop indefinitely.
    if let Err(err) = stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT)) {
        config.log(&format!("failed to set socket read timeout: {err}"));
    }
    if let Err(err) = stream.set_write_timeout(Some(SOCKET_WRITE_TIMEOUT)) {
        config.log(&format!("failed to set socket write timeout: {err}"));
    }

    let mut requests_served: u64 = 0;

    loop {
        let partial = match read_request_headers(&mut stream, config.max_upload_bytes) {
            Ok(partial) => partial,
            Err(response) => {
                if requests_served > 0 {
                    return Ok(());
                }
                let _ = write_response(&mut stream, response, true);
                return Ok(());
            }
        };

        // Start timing after headers are read so latency reflects server
        // processing time, not client send / socket-wait time.
        let start = Instant::now();

        let request_id =
            extract_request_id(&partial.headers).unwrap_or_else(|| Uuid::new_v4().to_string());

        let client_wants_close = partial
            .headers
            .iter()
            .any(|(name, value)| name == "connection" && value.eq_ignore_ascii_case("close"));

        let accepts_gzip = config.enable_compression
            && http_parse::header_value(&partial.headers, "accept-encoding")
                .is_some_and(|v| http_parse::accepts_encoding(v, "gzip"));

        let is_head = partial.method == "HEAD";

        let requires_auth = matches!(
            (partial.method.as_str(), partial.path()),
            ("POST", "/images:transform") | ("POST", "/images")
        );
        if requires_auth
            && let Err(mut response) = authorize_request_headers(&partial.headers, config)
        {
            response
                .headers
                .push(("X-Request-Id".to_string(), request_id.clone()));
            record_http_metrics(RouteMetric::Unknown, response.status);
            let sc = status_code(response.status).unwrap_or("unknown");
            let method_log = partial.method.clone();
            let path_log = partial.path().to_string();
            let _ = write_response(&mut stream, response, true);
            record_http_request_duration(RouteMetric::Unknown, start);
            emit_access_log(
                config,
                &AccessLogEntry {
                    request_id: &request_id,
                    method: &method_log,
                    path: &path_log,
                    route: &path_log,
                    status: sc,
                    start,
                    cache_status: None,
                    watermark: false,
                },
            );
            return Ok(());
        }

        // Early-reject /metrics requests before draining the body so that
        // unauthenticated or disabled-metrics requests do not force a body read.
        if matches!(
            (partial.method.as_str(), partial.path()),
            ("GET" | "HEAD", "/metrics")
        ) {
            let early_response = if config.disable_metrics {
                Some(HttpResponse::problem(
                    "404 Not Found",
                    NOT_FOUND_BODY.as_bytes().to_vec(),
                ))
            } else if let Some(expected) = &config.metrics_token {
                let provided = http_parse::header_value(&partial.headers, "authorization")
                    .and_then(|value| {
                        let (scheme, token) = value.split_once(|c: char| c.is_whitespace())?;
                        scheme.eq_ignore_ascii_case("Bearer").then(|| token.trim())
                    });
                match provided {
                    Some(token) if token.as_bytes().ct_eq(expected.as_bytes()).into() => None,
                    _ => Some(response::auth_required_response(
                        "metrics endpoint requires authentication",
                    )),
                }
            } else {
                None
            };

            if let Some(mut response) = early_response {
                response
                    .headers
                    .push(("X-Request-Id".to_string(), request_id.clone()));
                record_http_metrics(RouteMetric::Metrics, response.status);
                let sc = status_code(response.status).unwrap_or("unknown");
                let method_log = partial.method.clone();
                let path_log = partial.path().to_string();
                let _ = write_response(&mut stream, response, true);
                record_http_request_duration(RouteMetric::Metrics, start);
                emit_access_log(
                    config,
                    &AccessLogEntry {
                        request_id: &request_id,
                        method: &method_log,
                        path: &path_log,
                        route: "/metrics",
                        status: sc,
                        start,
                        cache_status: None,
                        watermark: false,
                    },
                );
                return Ok(());
            }
        }

        // Clone method/path before `read_request_body` consumes `partial`.
        let method = partial.method.clone();
        let path = partial.path().to_string();

        let request = match read_request_body(&mut stream, partial) {
            Ok(request) => request,
            Err(mut response) => {
                response
                    .headers
                    .push(("X-Request-Id".to_string(), request_id.clone()));
                record_http_metrics(RouteMetric::Unknown, response.status);
                let sc = status_code(response.status).unwrap_or("unknown");
                let _ = write_response(&mut stream, response, true);
                record_http_request_duration(RouteMetric::Unknown, start);
                emit_access_log(
                    config,
                    &AccessLogEntry {
                        request_id: &request_id,
                        method: &method,
                        path: &path,
                        route: &path,
                        status: sc,
                        start,
                        cache_status: None,
                        watermark: false,
                    },
                );
                return Ok(());
            }
        };
        let route = classify_route(&request);
        let mut response = route_request(request, config);
        record_http_metrics(route, response.status);

        response
            .headers
            .push(("X-Request-Id".to_string(), request_id.clone()));

        let cache_status = extract_cache_status(&response.headers);
        let had_watermark = extract_watermark_flag(&mut response.headers);

        let sc = status_code(response.status).unwrap_or("unknown");

        if is_head {
            response.body = Vec::new();
        }

        requests_served += 1;
        let close_after = client_wants_close || requests_served >= config.keep_alive_max_requests;

        write_response_compressed(
            &mut stream,
            response,
            close_after,
            accepts_gzip,
            config.compression_level,
        )?;
        record_http_request_duration(route, start);

        emit_access_log(
            config,
            &AccessLogEntry {
                request_id: &request_id,
                method: &method,
                path: &path,
                route: route.as_label(),
                status: sc,
                start,
                cache_status,
                watermark: had_watermark,
            },
        );

        if close_after {
            return Ok(());
        }
    }
}

fn route_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    let method = request.method.clone();
    let path = request.path().to_string();

    match (method.as_str(), path.as_str()) {
        ("GET" | "HEAD", "/health") => handle_health(config),
        ("GET" | "HEAD", "/health/live") => handle_health_live(),
        ("GET" | "HEAD", "/health/ready") => handle_health_ready(config),
        ("GET" | "HEAD", "/images/by-path") => handle_public_path_request(request, config),
        ("GET" | "HEAD", "/images/by-url") => handle_public_url_request(request, config),
        ("POST", "/images:transform") => handle_transform_request(request, config),
        ("POST", "/images") => handle_upload_request(request, config),
        ("GET" | "HEAD", "/metrics") => handle_metrics_request(request, config),
        _ => HttpResponse::problem("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec()),
    }
}

fn classify_route(request: &HttpRequest) -> RouteMetric {
    match (request.method.as_str(), request.path()) {
        ("GET" | "HEAD", "/health") => RouteMetric::Health,
        ("GET" | "HEAD", "/health/live") => RouteMetric::HealthLive,
        ("GET" | "HEAD", "/health/ready") => RouteMetric::HealthReady,
        ("GET" | "HEAD", "/images/by-path") => RouteMetric::PublicByPath,
        ("GET" | "HEAD", "/images/by-url") => RouteMetric::PublicByUrl,
        ("POST", "/images:transform") => RouteMetric::Transform,
        ("POST", "/images") => RouteMetric::Upload,
        ("GET" | "HEAD", "/metrics") => RouteMetric::Metrics,
        _ => RouteMetric::Unknown,
    }
}

fn handle_transform_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    let request_deadline = Some(Instant::now() + Duration::from_secs(REQUEST_DEADLINE_SECS));

    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    if !request_has_json_content_type(&request) {
        return unsupported_media_type_response("content-type must be application/json");
    }

    let payload: TransformImageRequestPayload = match serde_json::from_slice(&request.body) {
        Ok(payload) => payload,
        Err(error) => {
            return bad_request_response(&format!("request body must be valid JSON: {error}"));
        }
    };
    let options = match payload.options.into_options() {
        Ok(options) => options,
        Err(response) => return response,
    };

    let versioned_hash = payload.source.versioned_source_hash(config);
    let validated_wm = match validate_watermark_payload(payload.watermark.as_ref()) {
        Ok(wm) => wm,
        Err(response) => return response,
    };
    let watermark_id = validated_wm.as_ref().map(|v| v.cache_identity());

    if let Some(response) = try_versioned_cache_lookup(
        versioned_hash.as_deref(),
        &options,
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
        watermark_id.as_deref(),
    ) {
        return response;
    }

    let storage_start = Instant::now();
    let backend_label = payload.source.metrics_backend_label(config);
    let backend_idx = backend_label.map(|l| storage_backend_index_from_config(&l));
    let source_bytes = match resolve_source_bytes(payload.source, config, request_deadline) {
        Ok(bytes) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            bytes
        }
        Err(response) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            return response;
        }
    };
    transform_source_bytes(
        source_bytes,
        options,
        versioned_hash.as_deref(),
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
        WatermarkSource::from_validated(validated_wm),
        watermark_id.as_deref(),
        request_deadline,
    )
}

fn handle_public_path_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    handle_public_get_request(request, config, PublicSourceKind::Path)
}

fn handle_public_url_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    handle_public_get_request(request, config, PublicSourceKind::Url)
}

fn handle_public_get_request(
    request: HttpRequest,
    config: &ServerConfig,
    source_kind: PublicSourceKind,
) -> HttpResponse {
    let request_deadline = Some(Instant::now() + Duration::from_secs(REQUEST_DEADLINE_SECS));
    let query = match parse_query_params(&request) {
        Ok(query) => query,
        Err(response) => return response,
    };
    if let Err(response) = authorize_signed_request(&request, &query, config) {
        return response;
    }
    let (source, options, watermark_payload) =
        match parse_public_get_request(&query, source_kind, config) {
            Ok(parsed) => parsed,
            Err(response) => return response,
        };

    let validated_wm = match validate_watermark_payload(watermark_payload.as_ref()) {
        Ok(wm) => wm,
        Err(response) => return response,
    };
    let watermark_id = validated_wm.as_ref().map(|v| v.cache_identity());

    // When the storage backend is object storage (S3 or GCS), convert Path
    // sources to Storage sources so that the `path` query parameter is
    // resolved as an object key.
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    let source = if is_object_storage_backend(config) {
        match source {
            TransformSourcePayload::Path { path, version } => TransformSourcePayload::Storage {
                bucket: None,
                key: path.trim_start_matches('/').to_string(),
                version,
            },
            other => other,
        }
    } else {
        source
    };

    let versioned_hash = source.versioned_source_hash(config);
    if let Some(response) = try_versioned_cache_lookup(
        versioned_hash.as_deref(),
        &options,
        &request,
        ImageResponsePolicy::PublicGet,
        config,
        watermark_id.as_deref(),
    ) {
        return response;
    }

    let storage_start = Instant::now();
    let backend_label = source.metrics_backend_label(config);
    let backend_idx = backend_label.map(|l| storage_backend_index_from_config(&l));
    let source_bytes = match resolve_source_bytes(source, config, request_deadline) {
        Ok(bytes) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            bytes
        }
        Err(response) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            return response;
        }
    };

    transform_source_bytes(
        source_bytes,
        options,
        versioned_hash.as_deref(),
        &request,
        ImageResponsePolicy::PublicGet,
        config,
        WatermarkSource::from_validated(validated_wm),
        watermark_id.as_deref(),
        request_deadline,
    )
}

fn handle_upload_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    let boundary = match parse_multipart_boundary(&request) {
        Ok(boundary) => boundary,
        Err(response) => return response,
    };
    let (file_bytes, options, watermark) = match parse_upload_request(&request.body, &boundary) {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    let watermark_identity = watermark.as_ref().map(|wm| {
        let content_hash = hex::encode(sha2::Sha256::digest(&wm.image.bytes));
        cache::compute_watermark_content_identity(
            &content_hash,
            wm.position.as_name(),
            wm.opacity,
            wm.margin,
        )
    });
    transform_source_bytes(
        file_bytes,
        options,
        None,
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
        WatermarkSource::from_ready(watermark),
        watermark_identity.as_deref(),
        None,
    )
}

/// Returns the number of free bytes on the filesystem containing `path`,
/// or `None` if the query fails.
#[cfg(target_os = "linux")]
fn disk_free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::ffi::CString;

    let c_path = CString::new(path.to_str()?).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret == 0 {
        stat.f_bavail.checked_mul(stat.f_frsize)
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
fn disk_free_bytes(_path: &std::path::Path) -> Option<u64> {
    None
}

/// Returns the current process RSS (Resident Set Size) in bytes by reading
/// `/proc/self/status`. Returns `None` on non-Linux platforms or on read failure.
#[cfg(target_os = "linux")]
fn process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            let value = value.trim();
            // Format: "123456 kB"
            let kb_str = value.strip_suffix(" kB")?.trim();
            let kb: u64 = kb_str.parse().ok()?;
            return kb.checked_mul(1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn process_rss_bytes() -> Option<u64> {
    None
}

/// Returns a minimal liveness response confirming the process is running.
fn handle_health_live() -> HttpResponse {
    let body = serde_json::to_vec(&json!({
        "status": "ok",
        "service": "truss",
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .expect("serialize liveness");
    let mut body = body;
    body.push(b'\n');
    HttpResponse::json("200 OK", body)
}

/// Returns a readiness response after checking that critical infrastructure
/// dependencies are available (storage root, cache root if configured, S3
/// reachability) and configurable resource thresholds.
fn handle_health_ready(config: &ServerConfig) -> HttpResponse {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut all_ok = true;

    // When the server is draining (shutdown signal received), immediately
    // report not-ready so that load balancers stop routing traffic.
    if config.draining.load(Ordering::Relaxed) {
        checks.push(json!({
            "name": "draining",
            "status": "fail",
        }));
        all_ok = false;
    }

    for (ok, name) in storage_health_check(config) {
        checks.push(json!({
            "name": name,
            "status": if ok { "ok" } else { "fail" },
        }));
        if !ok {
            all_ok = false;
        }
    }

    if let Some(cache_root) = &config.cache_root {
        let cache_ok = cache_root.is_dir();
        checks.push(json!({
            "name": "cacheRoot",
            "status": if cache_ok { "ok" } else { "fail" },
        }));
        if !cache_ok {
            all_ok = false;
        }
    }

    if let Some(cache_root) = &config.cache_root {
        let free = disk_free_bytes(cache_root);
        let threshold = config.health_cache_min_free_bytes;
        let disk_ok = match (free, threshold) {
            (Some(f), Some(min)) => f >= min,
            _ => true,
        };
        let mut check = json!({
            "name": "cacheDiskFree",
            "status": if disk_ok { "ok" } else { "fail" },
        });
        if let Some(f) = free {
            check["freeBytes"] = json!(f);
        }
        if let Some(min) = threshold {
            check["thresholdBytes"] = json!(min);
        }
        checks.push(check);
        if !disk_ok {
            all_ok = false;
        }
    }

    // Concurrency utilization
    let in_flight = config.transforms_in_flight.load(Ordering::Relaxed);
    let overloaded = in_flight >= config.max_concurrent_transforms;
    checks.push(json!({
        "name": "transformCapacity",
        "status": if overloaded { "fail" } else { "ok" },
        "current": in_flight,
        "max": config.max_concurrent_transforms,
    }));
    if overloaded {
        all_ok = false;
    }

    // Memory usage (Linux only) — skip entirely when RSS is unavailable
    if let Some(rss_bytes) = process_rss_bytes() {
        let threshold = config.health_max_memory_bytes;
        let mem_ok = threshold.is_none_or(|max| rss_bytes <= max);
        let mut check = json!({
            "name": "memoryUsage",
            "status": if mem_ok { "ok" } else { "fail" },
            "rssBytes": rss_bytes,
        });
        if let Some(max) = threshold {
            check["thresholdBytes"] = json!(max);
        }
        checks.push(check);
        if !mem_ok {
            all_ok = false;
        }
    }

    let status_str = if all_ok { "ok" } else { "fail" };
    let mut body = serde_json::to_vec(&json!({
        "status": status_str,
        "checks": checks,
    }))
    .expect("serialize readiness");
    body.push(b'\n');

    if all_ok {
        HttpResponse::json("200 OK", body)
    } else {
        HttpResponse::json("503 Service Unavailable", body)
    }
}

/// Returns a comprehensive diagnostic health response.
fn storage_health_check(config: &ServerConfig) -> Vec<(bool, &'static str)> {
    #[allow(unused_mut)]
    let mut checks = vec![(config.storage_root.is_dir(), "storageRoot")];
    #[cfg(feature = "s3")]
    if config.storage_backend == StorageBackend::S3 {
        let reachable = config
            .s3_context
            .as_ref()
            .is_some_and(|ctx| ctx.check_reachable());
        checks.push((reachable, "storageBackend"));
    }
    #[cfg(feature = "gcs")]
    if config.storage_backend == StorageBackend::Gcs {
        let reachable = config
            .gcs_context
            .as_ref()
            .is_some_and(|ctx| ctx.check_reachable());
        checks.push((reachable, "storageBackend"));
    }
    #[cfg(feature = "azure")]
    if config.storage_backend == StorageBackend::Azure {
        let reachable = config
            .azure_context
            .as_ref()
            .is_some_and(|ctx| ctx.check_reachable());
        checks.push((reachable, "storageBackend"));
    }
    checks
}

fn handle_health(config: &ServerConfig) -> HttpResponse {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut all_ok = true;

    for (ok, name) in storage_health_check(config) {
        checks.push(json!({
            "name": name,
            "status": if ok { "ok" } else { "fail" },
        }));
        if !ok {
            all_ok = false;
        }
    }

    if let Some(cache_root) = &config.cache_root {
        let cache_ok = cache_root.is_dir();
        checks.push(json!({
            "name": "cacheRoot",
            "status": if cache_ok { "ok" } else { "fail" },
        }));
        if !cache_ok {
            all_ok = false;
        }
    }

    // Cache disk free space
    if let Some(cache_root) = &config.cache_root {
        let free = disk_free_bytes(cache_root);
        let threshold = config.health_cache_min_free_bytes;
        let disk_ok = match (free, threshold) {
            (Some(f), Some(min)) => f >= min,
            _ => true,
        };
        let mut check = json!({
            "name": "cacheDiskFree",
            "status": if disk_ok { "ok" } else { "fail" },
        });
        if let Some(f) = free {
            check["freeBytes"] = json!(f);
        }
        if let Some(min) = threshold {
            check["thresholdBytes"] = json!(min);
        }
        checks.push(check);
        if !disk_ok {
            all_ok = false;
        }
    }

    // Concurrency utilization
    let in_flight = config.transforms_in_flight.load(Ordering::Relaxed);
    let overloaded = in_flight >= config.max_concurrent_transforms;
    checks.push(json!({
        "name": "transformCapacity",
        "status": if overloaded { "fail" } else { "ok" },
        "current": in_flight,
        "max": config.max_concurrent_transforms,
    }));
    if overloaded {
        all_ok = false;
    }

    // Memory usage (Linux only)
    let rss = process_rss_bytes();
    if let Some(rss_bytes) = rss {
        let threshold = config.health_max_memory_bytes;
        let mem_ok = threshold.is_none_or(|max| rss_bytes <= max);
        let mut check = json!({
            "name": "memoryUsage",
            "status": if mem_ok { "ok" } else { "fail" },
            "rssBytes": rss_bytes,
        });
        if let Some(max) = threshold {
            check["thresholdBytes"] = json!(max);
        }
        checks.push(check);
        if !mem_ok {
            all_ok = false;
        }
    }

    let status_str = if all_ok { "ok" } else { "fail" };
    let mut body = serde_json::to_vec(&json!({
        "status": status_str,
        "service": "truss",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": uptime_seconds(),
        "checks": checks,
        "maxInputPixels": config.max_input_pixels,
    }))
    .expect("serialize health");
    body.push(b'\n');

    HttpResponse::json("200 OK", body)
}

fn handle_metrics_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if config.disable_metrics {
        return HttpResponse::problem("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec());
    }

    if let Some(expected) = &config.metrics_token {
        let provided = request.header("authorization").and_then(|value| {
            let (scheme, token) = value.split_once(|c: char| c.is_whitespace())?;
            scheme.eq_ignore_ascii_case("Bearer").then(|| token.trim())
        });
        match provided {
            Some(token) if token.as_bytes().ct_eq(expected.as_bytes()).into() => {}
            _ => {
                return response::auth_required_response(
                    "metrics endpoint requires authentication",
                );
            }
        }
    }

    HttpResponse::text(
        "200 OK",
        "text/plain; version=0.0.4; charset=utf-8",
        render_metrics_text(
            config.max_concurrent_transforms,
            &config.transforms_in_flight,
        )
        .into_bytes(),
    )
}

fn parse_public_get_request(
    query: &BTreeMap<String, String>,
    source_kind: PublicSourceKind,
    config: &ServerConfig,
) -> Result<
    (
        TransformSourcePayload,
        TransformOptions,
        Option<WatermarkPayload>,
    ),
    HttpResponse,
> {
    validate_public_query_names(query, source_kind)?;

    let source = match source_kind {
        PublicSourceKind::Path => TransformSourcePayload::Path {
            path: required_query_param(query, "path")?.to_string(),
            version: query.get("version").cloned(),
        },
        PublicSourceKind::Url => TransformSourcePayload::Url {
            url: required_query_param(query, "url")?.to_string(),
            version: query.get("version").cloned(),
        },
    };

    let has_orphaned_watermark_params = query.contains_key("watermarkPosition")
        || query.contains_key("watermarkOpacity")
        || query.contains_key("watermarkMargin");
    let watermark = if query.contains_key("watermarkUrl") {
        Some(WatermarkPayload {
            url: query.get("watermarkUrl").cloned(),
            position: query.get("watermarkPosition").cloned(),
            opacity: parse_optional_u8_query(query, "watermarkOpacity")?,
            margin: parse_optional_integer_query(query, "watermarkMargin")?,
        })
    } else if has_orphaned_watermark_params {
        return Err(bad_request_response(
            "watermarkPosition, watermarkOpacity, and watermarkMargin require watermarkUrl",
        ));
    } else {
        None
    };

    // Build per-request overrides from query parameters.
    let per_request = TransformOptionsPayload {
        width: parse_optional_integer_query(query, "width")?,
        height: parse_optional_integer_query(query, "height")?,
        fit: query.get("fit").cloned(),
        position: query.get("position").cloned(),
        format: query.get("format").cloned(),
        quality: parse_optional_u8_query(query, "quality")?,
        background: query.get("background").cloned(),
        rotate: query
            .get("rotate")
            .map(|v| v.parse::<u16>())
            .transpose()
            .map_err(|_| bad_request_response("rotate must be 0, 90, 180, or 270"))?,
        auto_orient: parse_optional_bool_query(query, "autoOrient")?,
        strip_metadata: parse_optional_bool_query(query, "stripMetadata")?,
        preserve_exif: parse_optional_bool_query(query, "preserveExif")?,
        crop: query.get("crop").cloned(),
        blur: parse_optional_float_query(query, "blur")?,
        sharpen: parse_optional_float_query(query, "sharpen")?,
    };

    // Resolve preset and merge with per-request overrides.
    let merged = if let Some(preset_name) = query.get("preset") {
        let preset = config
            .presets
            .get(preset_name)
            .ok_or_else(|| bad_request_response(&format!("unknown preset `{preset_name}`")))?;
        preset.clone().with_overrides(&per_request)
    } else {
        per_request
    };

    let options = merged.into_options()?;

    Ok((source, options, watermark))
}

/// Watermark source: either already resolved (multipart upload) or deferred (URL fetch).
enum WatermarkSource {
    Deferred(ValidatedWatermarkPayload),
    Ready(WatermarkInput),
    None,
}

impl WatermarkSource {
    fn from_validated(validated: Option<ValidatedWatermarkPayload>) -> Self {
        match validated {
            Some(v) => Self::Deferred(v),
            None => Self::None,
        }
    }

    fn from_ready(input: Option<WatermarkInput>) -> Self {
        match input {
            Some(w) => Self::Ready(w),
            None => Self::None,
        }
    }

    fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[allow(clippy::too_many_arguments)]
fn transform_source_bytes(
    source_bytes: Vec<u8>,
    options: TransformOptions,
    versioned_hash: Option<&str>,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    config: &ServerConfig,
    watermark: WatermarkSource,
    watermark_identity: Option<&str>,
    request_deadline: Option<Instant>,
) -> HttpResponse {
    let content_hash;
    let source_hash = match versioned_hash {
        Some(hash) => hash,
        None => {
            content_hash = hex::encode(Sha256::digest(&source_bytes));
            &content_hash
        }
    };

    let cache = config
        .cache_root
        .as_ref()
        .map(|root| TransformCache::new(root.clone()).with_log_handler(config.log_handler.clone()));

    if let Some(ref cache) = cache
        && options.format.is_some()
    {
        let cache_key = compute_cache_key(source_hash, &options, None, watermark_identity);
        if let CacheLookup::Hit {
            media_type,
            body,
            age,
        } = cache.get(&cache_key)
        {
            CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
            let etag = build_image_etag(&body);
            let mut headers = build_image_response_headers(
                media_type,
                &etag,
                response_policy,
                false,
                CacheHitStatus::Hit,
                config.public_max_age_seconds,
                config.public_stale_while_revalidate_seconds,
                &config.custom_response_headers,
            );
            headers.push(("Age".to_string(), age.as_secs().to_string()));
            if matches!(response_policy, ImageResponsePolicy::PublicGet)
                && if_none_match_matches(request.header("if-none-match"), &etag)
            {
                return HttpResponse::empty("304 Not Modified", headers);
            }
            return HttpResponse::binary_with_headers(
                "200 OK",
                media_type.as_mime(),
                headers,
                body,
            );
        }
    }

    let _slot = match TransformSlot::try_acquire(
        &config.transforms_in_flight,
        config.max_concurrent_transforms,
    ) {
        Some(slot) => slot,
        None => return service_unavailable_response("too many concurrent transforms; retry later"),
    };
    transform_source_bytes_inner(
        source_bytes,
        options,
        request,
        response_policy,
        cache.as_ref(),
        source_hash,
        ImageResponseConfig {
            disable_accept_negotiation: config.disable_accept_negotiation,
            public_cache_control: PublicCacheControl {
                max_age: config.public_max_age_seconds,
                stale_while_revalidate: config.public_stale_while_revalidate_seconds,
            },
            transform_deadline: Duration::from_secs(config.transform_deadline_secs),
        },
        watermark,
        watermark_identity,
        config,
        request_deadline,
    )
}

#[allow(clippy::too_many_arguments)]
fn transform_source_bytes_inner(
    source_bytes: Vec<u8>,
    mut options: TransformOptions,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    cache: Option<&TransformCache>,
    source_hash: &str,
    response_config: ImageResponseConfig,
    watermark_source: WatermarkSource,
    watermark_identity: Option<&str>,
    config: &ServerConfig,
    request_deadline: Option<Instant>,
) -> HttpResponse {
    if options.deadline.is_none() {
        options.deadline = Some(response_config.transform_deadline);
    }
    let artifact = match sniff_artifact(RawArtifact::new(source_bytes, None)) {
        Ok(artifact) => artifact,
        Err(error) => {
            record_transform_error(&error);
            return transform_error_response(error);
        }
    };
    let negotiation_used =
        if options.format.is_none() && !response_config.disable_accept_negotiation {
            match negotiate_output_format(request.header("accept"), &artifact) {
                Ok(Some(format)) => {
                    options.format = Some(format);
                    true
                }
                Ok(None) => false,
                Err(response) => return response,
            }
        } else {
            false
        };

    // Check input pixel count against the server-level limit before decode.
    // This runs before the cache lookup so that a policy change (lowering the
    // limit) takes effect immediately, even for previously-cached images.
    if let (Some(w), Some(h)) = (artifact.metadata.width, artifact.metadata.height) {
        let pixels = u64::from(w) * u64::from(h);
        if pixels > config.max_input_pixels {
            return response::unprocessable_entity_response(&format!(
                "input image has {pixels} pixels, server limit is {}",
                config.max_input_pixels
            ));
        }
    }

    let negotiated_accept = if negotiation_used {
        request.header("accept")
    } else {
        None
    };
    let cache_key = compute_cache_key(source_hash, &options, negotiated_accept, watermark_identity);

    if let Some(cache) = cache
        && let CacheLookup::Hit {
            media_type,
            body,
            age,
        } = cache.get(&cache_key)
    {
        CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
        let etag = build_image_etag(&body);
        let mut headers = build_image_response_headers(
            media_type,
            &etag,
            response_policy,
            negotiation_used,
            CacheHitStatus::Hit,
            response_config.public_cache_control.max_age,
            response_config.public_cache_control.stale_while_revalidate,
            &config.custom_response_headers,
        );
        headers.push(("Age".to_string(), age.as_secs().to_string()));
        if matches!(response_policy, ImageResponsePolicy::PublicGet)
            && if_none_match_matches(request.header("if-none-match"), &etag)
        {
            return HttpResponse::empty("304 Not Modified", headers);
        }
        return HttpResponse::binary_with_headers("200 OK", media_type.as_mime(), headers, body);
    }

    if cache.is_some() {
        CACHE_MISSES_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    let is_svg = artifact.media_type == MediaType::Svg;

    // Resolve watermark: reject SVG+watermark early (before fetch), then fetch if deferred.
    let watermark = if is_svg && watermark_source.is_some() {
        return bad_request_response("watermark is not supported for SVG source images");
    } else {
        match watermark_source {
            WatermarkSource::Deferred(validated) => {
                match fetch_watermark(validated, config, request_deadline) {
                    Ok(wm) => {
                        record_watermark_transform();
                        Some(wm)
                    }
                    Err(response) => return response,
                }
            }
            WatermarkSource::Ready(wm) => {
                record_watermark_transform();
                Some(wm)
            }
            WatermarkSource::None => None,
        }
    };

    let had_watermark = watermark.is_some();

    let transform_start = Instant::now();
    let mut request_obj = TransformRequest::new(artifact, options);
    request_obj.watermark = watermark;
    let result = if is_svg {
        match transform_svg(request_obj) {
            Ok(result) => result,
            Err(error) => {
                record_transform_error(&error);
                return transform_error_response(error);
            }
        }
    } else {
        match transform_raster(request_obj) {
            Ok(result) => result,
            Err(error) => {
                record_transform_error(&error);
                return transform_error_response(error);
            }
        }
    };
    record_transform_duration(result.artifact.media_type, transform_start);

    for warning in &result.warnings {
        let msg = format!("truss: {warning}");
        if let Some(c) = cache
            && let Some(handler) = &c.log_handler
        {
            handler(&msg);
        } else {
            stderr_write(&msg);
        }
    }

    let output = result.artifact;

    if let Some(cache) = cache {
        cache.put(&cache_key, output.media_type, &output.bytes);
    }

    let cache_hit_status = if cache.is_some() {
        CacheHitStatus::Miss
    } else {
        CacheHitStatus::Disabled
    };

    let etag = build_image_etag(&output.bytes);
    let headers = build_image_response_headers(
        output.media_type,
        &etag,
        response_policy,
        negotiation_used,
        cache_hit_status,
        response_config.public_cache_control.max_age,
        response_config.public_cache_control.stale_while_revalidate,
        &config.custom_response_headers,
    );

    if matches!(response_policy, ImageResponsePolicy::PublicGet)
        && if_none_match_matches(request.header("if-none-match"), &etag)
    {
        return HttpResponse::empty("304 Not Modified", headers);
    }

    let mut response = HttpResponse::binary_with_headers(
        "200 OK",
        output.media_type.as_mime(),
        headers,
        output.bytes,
    );
    if had_watermark {
        response
            .headers
            .push(("X-Truss-Watermark".to_string(), "true".to_string()));
    }
    response
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::config::DEFAULT_MAX_CONCURRENT_TRANSFORMS;
    use super::config::{
        DEFAULT_PUBLIC_MAX_AGE_SECONDS, DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
        parse_presets_from_env,
    };
    use super::http_parse::{
        DEFAULT_MAX_UPLOAD_BODY_BYTES, HttpRequest, find_header_terminator, read_request_body,
        read_request_headers, resolve_storage_path,
    };
    use super::multipart::parse_multipart_form_data;
    use super::remote::{PinnedResolver, prepare_remote_fetch_target};
    use super::response::auth_required_response;
    use super::response::{HttpResponse, bad_request_response};
    use super::{
        CacheHitStatus, DEFAULT_BIND_ADDR, ImageResponsePolicy, PublicSourceKind, ServerConfig,
        SignedUrlSource, TransformOptionsPayload, TransformSourcePayload, WatermarkSource,
        authorize_signed_request, bind_addr, build_image_etag, build_image_response_headers,
        canonical_query_without_signature, negotiate_output_format, parse_public_get_request,
        route_request, serve_once_with_config, sign_public_url, transform_source_bytes,
    };
    use crate::{
        Artifact, ArtifactMetadata, Fit, MediaType, RawArtifact, TransformOptions, sniff_artifact,
    };
    use hmac::{Hmac, Mac};
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    use sha2::Sha256;
    use std::collections::{BTreeMap, HashMap};
    use std::env;
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Test-only convenience wrapper that reads headers + body in one shot,
    /// preserving the original `read_request` semantics for existing tests.
    fn read_request<R: Read>(stream: &mut R) -> Result<HttpRequest, HttpResponse> {
        let partial = read_request_headers(stream, DEFAULT_MAX_UPLOAD_BODY_BYTES)?;
        read_request_body(stream, partial)
    }

    fn png_bytes() -> Vec<u8> {
        let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 4, 3, ColorType::Rgba8.into())
            .expect("encode png");
        bytes
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("truss-server-{name}-{unique}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_png(path: &Path) {
        fs::write(path, png_bytes()).expect("write png fixture");
    }

    fn artifact_with_alpha(has_alpha: bool) -> Artifact {
        Artifact::new(
            png_bytes(),
            MediaType::Png,
            ArtifactMetadata {
                width: Some(4),
                height: Some(3),
                frame_count: 1,
                duration: None,
                has_alpha: Some(has_alpha),
            },
        )
    }

    fn sign_public_query(
        method: &str,
        authority: &str,
        path: &str,
        query: &BTreeMap<String, String>,
        secret: &str,
    ) -> String {
        let canonical = format!(
            "{method}\n{authority}\n{path}\n{}",
            canonical_query_without_signature(query)
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("create hmac");
        mac.update(canonical.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    type FixtureResponse = (String, Vec<(String, String)>, Vec<u8>);

    fn read_fixture_request(stream: &mut TcpStream) {
        stream
            .set_nonblocking(false)
            .expect("configure fixture stream blocking mode");
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("configure fixture stream timeout");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = match stream.read(&mut chunk) {
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("read fixture request headers: {error}"),
            };
            if read == 0 {
                panic!("fixture request ended before headers were complete");
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(index) = find_header_terminator(&buffer) {
                break index;
            }
        };

        let header_text = std::str::from_utf8(&buffer[..header_end]).expect("fixture request utf8");
        let content_length = header_text
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find_map(|(name, value)| {
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .map(|value| {
                value
                    .parse::<usize>()
                    .expect("fixture content-length should be numeric")
            })
            .unwrap_or(0);

        let mut body = buffer.len().saturating_sub(header_end + 4);
        while body < content_length {
            let read = match stream.read(&mut chunk) {
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("read fixture request body: {error}"),
            };
            if read == 0 {
                panic!("fixture request body was truncated");
            }
            body += read;
        }
    }

    fn spawn_http_server(responses: Vec<FixtureResponse>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        listener
            .set_nonblocking(true)
            .expect("configure fixture server");
        let addr = listener.local_addr().expect("fixture server addr");
        let url = format!("http://{addr}/image");

        let handle = thread::spawn(move || {
            for (status, headers, body) in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let mut accepted = None;
                while std::time::Instant::now() < deadline {
                    match listener.accept() {
                        Ok(stream) => {
                            accepted = Some(stream);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept fixture request: {error}"),
                    }
                }

                let Some((mut stream, _)) = accepted else {
                    break;
                };
                read_fixture_request(&mut stream);
                let mut header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    header.push_str(&format!("{name}: {value}\r\n"));
                }
                header.push_str("\r\n");
                stream
                    .write_all(header.as_bytes())
                    .expect("write fixture headers");
                stream.write_all(&body).expect("write fixture body");
                stream.flush().expect("flush fixture response");
            }
        });

        (url, handle)
    }

    fn transform_request(path: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            target: "/images:transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: format!(
                "{{\"source\":{{\"kind\":\"path\",\"path\":\"{path}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
            )
            .into_bytes(),
        }
    }

    fn transform_url_request(url: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            target: "/images:transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: format!(
                "{{\"source\":{{\"kind\":\"url\",\"url\":\"{url}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
            )
            .into_bytes(),
        }
    }

    fn upload_request(file_bytes: &[u8], options_json: Option<&str>) -> HttpRequest {
        let boundary = "truss-test-boundary";
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(b"\r\n");

        if let Some(options_json) = options_json {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{options_json}\r\n"
                )
                .as_bytes(),
            );
        }

        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                (
                    "content-type".to_string(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ],
            body,
        }
    }

    fn metrics_request(with_auth: bool) -> HttpRequest {
        let mut headers = Vec::new();
        if with_auth {
            headers.push(("authorization".to_string(), "Bearer secret".to_string()));
        }

        HttpRequest {
            method: "GET".to_string(),
            target: "/metrics".to_string(),
            version: "HTTP/1.1".to_string(),
            headers,
            body: Vec::new(),
        }
    }

    fn response_body(response: &HttpResponse) -> &str {
        std::str::from_utf8(&response.body).expect("utf8 response body")
    }

    fn signed_public_request(target: &str, host: &str, secret: &str) -> HttpRequest {
        let (path, query) = target.split_once('?').expect("target has query");
        let mut query = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect::<BTreeMap<_, _>>();
        let signature = sign_public_query("GET", host, path, &query, secret);
        query.insert("signature".to_string(), signature);
        let final_query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(
                query
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            )
            .finish();

        HttpRequest {
            method: "GET".to_string(),
            target: format!("{path}?{final_query}"),
            version: "HTTP/1.1".to_string(),
            headers: vec![("host".to_string(), host.to_string())],
            body: Vec::new(),
        }
    }

    #[test]
    fn uses_default_bind_addr_when_env_is_missing() {
        unsafe { std::env::remove_var("TRUSS_BIND_ADDR") };
        assert_eq!(bind_addr(), DEFAULT_BIND_ADDR);
    }

    #[test]
    fn authorize_signed_request_accepts_a_valid_signature() {
        let request = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=public-dev&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-value",
        );
        let query = super::auth::parse_query_params(&request).expect("parse query");
        let config = ServerConfig::new(temp_dir("public-auth"), None)
            .with_signed_url_credentials("public-dev", "secret-value");

        authorize_signed_request(&request, &query, &config).expect("signed auth should pass");
    }

    #[test]
    fn authorize_signed_request_uses_public_base_url_authority() {
        let request = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=public-dev&expires=4102444800&format=jpeg",
            "cdn.example.com",
            "secret-value",
        );
        let query = super::auth::parse_query_params(&request).expect("parse query");
        let mut config = ServerConfig::new(temp_dir("public-authority"), None)
            .with_signed_url_credentials("public-dev", "secret-value");
        config.public_base_url = Some("https://cdn.example.com".to_string());

        authorize_signed_request(&request, &query, &config).expect("signed auth should pass");
    }

    #[test]
    fn negotiate_output_format_prefers_alpha_safe_formats_for_transparent_inputs() {
        let format =
            negotiate_output_format(Some("image/jpeg,image/png"), &artifact_with_alpha(true))
                .expect("negotiate output format")
                .expect("resolved output format");

        assert_eq!(format, MediaType::Png);
    }

    #[test]
    fn negotiate_output_format_prefers_avif_for_wildcard_accept() {
        let format = negotiate_output_format(Some("image/*"), &artifact_with_alpha(false))
            .expect("negotiate output format")
            .expect("resolved output format");

        assert_eq!(format, MediaType::Avif);
    }

    #[test]
    fn build_image_response_headers_include_cache_and_safety_metadata() {
        let headers = build_image_response_headers(
            MediaType::Webp,
            &build_image_etag(b"demo"),
            ImageResponsePolicy::PublicGet,
            true,
            CacheHitStatus::Disabled,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
            &[],
        );

        assert!(headers.contains(&(
            "Cache-Control".to_string(),
            "public, max-age=3600, stale-while-revalidate=60".to_string()
        )));
        assert!(headers.contains(&("Vary".to_string(), "Accept".to_string())));
        assert!(headers.contains(&("X-Content-Type-Options".to_string(), "nosniff".to_string())));
        assert!(headers.contains(&(
            "Content-Disposition".to_string(),
            "inline; filename=\"truss.webp\"".to_string()
        )));
        assert!(headers.contains(&(
            "Cache-Status".to_string(),
            "\"truss\"; fwd=miss".to_string()
        )));
    }

    #[test]
    fn build_image_response_headers_include_csp_sandbox_for_svg() {
        let headers = build_image_response_headers(
            MediaType::Svg,
            &build_image_etag(b"svg-data"),
            ImageResponsePolicy::PublicGet,
            true,
            CacheHitStatus::Disabled,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
            &[],
        );

        assert!(headers.contains(&("Content-Security-Policy".to_string(), "sandbox".to_string())));
    }

    #[test]
    fn build_image_response_headers_omit_csp_sandbox_for_raster() {
        let headers = build_image_response_headers(
            MediaType::Png,
            &build_image_etag(b"png-data"),
            ImageResponsePolicy::PublicGet,
            true,
            CacheHitStatus::Disabled,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
            &[],
        );

        assert!(!headers.iter().any(|(k, _)| *k == "Content-Security-Policy"));
    }

    #[test]
    fn backpressure_rejects_when_at_capacity() {
        let config = ServerConfig::new(std::env::temp_dir(), None);
        config
            .transforms_in_flight
            .store(DEFAULT_MAX_CONCURRENT_TRANSFORMS, Ordering::Relaxed);

        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let png_bytes = {
            let mut buf = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
            encoder
                .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
                .unwrap();
            buf
        };

        let response = transform_source_bytes(
            png_bytes,
            TransformOptions::default(),
            None,
            &request,
            ImageResponsePolicy::PrivateTransform,
            &config,
            WatermarkSource::None,
            None,
            None,
        );

        assert!(response.status.contains("503"));

        assert_eq!(
            config.transforms_in_flight.load(Ordering::Relaxed),
            DEFAULT_MAX_CONCURRENT_TRANSFORMS
        );
    }

    #[test]
    fn backpressure_rejects_with_custom_concurrency_limit() {
        let custom_limit = 2u64;
        let mut config = ServerConfig::new(std::env::temp_dir(), None);
        config.max_concurrent_transforms = custom_limit;
        config
            .transforms_in_flight
            .store(custom_limit, Ordering::Relaxed);

        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let png_bytes = {
            let mut buf = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
            encoder
                .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
                .unwrap();
            buf
        };

        let response = transform_source_bytes(
            png_bytes,
            TransformOptions::default(),
            None,
            &request,
            ImageResponsePolicy::PrivateTransform,
            &config,
            WatermarkSource::None,
            None,
            None,
        );

        assert!(response.status.contains("503"));
    }

    #[test]
    fn compute_cache_key_is_deterministic() {
        let opts = TransformOptions {
            width: Some(300),
            height: Some(200),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let key1 = super::cache::compute_cache_key("source-abc", &opts, None, None);
        let key2 = super::cache::compute_cache_key("source-abc", &opts, None, None);
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 64);
    }

    #[test]
    fn compute_cache_key_differs_for_different_options() {
        let opts1 = TransformOptions {
            width: Some(300),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let opts2 = TransformOptions {
            width: Some(400),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let key1 = super::cache::compute_cache_key("same-source", &opts1, None, None);
        let key2 = super::cache::compute_cache_key("same-source", &opts2, None, None);
        assert_ne!(key1, key2);
    }

    #[test]
    fn compute_cache_key_includes_accept_when_present() {
        let opts = TransformOptions::default();
        let key_no_accept = super::cache::compute_cache_key("src", &opts, None, None);
        let key_with_accept =
            super::cache::compute_cache_key("src", &opts, Some("image/webp"), None);
        assert_ne!(key_no_accept, key_with_accept);
    }

    #[test]
    fn transform_cache_put_and_get_round_trips() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        cache.put(
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            MediaType::Png,
            b"png-data",
        );
        let result = cache.get("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890");

        match result {
            super::cache::CacheLookup::Hit {
                media_type, body, ..
            } => {
                assert_eq!(media_type, MediaType::Png);
                assert_eq!(body, b"png-data");
            }
            super::cache::CacheLookup::Miss => panic!("expected cache hit"),
        }
    }

    #[test]
    fn transform_cache_miss_for_unknown_key() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        let result = cache.get("0000001234567890abcdef1234567890abcdef1234567890abcdef1234567890");
        assert!(matches!(result, super::cache::CacheLookup::Miss));
    }

    #[test]
    fn transform_cache_uses_sharded_layout() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        cache.put(key, MediaType::Jpeg, b"jpeg-data");

        let expected = dir.path().join("ab").join("cd").join("ef").join(key);
        assert!(
            expected.exists(),
            "sharded file should exist at {expected:?}"
        );
    }

    #[test]
    fn transform_cache_expired_entry_is_miss() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let mut cache = super::cache::TransformCache::new(dir.path().to_path_buf());
        cache.ttl = Duration::from_secs(0);

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        cache.put(key, MediaType::Png, b"data");

        std::thread::sleep(Duration::from_millis(10));

        let result = cache.get(key);
        assert!(matches!(result, super::cache::CacheLookup::Miss));
    }

    #[test]
    fn transform_cache_handles_corrupted_entry_as_miss() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let path = cache.entry_path(key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"corrupted-data-without-header").unwrap();

        let result = cache.get(key);
        assert!(matches!(result, super::cache::CacheLookup::Miss));
    }

    #[test]
    fn cache_status_header_reflects_hit() {
        let headers = build_image_response_headers(
            MediaType::Png,
            &build_image_etag(b"data"),
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Hit,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
            &[],
        );
        assert!(headers.contains(&("Cache-Status".to_string(), "\"truss\"; hit".to_string())));
    }

    #[test]
    fn cache_status_header_reflects_miss() {
        let headers = build_image_response_headers(
            MediaType::Png,
            &build_image_etag(b"data"),
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
            &[],
        );
        assert!(headers.contains(&(
            "Cache-Status".to_string(),
            "\"truss\"; fwd=miss".to_string()
        )));
    }

    #[test]
    fn origin_cache_put_and_get_round_trips() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        cache.put("src", "https://example.com/image.png", b"raw-source-bytes");
        let result = cache.get("src", "https://example.com/image.png");

        assert_eq!(result.as_deref(), Some(b"raw-source-bytes".as_ref()));
    }

    #[test]
    fn origin_cache_miss_for_unknown_url() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        assert!(
            cache
                .get("src", "https://unknown.example.com/missing.png")
                .is_none()
        );
    }

    #[test]
    fn origin_cache_expired_entry_is_none() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let mut cache = super::cache::OriginCache::new(dir.path());
        cache.ttl = Duration::from_secs(0);

        cache.put("src", "https://example.com/img.png", b"data");
        std::thread::sleep(Duration::from_millis(10));

        assert!(cache.get("src", "https://example.com/img.png").is_none());
    }

    #[test]
    fn origin_cache_uses_origin_subdirectory() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        cache.put("src", "https://example.com/test.png", b"bytes");

        let origin_dir = dir.path().join("origin");
        assert!(origin_dir.exists(), "origin subdirectory should exist");
    }

    #[test]
    fn sign_public_url_builds_a_signed_path_url() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/image.png".to_string(),
                version: Some("v1".to_string()),
            },
            &crate::TransformOptions {
                format: Some(MediaType::Jpeg),
                width: Some(320),
                ..crate::TransformOptions::default()
            },
            "public-dev",
            "secret-value",
            4_102_444_800,
            None,
            None,
        )
        .expect("sign public URL");

        assert!(url.starts_with("https://cdn.example.com/images/by-path?"));
        assert!(url.contains("path=%2Fimage.png"));
        assert!(url.contains("version=v1"));
        assert!(url.contains("width=320"));
        assert!(url.contains("format=jpeg"));
        assert!(url.contains("keyId=public-dev"));
        assert!(url.contains("expires=4102444800"));
        assert!(url.contains("signature="));
    }

    #[test]
    fn parse_public_get_request_rejects_unknown_query_parameters() {
        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("signature".to_string(), "deadbeef".to_string()),
            ("unexpected".to_string(), "value".to_string()),
        ]);

        let config = ServerConfig::new(temp_dir("parse-query"), None);
        let response = parse_public_get_request(&query, PublicSourceKind::Path, &config)
            .expect_err("unknown query should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("is not supported"));
    }

    #[test]
    fn parse_public_get_request_resolves_preset() {
        let mut presets = HashMap::new();
        presets.insert(
            "thumbnail".to_string(),
            TransformOptionsPayload {
                width: Some(150),
                height: Some(150),
                fit: Some("cover".to_string()),
                ..TransformOptionsPayload::default()
            },
        );
        let config = ServerConfig::new(temp_dir("preset"), None).with_presets(presets);

        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("preset".to_string(), "thumbnail".to_string()),
        ]);
        let (_, options, _) =
            parse_public_get_request(&query, PublicSourceKind::Path, &config).unwrap();

        assert_eq!(options.width, Some(150));
        assert_eq!(options.height, Some(150));
        assert_eq!(options.fit, Some(Fit::Cover));
    }

    #[test]
    fn parse_public_get_request_preset_with_override() {
        let mut presets = HashMap::new();
        presets.insert(
            "thumbnail".to_string(),
            TransformOptionsPayload {
                width: Some(150),
                height: Some(150),
                fit: Some("cover".to_string()),
                format: Some("webp".to_string()),
                ..TransformOptionsPayload::default()
            },
        );
        let config = ServerConfig::new(temp_dir("preset-override"), None).with_presets(presets);

        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("preset".to_string(), "thumbnail".to_string()),
            ("width".to_string(), "200".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]);
        let (_, options, _) =
            parse_public_get_request(&query, PublicSourceKind::Path, &config).unwrap();

        assert_eq!(options.width, Some(200));
        assert_eq!(options.height, Some(150));
        assert_eq!(options.format, Some(MediaType::Jpeg));
    }

    #[test]
    fn parse_public_get_request_rejects_unknown_preset() {
        let config = ServerConfig::new(temp_dir("preset-unknown"), None);

        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("preset".to_string(), "nonexistent".to_string()),
        ]);
        let response = parse_public_get_request(&query, PublicSourceKind::Path, &config)
            .expect_err("unknown preset should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("unknown preset"));
    }

    #[test]
    fn sign_public_url_includes_preset_in_signed_url() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/image.png".to_string(),
                version: None,
            },
            &crate::TransformOptions::default(),
            "public-dev",
            "secret-value",
            4_102_444_800,
            None,
            Some("thumbnail"),
        )
        .expect("sign public URL with preset");

        assert!(url.contains("preset=thumbnail"));
        assert!(url.contains("signature="));
    }

    #[test]
    #[serial]
    fn parse_presets_from_env_parses_json() {
        unsafe {
            env::set_var(
                "TRUSS_PRESETS",
                r#"{"thumb":{"width":100,"height":100,"fit":"cover"}}"#,
            );
            env::remove_var("TRUSS_PRESETS_FILE");
        }
        let presets = parse_presets_from_env().unwrap();
        unsafe {
            env::remove_var("TRUSS_PRESETS");
        }

        assert_eq!(presets.len(), 1);
        let thumb = presets.get("thumb").unwrap();
        assert_eq!(thumb.width, Some(100));
        assert_eq!(thumb.height, Some(100));
        assert_eq!(thumb.fit.as_deref(), Some("cover"));
    }

    #[test]
    fn prepare_remote_fetch_target_pins_the_validated_netloc() {
        let target = prepare_remote_fetch_target(
            "http://1.1.1.1/image.png",
            &ServerConfig::new(temp_dir("pin"), Some("secret".to_string())),
        )
        .expect("prepare remote target");

        assert_eq!(target.netloc, "1.1.1.1:80");
        assert_eq!(target.addrs, vec![SocketAddr::from(([1, 1, 1, 1], 80))]);
    }

    #[test]
    fn pinned_resolver_rejects_unexpected_netlocs() {
        use ureq::unversioned::resolver::Resolver;

        let resolver = PinnedResolver {
            expected_netloc: "example.com:443".to_string(),
            addrs: vec![SocketAddr::from(([93, 184, 216, 34], 443))],
        };

        let config = ureq::config::Config::builder().build();
        let timeout = ureq::unversioned::transport::NextTimeout {
            after: ureq::unversioned::transport::time::Duration::Exact(
                std::time::Duration::from_secs(30),
            ),
            reason: ureq::Timeout::Resolve,
        };

        let uri: ureq::http::Uri = "https://example.com/path".parse().unwrap();
        let result = resolver
            .resolve(&uri, &config, timeout)
            .expect("resolve expected netloc");
        assert_eq!(&result[..], &[SocketAddr::from(([93, 184, 216, 34], 443))]);

        let bad_uri: ureq::http::Uri = "https://proxy.example:8080/path".parse().unwrap();
        let timeout2 = ureq::unversioned::transport::NextTimeout {
            after: ureq::unversioned::transport::time::Duration::Exact(
                std::time::Duration::from_secs(30),
            ),
            reason: ureq::Timeout::Resolve,
        };
        let error = resolver
            .resolve(&bad_uri, &config, timeout2)
            .expect_err("unexpected netloc should fail");
        assert!(matches!(error, ureq::Error::HostNotFound));
    }

    #[test]
    fn health_live_returns_status_service_version() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/live".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(temp_dir("live"), None));

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse live body");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "truss");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn health_ready_returns_ok_when_storage_exists() {
        let storage = temp_dir("ready-ok");
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(storage, None));

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready body");
        assert_eq!(body["status"], "ok");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "storageRoot" && c["status"] == "ok")
        );
    }

    #[test]
    fn health_ready_returns_503_when_storage_missing() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let config = ServerConfig::new(PathBuf::from("/nonexistent-truss-test-dir"), None);
        let response = route_request(request, &config);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready fail body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "storageRoot" && c["status"] == "fail")
        );
    }

    #[test]
    fn health_ready_returns_503_when_cache_root_missing() {
        let storage = temp_dir("ready-cache-fail");
        let mut config = ServerConfig::new(storage, None);
        config.cache_root = Some(PathBuf::from("/nonexistent-truss-cache-dir"));

        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &config);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready cache body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "cacheRoot" && c["status"] == "fail")
        );
    }

    #[test]
    fn health_returns_comprehensive_diagnostic() {
        let storage = temp_dir("health-diag");
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(storage, None));

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "truss");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["uptimeSeconds"].is_u64());
        assert!(body["checks"].is_array());
    }

    #[test]
    fn unknown_path_returns_not_found() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/unknown".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(temp_dir("not-found"), None));

        assert_eq!(response.status, "404 Not Found");
        assert_eq!(response.content_type, Some("application/problem+json"));
        let body = response_body(&response);
        assert!(body.contains("\"type\":\"about:blank\""));
        assert!(body.contains("\"title\":\"Not Found\""));
        assert!(body.contains("\"status\":404"));
        assert!(body.contains("not found"));
    }

    #[test]
    fn transform_endpoint_requires_authentication() {
        let storage_root = temp_dir("auth");
        write_png(&storage_root.join("image.png"));
        let mut request = transform_request("/image.png");
        request.headers.retain(|(name, _)| name != "authorization");

        let response = route_request(
            request,
            &ServerConfig::new(storage_root, Some("secret".to_string())),
        );

        assert_eq!(response.status, "401 Unauthorized");
        assert!(response_body(&response).contains("authorization required"));
    }

    #[test]
    fn transform_endpoint_returns_service_unavailable_without_configured_token() {
        let storage_root = temp_dir("token");
        write_png(&storage_root.join("image.png"));

        let response = route_request(
            transform_request("/image.png"),
            &ServerConfig::new(storage_root, None),
        );

        assert_eq!(response.status, "503 Service Unavailable");
        assert!(response_body(&response).contains("bearer token is not configured"));
    }

    #[test]
    fn transform_endpoint_transforms_a_path_source() {
        let storage_root = temp_dir("transform");
        write_png(&storage_root.join("image.png"));

        let response = route_request(
            transform_request("/image.png"),
            &ServerConfig::new(storage_root, Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type, Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
        assert_eq!(artifact.metadata.width, Some(4));
        assert_eq!(artifact.metadata.height, Some(3));
    }

    #[test]
    fn transform_endpoint_rejects_private_url_sources_by_default() {
        let response = route_request(
            transform_url_request("http://127.0.0.1:8080/image.png"),
            &ServerConfig::new(temp_dir("url-blocked"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "403 Forbidden");
        assert!(response_body(&response).contains("port is not allowed"));
    }

    #[test]
    fn transform_endpoint_transforms_a_url_source_when_insecure_allowance_is_enabled() {
        let (url, handle) = spawn_http_server(vec![(
            "200 OK".to_string(),
            vec![("Content-Type".to_string(), "image/png".to_string())],
            png_bytes(),
        )]);

        let response = route_request(
            transform_url_request(&url),
            &ServerConfig::new(temp_dir("url"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type, Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn transform_endpoint_follows_remote_redirects() {
        let (redirect_url, handle) = spawn_http_server(vec![
            (
                "302 Found".to_string(),
                vec![("Location".to_string(), "/final-image".to_string())],
                Vec::new(),
            ),
            (
                "200 OK".to_string(),
                vec![("Content-Type".to_string(), "image/png".to_string())],
                png_bytes(),
            ),
        ]);

        let response = route_request(
            transform_url_request(&redirect_url),
            &ServerConfig::new(temp_dir("redirect"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "200 OK");
        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn upload_endpoint_transforms_uploaded_file() {
        let response = route_request(
            upload_request(&png_bytes(), Some(r#"{"format":"jpeg"}"#)),
            &ServerConfig::new(temp_dir("upload"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type, Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn upload_endpoint_requires_a_file_field() {
        let boundary = "truss-test-boundary";
        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                (
                    "content-type".to_string(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ],
            body: format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"jpeg\"}}\r\n--{boundary}--\r\n"
            )
            .into_bytes(),
        };

        let response = route_request(
            request,
            &ServerConfig::new(temp_dir("upload-missing-file"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("requires a `file` field"));
    }

    #[test]
    fn upload_endpoint_rejects_non_multipart_content_type() {
        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: br#"{"file":"not-really-json"}"#.to_vec(),
        };

        let response = route_request(
            request,
            &ServerConfig::new(temp_dir("upload-content-type"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "415 Unsupported Media Type");
        assert!(response_body(&response).contains("multipart/form-data"));
    }

    #[test]
    fn parse_upload_request_extracts_file_and_options() {
        let request = upload_request(&png_bytes(), Some(r#"{"width":8,"format":"jpeg"}"#));
        let boundary =
            super::multipart::parse_multipart_boundary(&request).expect("parse boundary");
        let (file_bytes, options, _watermark) =
            super::multipart::parse_upload_request(&request.body, &boundary)
                .expect("parse upload body");

        assert_eq!(file_bytes, png_bytes());
        assert_eq!(options.width, Some(8));
        assert_eq!(options.format, Some(MediaType::Jpeg));
    }

    #[test]
    fn metrics_endpoint_does_not_require_authentication() {
        let response = route_request(
            metrics_request(false),
            &ServerConfig::new(temp_dir("metrics-no-auth"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
    }

    #[test]
    fn metrics_endpoint_returns_prometheus_text() {
        super::metrics::record_http_metrics(super::metrics::RouteMetric::Health, "200 OK");
        let response = route_request(
            metrics_request(true),
            &ServerConfig::new(temp_dir("metrics"), Some("secret".to_string())),
        );
        let body = response_body(&response);

        assert_eq!(response.status, "200 OK");
        assert_eq!(
            response.content_type,
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        assert!(body.contains("truss_http_requests_total"));
        assert!(body.contains("truss_http_requests_by_route_total{route=\"/health\"}"));
        assert!(body.contains("truss_http_responses_total{status=\"200\"}"));
        // Histogram metrics
        assert!(body.contains("# TYPE truss_http_request_duration_seconds histogram"));
        assert!(
            body.contains(
                "truss_http_request_duration_seconds_bucket{route=\"/health\",le=\"+Inf\"}"
            )
        );
        assert!(body.contains("# TYPE truss_transform_duration_seconds histogram"));
        assert!(body.contains("# TYPE truss_storage_request_duration_seconds histogram"));
        // Transform error counter
        assert!(body.contains("# TYPE truss_transform_errors_total counter"));
        assert!(body.contains("truss_transform_errors_total{error_type=\"decode_failed\"}"));
    }

    #[test]
    fn metrics_endpoint_returns_401_when_token_required() {
        let mut config = ServerConfig::new(temp_dir("metrics-auth"), None);
        config.metrics_token = Some("my-secret-token".to_string());

        // No auth header → 401
        let response = route_request(metrics_request(false), &config);
        assert_eq!(response.status, "401 Unauthorized");
    }

    #[test]
    fn metrics_endpoint_accepts_valid_token() {
        let mut config = ServerConfig::new(temp_dir("metrics-auth-ok"), None);
        config.metrics_token = Some("secret".to_string());

        // Bearer secret matches
        let response = route_request(metrics_request(true), &config);
        assert_eq!(response.status, "200 OK");
    }

    #[test]
    fn metrics_endpoint_rejects_wrong_token() {
        let mut config = ServerConfig::new(temp_dir("metrics-auth-bad"), None);
        config.metrics_token = Some("correct-token".to_string());

        // Bearer secret ≠ correct-token
        let response = route_request(metrics_request(true), &config);
        assert_eq!(response.status, "401 Unauthorized");
    }

    #[test]
    fn metrics_endpoint_returns_404_when_disabled() {
        let mut config = ServerConfig::new(temp_dir("metrics-disabled"), None);
        config.disable_metrics = true;

        let response = route_request(metrics_request(false), &config);
        assert_eq!(response.status, "404 Not Found");
    }

    #[test]
    fn transform_endpoint_rejects_unsupported_remote_content_encoding() {
        let (url, handle) = spawn_http_server(vec![(
            "200 OK".to_string(),
            vec![
                ("Content-Type".to_string(), "image/png".to_string()),
                ("Content-Encoding".to_string(), "compress".to_string()),
            ],
            png_bytes(),
        )]);

        let response = route_request(
            transform_url_request(&url),
            &ServerConfig::new(temp_dir("encoding"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "502 Bad Gateway");
        assert!(response_body(&response).contains("unsupported content-encoding"));
    }

    #[test]
    fn resolve_storage_path_rejects_parent_segments() {
        let storage_root = temp_dir("resolve");
        let response = resolve_storage_path(&storage_root, "../escape.png")
            .expect_err("parent segments should be rejected");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("must not contain root"));
    }

    #[test]
    fn read_request_parses_headers_and_body() {
        let request_bytes = b"POST /images:transform HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(request_bytes);
        let request = read_request(&mut cursor).expect("parse request");

        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/images:transform");
        assert_eq!(request.version, "HTTP/1.1");
        assert_eq!(request.header("host"), Some("localhost"));
        assert_eq!(request.body, b"{}");
    }

    #[test]
    fn read_request_rejects_duplicate_content_length() {
        let request_bytes =
            b"POST /images:transform HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(request_bytes);
        let response = read_request(&mut cursor).expect_err("duplicate headers should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("content-length"));
    }

    #[test]
    fn serve_once_handles_a_tcp_request() {
        let storage_root = temp_dir("serve-once");
        let config = ServerConfig::new(storage_root, None);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("read local addr");

        let server = thread::spawn(move || serve_once_with_config(listener, config));

        let mut stream = TcpStream::connect(addr).expect("connect to test server");
        stream
            .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write request");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");

        server
            .join()
            .expect("join test server thread")
            .expect("serve one request");

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.contains("\"status\":\"ok\""));
        assert!(response.contains("\"service\":\"truss\""));
        assert!(response.contains("\"version\":"));
    }

    #[test]
    fn helper_error_responses_use_rfc7807_problem_details() {
        let response = auth_required_response("authorization required");
        let bad_request = bad_request_response("bad input");

        assert_eq!(
            response.content_type,
            Some("application/problem+json"),
            "error responses must use application/problem+json"
        );
        assert_eq!(bad_request.content_type, Some("application/problem+json"),);

        let auth_body = response_body(&response);
        assert!(auth_body.contains("authorization required"));
        assert!(auth_body.contains("\"type\":\"about:blank\""));
        assert!(auth_body.contains("\"title\":\"Unauthorized\""));
        assert!(auth_body.contains("\"status\":401"));

        let bad_body = response_body(&bad_request);
        assert!(bad_body.contains("bad input"));
        assert!(bad_body.contains("\"type\":\"about:blank\""));
        assert!(bad_body.contains("\"title\":\"Bad Request\""));
        assert!(bad_body.contains("\"status\":400"));
    }

    #[test]
    fn parse_headers_rejects_duplicate_host() {
        let lines = "Host: example.com\r\nHost: evil.com\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_authorization() {
        let lines = "Authorization: Bearer a\r\nAuthorization: Bearer b\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_content_type() {
        let lines = "Content-Type: application/json\r\nContent-Type: text/plain\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_transfer_encoding() {
        let lines = "Transfer-Encoding: chunked\r\nTransfer-Encoding: gzip\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_single_transfer_encoding() {
        let lines = "Host: example.com\r\nTransfer-Encoding: chunked\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        let err = result.unwrap_err();
        assert!(
            err.status.starts_with("501"),
            "expected 501 status, got: {}",
            err.status
        );
        assert!(
            String::from_utf8_lossy(&err.body).contains("Transfer-Encoding"),
            "error response should mention Transfer-Encoding"
        );
    }

    #[test]
    fn parse_headers_rejects_transfer_encoding_identity() {
        let lines = "Transfer-Encoding: identity\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_allows_single_instances_of_singleton_headers() {
        let lines =
            "Host: example.com\r\nAuthorization: Bearer tok\r\nContent-Type: application/json\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 3);
    }

    #[test]
    fn max_body_for_multipart_uses_upload_limit() {
        let headers = vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=abc".to_string(),
        )];
        assert_eq!(
            super::http_parse::max_body_for_headers(
                &headers,
                super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
            ),
            super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_json_uses_default_limit() {
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        assert_eq!(
            super::http_parse::max_body_for_headers(
                &headers,
                super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
            ),
            super::http_parse::MAX_REQUEST_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_no_content_type_uses_default_limit() {
        let headers: Vec<(String, String)> = vec![];
        assert_eq!(
            super::http_parse::max_body_for_headers(
                &headers,
                super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
            ),
            super::http_parse::MAX_REQUEST_BODY_BYTES
        );
    }

    fn make_test_config() -> ServerConfig {
        ServerConfig::new(std::env::temp_dir(), None)
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn storage_backend_parse_filesystem_aliases() {
        assert_eq!(
            super::StorageBackend::parse("filesystem").unwrap(),
            super::StorageBackend::Filesystem
        );
        assert_eq!(
            super::StorageBackend::parse("fs").unwrap(),
            super::StorageBackend::Filesystem
        );
        assert_eq!(
            super::StorageBackend::parse("local").unwrap(),
            super::StorageBackend::Filesystem
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_backend_parse_s3() {
        assert_eq!(
            super::StorageBackend::parse("s3").unwrap(),
            super::StorageBackend::S3
        );
        assert_eq!(
            super::StorageBackend::parse("S3").unwrap(),
            super::StorageBackend::S3
        );
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn storage_backend_parse_gcs() {
        assert_eq!(
            super::StorageBackend::parse("gcs").unwrap(),
            super::StorageBackend::Gcs
        );
        assert_eq!(
            super::StorageBackend::parse("GCS").unwrap(),
            super::StorageBackend::Gcs
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn storage_backend_parse_rejects_unknown() {
        assert!(super::StorageBackend::parse("").is_err());
        #[cfg(not(feature = "azure"))]
        assert!(super::StorageBackend::parse("azure").is_err());
        #[cfg(feature = "azure")]
        assert!(super::StorageBackend::parse("azure").is_ok());
    }

    #[test]
    fn versioned_source_hash_returns_none_without_version() {
        let source = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: None,
        };
        assert!(source.versioned_source_hash(&make_test_config()).is_none());
    }

    #[test]
    fn versioned_source_hash_is_deterministic() {
        let cfg = make_test_config();
        let source = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let hash1 = source.versioned_source_hash(&cfg).unwrap();
        let hash2 = source.versioned_source_hash(&cfg).unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn versioned_source_hash_differs_by_version() {
        let cfg = make_test_config();
        let v1 = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let v2 = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v2".to_string()),
        };
        assert_ne!(
            v1.versioned_source_hash(&cfg).unwrap(),
            v2.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    fn versioned_source_hash_differs_by_kind() {
        let cfg = make_test_config();
        let path = TransformSourcePayload::Path {
            path: "example.com/image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let url = TransformSourcePayload::Url {
            url: "example.com/image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            path.versioned_source_hash(&cfg).unwrap(),
            url.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    fn versioned_source_hash_differs_by_storage_root() {
        let cfg1 = ServerConfig::new(PathBuf::from("/data/images"), None);
        let cfg2 = ServerConfig::new(PathBuf::from("/other/images"), None);
        let source = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg1).unwrap(),
            source.versioned_source_hash(&cfg2).unwrap()
        );
    }

    #[test]
    fn versioned_source_hash_differs_by_insecure_flag() {
        let mut cfg1 = make_test_config();
        cfg1.allow_insecure_url_sources = false;
        let mut cfg2 = make_test_config();
        cfg2.allow_insecure_url_sources = true;
        let source = TransformSourcePayload::Url {
            url: "http://example.com/img.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg1).unwrap(),
            source.versioned_source_hash(&cfg2).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_variant_is_deterministic() {
        let cfg = make_test_config();
        let source = TransformSourcePayload::Storage {
            bucket: Some("my-bucket".to_string()),
            key: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let hash1 = source.versioned_source_hash(&cfg).unwrap();
        let hash2 = source.versioned_source_hash(&cfg).unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_differs_from_path() {
        let cfg = make_test_config();
        let path_source = TransformSourcePayload::Path {
            path: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let storage_source = TransformSourcePayload::Storage {
            bucket: Some("my-bucket".to_string()),
            key: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            path_source.versioned_source_hash(&cfg).unwrap(),
            storage_source.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_differs_by_bucket() {
        let cfg = make_test_config();
        let s1 = TransformSourcePayload::Storage {
            bucket: Some("bucket-a".to_string()),
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let s2 = TransformSourcePayload::Storage {
            bucket: Some("bucket-b".to_string()),
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            s1.versioned_source_hash(&cfg).unwrap(),
            s2.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_differs_by_backend() {
        let cfg_fs = make_test_config();
        let mut cfg_s3 = make_test_config();
        cfg_s3.storage_backend = super::StorageBackend::S3;

        let source = TransformSourcePayload::Path {
            path: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg_fs).unwrap(),
            source.versioned_source_hash(&cfg_s3).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_differs_by_endpoint() {
        let mut cfg_a = make_test_config();
        cfg_a.storage_backend = super::StorageBackend::S3;
        cfg_a.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "shared",
            Some("http://minio-a:9000"),
        )));

        let mut cfg_b = make_test_config();
        cfg_b.storage_backend = super::StorageBackend::S3;
        cfg_b.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "shared",
            Some("http://minio-b:9000"),
        )));

        let source = TransformSourcePayload::Storage {
            bucket: None,
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg_a).unwrap(),
            source.versioned_source_hash(&cfg_b).unwrap(),
        );
        assert_ne!(cfg_a, cfg_b);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_backend_default_is_filesystem() {
        let cfg = make_test_config();
        assert_eq!(cfg.storage_backend, super::StorageBackend::Filesystem);
        assert!(cfg.s3_context.is_none());
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_payload_deserializes_storage_variant() {
        let json = r#"{"source":{"kind":"storage","key":"photos/hero.jpg"},"options":{}}"#;
        let payload: super::TransformImageRequestPayload = serde_json::from_str(json).unwrap();
        match payload.source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert!(bucket.is_none());
                assert_eq!(key, "photos/hero.jpg");
                assert!(version.is_none());
            }
            _ => panic!("expected Storage variant"),
        }
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_payload_deserializes_with_bucket() {
        let json = r#"{"source":{"kind":"storage","bucket":"my-bucket","key":"img.png","version":"v2"},"options":{}}"#;
        let payload: super::TransformImageRequestPayload = serde_json::from_str(json).unwrap();
        match payload.source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert_eq!(bucket.as_deref(), Some("my-bucket"));
                assert_eq!(key, "img.png");
                assert_eq!(version.as_deref(), Some("v2"));
            }
            _ => panic!("expected Storage variant"),
        }
    }

    // -----------------------------------------------------------------------
    // S3: default_bucket fallback with bucket: None
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_uses_default_bucket_when_bucket_is_none() {
        let mut cfg_a = make_test_config();
        cfg_a.storage_backend = super::StorageBackend::S3;
        cfg_a.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "bucket-a", None,
        )));

        let mut cfg_b = make_test_config();
        cfg_b.storage_backend = super::StorageBackend::S3;
        cfg_b.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "bucket-b", None,
        )));

        let source = TransformSourcePayload::Storage {
            bucket: None,
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        // Different default_bucket ⇒ different hash
        assert_ne!(
            source.versioned_source_hash(&cfg_a).unwrap(),
            source.versioned_source_hash(&cfg_b).unwrap(),
        );
        // PartialEq also distinguishes them
        assert_ne!(cfg_a, cfg_b);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_returns_none_without_bucket_or_context() {
        let mut cfg = make_test_config();
        cfg.storage_backend = super::StorageBackend::S3;
        cfg.s3_context = None;

        let source = TransformSourcePayload::Storage {
            bucket: None,
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        // No bucket available ⇒ None (falls back to content-hash)
        assert!(source.versioned_source_hash(&cfg).is_none());
    }

    // -----------------------------------------------------------------------
    // S3: from_env branches
    //
    // These tests mutate process-global environment variables. A mutex
    // serializes them so that parallel test threads cannot interfere, and
    // each test saves/restores the variables it touches.
    // -----------------------------------------------------------------------

    #[cfg(feature = "s3")]
    static FROM_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(feature = "s3")]
    const S3_ENV_VARS: &[&str] = &[
        "TRUSS_STORAGE_ROOT",
        "TRUSS_STORAGE_BACKEND",
        "TRUSS_S3_BUCKET",
    ];

    /// Save current values, run `f`, then restore originals regardless of
    /// panics. Holds `FROM_ENV_MUTEX` for the duration.
    #[cfg(feature = "s3")]
    fn with_s3_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let _guard = FROM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<String>)> = S3_ENV_VARS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        // Apply requested overrides
        for &(key, value) in vars {
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // Restore originals
        for (key, original) in saved {
            match original {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_rejects_invalid_storage_backend() {
        let storage = temp_dir("env-bad-backend");
        let storage_str = storage.to_str().unwrap().to_string();
        with_s3_env(
            &[
                ("TRUSS_STORAGE_ROOT", Some(&storage_str)),
                ("TRUSS_STORAGE_BACKEND", Some("nosuchbackend")),
                ("TRUSS_S3_BUCKET", None),
            ],
            || {
                let result = ServerConfig::from_env();
                assert!(result.is_err());
                let msg = result.unwrap_err().to_string();
                assert!(msg.contains("unknown storage backend"), "got: {msg}");
            },
        );
        let _ = std::fs::remove_dir_all(storage);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_rejects_s3_without_bucket() {
        let storage = temp_dir("env-no-bucket");
        let storage_str = storage.to_str().unwrap().to_string();
        with_s3_env(
            &[
                ("TRUSS_STORAGE_ROOT", Some(&storage_str)),
                ("TRUSS_STORAGE_BACKEND", Some("s3")),
                ("TRUSS_S3_BUCKET", None),
            ],
            || {
                let result = ServerConfig::from_env();
                assert!(result.is_err());
                let msg = result.unwrap_err().to_string();
                assert!(msg.contains("TRUSS_S3_BUCKET"), "got: {msg}");
            },
        );
        let _ = std::fs::remove_dir_all(storage);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_accepts_s3_with_bucket() {
        let storage = temp_dir("env-s3-ok");
        let storage_str = storage.to_str().unwrap().to_string();
        with_s3_env(
            &[
                ("TRUSS_STORAGE_ROOT", Some(&storage_str)),
                ("TRUSS_STORAGE_BACKEND", Some("s3")),
                ("TRUSS_S3_BUCKET", Some("my-images")),
            ],
            || {
                let cfg =
                    ServerConfig::from_env().expect("from_env should succeed with s3 + bucket");
                assert_eq!(cfg.storage_backend, super::StorageBackend::S3);
                let ctx = cfg.s3_context.expect("s3_context should be Some");
                assert_eq!(ctx.default_bucket, "my-images");
            },
        );
        let _ = std::fs::remove_dir_all(storage);
    }

    // -----------------------------------------------------------------------
    // S3: health endpoint
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "s3")]
    fn health_ready_s3_returns_503_when_context_missing() {
        let storage = temp_dir("health-s3-no-ctx");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::S3;
        config.s3_context = None;

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "storageBackend" && c["status"] == "fail"),
            "expected s3Client fail check in {body}",
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn health_ready_s3_includes_s3_client_check() {
        let storage = temp_dir("health-s3-ok");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::S3;
        config.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "test-bucket",
            None,
        )));

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        // The s3Client check will report "fail" because there is no real S3
        // endpoint, but the important thing is that the check is present.
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().any(|c| c["name"] == "storageBackend"),
            "expected s3Client check in {body}",
        );
    }

    // -----------------------------------------------------------------------
    // S3: public by-path remap (leading slash trimmed, Storage variant used)
    // -----------------------------------------------------------------------

    /// Replicates the Path→Storage remap that `handle_public_get_request`
    /// performs when `storage_backend == S3`, so we can inspect the resulting
    /// key without issuing a real S3 request.
    #[cfg(feature = "s3")]
    fn remap_path_to_storage(path: &str, version: Option<&str>) -> TransformSourcePayload {
        let source = TransformSourcePayload::Path {
            path: path.to_string(),
            version: version.map(|v| v.to_string()),
        };
        match source {
            TransformSourcePayload::Path { path, version } => TransformSourcePayload::Storage {
                bucket: None,
                key: path.trim_start_matches('/').to_string(),
                version,
            },
            other => other,
        }
    }

    #[test]
    #[cfg(feature = "s3")]
    fn public_by_path_s3_remap_trims_leading_slash() {
        // Paths with a leading slash (the common case from signed URLs like
        // `path=/image.png`) must have the slash stripped so that the S3 key
        // is `image.png`, not `/image.png`.
        let source = remap_path_to_storage("/photos/hero.jpg", Some("v1"));
        match &source {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg", "leading / must be trimmed");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        // Without a leading slash the key must be unchanged.
        let source2 = remap_path_to_storage("photos/hero.jpg", Some("v1"));
        match &source2 {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        // Both must produce the same versioned hash (same effective key).
        let mut cfg = make_test_config();
        cfg.storage_backend = super::StorageBackend::S3;
        cfg.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "my-bucket",
            None,
        )));
        assert_eq!(
            source.versioned_source_hash(&cfg),
            source2.versioned_source_hash(&cfg),
            "leading-slash and no-leading-slash paths must hash identically after trim",
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn public_by_path_s3_remap_produces_storage_variant() {
        // Verify the remap converts Path to Storage with bucket: None.
        let source = remap_path_to_storage("/image.png", None);
        match source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert!(bucket.is_none(), "bucket must be None (use default)");
                assert_eq!(key, "image.png");
                assert!(version.is_none());
            }
            _ => panic!("expected Storage variant"),
        }
    }

    // -----------------------------------------------------------------------
    // GCS: health endpoint
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "gcs")]
    fn health_ready_gcs_returns_503_when_context_missing() {
        let storage = temp_dir("health-gcs-no-ctx");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Gcs;
        config.gcs_context = None;

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "storageBackend" && c["status"] == "fail"),
            "expected gcsClient fail check in {body}",
        );
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn health_ready_gcs_includes_gcs_client_check() {
        let storage = temp_dir("health-gcs-ok");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Gcs;
        config.gcs_context = Some(std::sync::Arc::new(super::gcs::GcsContext::for_test(
            "test-bucket",
            None,
        )));

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        // The gcsClient check will report "fail" because there is no real GCS
        // endpoint, but the important thing is that the check is present.
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().any(|c| c["name"] == "storageBackend"),
            "expected gcsClient check in {body}",
        );
    }

    // -----------------------------------------------------------------------
    // GCS: public by-path remap (leading slash trimmed, Storage variant used)
    // -----------------------------------------------------------------------

    #[cfg(feature = "gcs")]
    fn remap_path_to_storage_gcs(path: &str, version: Option<&str>) -> TransformSourcePayload {
        let source = TransformSourcePayload::Path {
            path: path.to_string(),
            version: version.map(|v| v.to_string()),
        };
        match source {
            TransformSourcePayload::Path { path, version } => TransformSourcePayload::Storage {
                bucket: None,
                key: path.trim_start_matches('/').to_string(),
                version,
            },
            other => other,
        }
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn public_by_path_gcs_remap_trims_leading_slash() {
        let source = remap_path_to_storage_gcs("/photos/hero.jpg", Some("v1"));
        match &source {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg", "leading / must be trimmed");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        let source2 = remap_path_to_storage_gcs("photos/hero.jpg", Some("v1"));
        match &source2 {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        let mut cfg = make_test_config();
        cfg.storage_backend = super::StorageBackend::Gcs;
        cfg.gcs_context = Some(std::sync::Arc::new(super::gcs::GcsContext::for_test(
            "my-bucket",
            None,
        )));
        assert_eq!(
            source.versioned_source_hash(&cfg),
            source2.versioned_source_hash(&cfg),
            "leading-slash and no-leading-slash paths must hash identically after trim",
        );
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn public_by_path_gcs_remap_produces_storage_variant() {
        let source = remap_path_to_storage_gcs("/image.png", None);
        match source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert!(bucket.is_none(), "bucket must be None (use default)");
                assert_eq!(key, "image.png");
                assert!(version.is_none());
            }
            _ => panic!("expected Storage variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Azure: health endpoint
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "azure")]
    fn health_ready_azure_returns_503_when_context_missing() {
        let storage = temp_dir("health-azure-no-ctx");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Azure;
        config.azure_context = None;

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "storageBackend" && c["status"] == "fail"),
            "expected azureClient fail check in {body}",
        );
    }

    #[test]
    #[cfg(feature = "azure")]
    fn health_ready_azure_includes_azure_client_check() {
        let storage = temp_dir("health-azure-ok");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Azure;
        config.azure_context = Some(std::sync::Arc::new(super::azure::AzureContext::for_test(
            "test-bucket",
            "http://localhost:10000/devstoreaccount1",
        )));

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().any(|c| c["name"] == "storageBackend"),
            "expected azureClient check in {body}",
        );
    }

    #[test]
    fn read_request_rejects_json_body_over_1mib() {
        let body = vec![b'x'; super::http_parse::MAX_REQUEST_BODY_BYTES + 1];
        let content_length = body.len();
        let raw = format!(
            "POST /images:transform HTTP/1.1\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {content_length}\r\n\r\n"
        );
        let mut data = raw.into_bytes();
        data.extend_from_slice(&body);
        let result = read_request(&mut data.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn read_request_accepts_multipart_body_over_1mib() {
        let payload_size = super::http_parse::MAX_REQUEST_BODY_BYTES + 100;
        let body_content = vec![b'A'; payload_size];
        let boundary = "test-boundary-123";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.jpg\"\r\n\r\n").as_bytes());
        body.extend_from_slice(&body_content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let content_length = body.len();
        let raw = format!(
            "POST /images HTTP/1.1\r\n\
             Content-Type: multipart/form-data; boundary={boundary}\r\n\
             Content-Length: {content_length}\r\n\r\n"
        );
        let mut data = raw.into_bytes();
        data.extend_from_slice(&body);
        let result = read_request(&mut data.as_slice());
        assert!(
            result.is_ok(),
            "multipart upload over 1 MiB should be accepted"
        );
    }

    #[test]
    fn multipart_boundary_in_payload_does_not_split_part() {
        let boundary = "abc123";
        let fake_boundary_in_payload = format!("\r\n--{boundary}NOTREAL");
        let part_body = format!("before{fake_boundary_in_payload}after");
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\
             {part_body}\r\n\
             --{boundary}--\r\n"
        );

        let parts = parse_multipart_form_data(body.as_bytes(), boundary)
            .expect("should parse despite boundary-like string in payload");
        assert_eq!(parts.len(), 1, "should have exactly one part");

        let part_data = &body.as_bytes()[parts[0].body_range.clone()];
        let part_text = std::str::from_utf8(part_data).unwrap();
        assert!(
            part_text.contains("NOTREAL"),
            "part body should contain the full fake boundary string"
        );
    }

    #[test]
    fn multipart_normal_two_parts_still_works() {
        let boundary = "testboundary";
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"field1\"\r\n\r\n\
             value1\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"field2\"\r\n\r\n\
             value2\r\n\
             --{boundary}--\r\n"
        );

        let parts = parse_multipart_form_data(body.as_bytes(), boundary)
            .expect("should parse two normal parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "field1");
        assert_eq!(parts[1].name, "field2");
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_default() {
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.storage_timeout_secs, 30);
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_custom() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_TIMEOUT_SECS", "60");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.storage_timeout_secs, 60);
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_min_boundary() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_TIMEOUT_SECS", "1");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.storage_timeout_secs, 1);
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_max_boundary() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_TIMEOUT_SECS", "300");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.storage_timeout_secs, 300);
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_empty_string_uses_default() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_TIMEOUT_SECS", "");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.storage_timeout_secs, 30);
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_TIMEOUT_SECS", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 300"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_over_max_rejected() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_TIMEOUT_SECS", "301");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 300"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_TIMEOUT_SECS", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "error should mention positive integer: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_TIMEOUT_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_default() {
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_concurrent_transforms, 64);
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_custom() {
        unsafe {
            std::env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "128");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_concurrent_transforms, 128);
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_min_boundary() {
        unsafe {
            std::env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "1");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_concurrent_transforms, 1);
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_max_boundary() {
        unsafe {
            std::env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "1024");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_concurrent_transforms, 1024);
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_empty_uses_default() {
        unsafe {
            std::env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_concurrent_transforms, 64);
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 1024"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_over_max_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "1025");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 1024"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
    }

    #[test]
    #[serial]
    fn test_max_concurrent_transforms_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "error should mention positive integer: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_default() {
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 30);
    }

    #[test]
    #[serial]
    fn test_transform_deadline_custom() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "60");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 60);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_min_boundary() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "1");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 1);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_max_boundary() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "300");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 300);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_empty_uses_default() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 30);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 300"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_over_max_rejected() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "301");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 300"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "error should mention positive integer: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(feature = "azure")]
    fn test_azure_container_env_var_required() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_BACKEND", "azure");
            std::env::remove_var("TRUSS_AZURE_CONTAINER");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("TRUSS_AZURE_CONTAINER"),
            "error should mention TRUSS_AZURE_CONTAINER: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_BACKEND");
        }
    }

    #[test]
    fn server_config_debug_redacts_bearer_token_and_signed_url_secret() {
        let mut config = ServerConfig::new(
            temp_dir("debug-redact"),
            Some("super-secret-token-12345".to_string()),
        );
        config.signed_url_key_id = Some("visible-key-id".to_string());
        config.signed_url_secret = Some("super-secret-hmac-key".to_string());
        let debug = format!("{config:?}");
        assert!(
            !debug.contains("super-secret-token-12345"),
            "bearer_token leaked in Debug output: {debug}"
        );
        assert!(
            !debug.contains("super-secret-hmac-key"),
            "signed_url_secret leaked in Debug output: {debug}"
        );
        assert!(
            debug.contains("[REDACTED]"),
            "expected [REDACTED] in Debug output: {debug}"
        );
        assert!(
            debug.contains("visible-key-id"),
            "signed_url_key_id should be visible: {debug}"
        );
    }

    #[test]
    fn authorize_headers_accepts_correct_bearer_token() {
        let config = ServerConfig::new(temp_dir("auth-ok"), Some("correct-token".to_string()));
        let headers = vec![(
            "authorization".to_string(),
            "Bearer correct-token".to_string(),
        )];
        assert!(super::authorize_request_headers(&headers, &config).is_ok());
    }

    #[test]
    fn authorize_headers_rejects_wrong_bearer_token() {
        let config = ServerConfig::new(temp_dir("auth-wrong"), Some("correct-token".to_string()));
        let headers = vec![(
            "authorization".to_string(),
            "Bearer wrong-token".to_string(),
        )];
        let err = super::authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn authorize_headers_rejects_missing_header() {
        let config = ServerConfig::new(temp_dir("auth-missing"), Some("correct-token".to_string()));
        let headers: Vec<(String, String)> = vec![];
        let err = super::authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    // ── TransformSlot RAII guard ──────────────────────────────────────

    #[test]
    fn transform_slot_acquire_succeeds_under_limit() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let slot = super::TransformSlot::try_acquire(&counter, 2);
        assert!(slot.is_some());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transform_slot_acquire_returns_none_at_limit() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let _s1 = super::TransformSlot::try_acquire(&counter, 1).unwrap();
        let s2 = super::TransformSlot::try_acquire(&counter, 1);
        assert!(s2.is_none());
        // Counter must still be 1 (failed acquire must not leak).
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transform_slot_drop_decrements_counter() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        {
            let _slot = super::TransformSlot::try_acquire(&counter, 4).unwrap();
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        // After drop the counter must return to zero.
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn transform_slot_multiple_acquires_up_to_limit() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let limit = 3u64;
        let mut slots = Vec::new();
        for _ in 0..limit {
            slots.push(super::TransformSlot::try_acquire(&counter, limit).unwrap());
        }
        assert_eq!(counter.load(Ordering::Relaxed), limit);
        // One more must fail.
        assert!(super::TransformSlot::try_acquire(&counter, limit).is_none());
        assert_eq!(counter.load(Ordering::Relaxed), limit);
        // Drop all slots.
        slots.clear();
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    // ── Access log via emit_access_log ────────────────────────────────

    #[test]
    fn emit_access_log_produces_json_with_expected_fields() {
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        let captured = Arc::new(Mutex::new(String::new()));
        let captured_clone = Arc::clone(&captured);
        let handler: super::LogHandler =
            Arc::new(move |msg: &str| *captured_clone.lock().unwrap() = msg.to_owned());

        let mut config = ServerConfig::new(temp_dir("access-log"), None);
        config.log_handler = Some(handler);

        let start = Instant::now();
        super::emit_access_log(
            &config,
            &super::AccessLogEntry {
                request_id: "req-123",
                method: "GET",
                path: "/image.png",
                route: "transform",
                status: "200",
                start,
                cache_status: Some("hit"),
                watermark: false,
            },
        );

        let output = captured.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        assert_eq!(parsed["kind"], "access_log");
        assert_eq!(parsed["request_id"], "req-123");
        assert_eq!(parsed["method"], "GET");
        assert_eq!(parsed["path"], "/image.png");
        assert_eq!(parsed["route"], "transform");
        assert_eq!(parsed["status"], "200");
        assert_eq!(parsed["cache_status"], "hit");
        assert!(parsed["latency_ms"].is_u64());
    }

    #[test]
    fn emit_access_log_null_cache_status_when_none() {
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        let captured = Arc::new(Mutex::new(String::new()));
        let captured_clone = Arc::clone(&captured);
        let handler: super::LogHandler =
            Arc::new(move |msg: &str| *captured_clone.lock().unwrap() = msg.to_owned());

        let mut config = ServerConfig::new(temp_dir("access-log-none"), None);
        config.log_handler = Some(handler);

        super::emit_access_log(
            &config,
            &super::AccessLogEntry {
                request_id: "req-456",
                method: "POST",
                path: "/upload",
                route: "upload",
                status: "201",
                start: Instant::now(),
                cache_status: None,
                watermark: false,
            },
        );

        let output = captured.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        assert!(parsed["cache_status"].is_null());
    }

    // ── X-Request-Id header ───────────────────────────────────────────

    #[test]
    fn x_request_id_is_extracted_from_incoming_headers() {
        let headers = vec![
            ("host".to_string(), "localhost".to_string()),
            ("x-request-id".to_string(), "custom-id-abc".to_string()),
        ];
        assert_eq!(
            super::extract_request_id(&headers),
            Some("custom-id-abc".to_string())
        );
    }

    #[test]
    fn x_request_id_not_extracted_when_empty() {
        let headers = vec![("x-request-id".to_string(), "".to_string())];
        assert!(super::extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_not_extracted_when_absent() {
        let headers = vec![("host".to_string(), "localhost".to_string())];
        assert!(super::extract_request_id(&headers).is_none());
    }

    // ── Cache status extraction ───────────────────────────────────────

    #[test]
    fn cache_status_hit_detected() {
        let headers: Vec<(String, String)> =
            vec![("Cache-Status".to_string(), "\"truss\"; hit".to_string())];
        assert_eq!(super::extract_cache_status(&headers), Some("hit"));
    }

    #[test]
    fn cache_status_miss_detected() {
        let headers: Vec<(String, String)> = vec![(
            "Cache-Status".to_string(),
            "\"truss\"; fwd=miss".to_string(),
        )];
        assert_eq!(super::extract_cache_status(&headers), Some("miss"));
    }

    #[test]
    fn cache_status_none_when_header_absent() {
        let headers: Vec<(String, String)> =
            vec![("Content-Type".to_string(), "image/png".to_string())];
        assert!(super::extract_cache_status(&headers).is_none());
    }

    #[test]
    fn signing_keys_populated_by_with_signed_url_credentials() {
        let config = ServerConfig::new(temp_dir("signing-keys-populated"), None)
            .with_signed_url_credentials("key-alpha", "secret-alpha");

        assert_eq!(
            config.signing_keys.get("key-alpha").map(String::as_str),
            Some("secret-alpha")
        );
    }

    #[test]
    fn authorize_signed_request_accepts_multiple_keys() {
        let mut extra = HashMap::new();
        extra.insert("key-beta".to_string(), "secret-beta".to_string());
        let config = ServerConfig::new(temp_dir("multi-key-accept"), None)
            .with_signed_url_credentials("key-alpha", "secret-alpha")
            .with_signing_keys(extra);

        // Sign with key-alpha
        let request_alpha = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=key-alpha&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-alpha",
        );
        let query_alpha =
            super::auth::parse_query_params(&request_alpha).expect("parse query alpha");
        authorize_signed_request(&request_alpha, &query_alpha, &config)
            .expect("key-alpha should be accepted");

        // Sign with key-beta
        let request_beta = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=key-beta&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-beta",
        );
        let query_beta = super::auth::parse_query_params(&request_beta).expect("parse query beta");
        authorize_signed_request(&request_beta, &query_beta, &config)
            .expect("key-beta should be accepted");
    }

    #[test]
    fn authorize_signed_request_rejects_unknown_key() {
        let config = ServerConfig::new(temp_dir("unknown-key-reject"), None)
            .with_signed_url_credentials("key-alpha", "secret-alpha");

        let request = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=key-unknown&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-unknown",
        );
        let query = super::auth::parse_query_params(&request).expect("parse query");
        authorize_signed_request(&request, &query, &config)
            .expect_err("unknown key should be rejected");
    }

    // ── Security: X-Request-Id CRLF injection prevention ─────────────

    #[test]
    fn x_request_id_rejects_crlf_injection() {
        let headers = vec![(
            "x-request-id".to_string(),
            "evil\r\nX-Injected: true".to_string(),
        )];
        assert!(
            super::extract_request_id(&headers).is_none(),
            "CRLF in request ID must be rejected"
        );
    }

    #[test]
    fn x_request_id_rejects_lone_cr() {
        let headers = vec![("x-request-id".to_string(), "evil\rid".to_string())];
        assert!(super::extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_rejects_lone_lf() {
        let headers = vec![("x-request-id".to_string(), "evil\nid".to_string())];
        assert!(super::extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_rejects_nul_byte() {
        let headers = vec![("x-request-id".to_string(), "evil\0id".to_string())];
        assert!(super::extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_accepts_normal_uuid() {
        let headers = vec![(
            "x-request-id".to_string(),
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        )];
        assert_eq!(
            super::extract_request_id(&headers),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    // ── Characterization: ServerConfig defaults ──────────────────────

    #[test]
    fn server_config_new_has_expected_defaults() {
        let root = temp_dir("cfg-defaults");
        let config = ServerConfig::new(root.clone(), None);
        assert_eq!(config.storage_root, root);
        assert!(config.bearer_token.is_none());
        assert!(config.signed_url_secret.is_none());
        assert!(config.signing_keys.is_empty());
        assert!(config.presets.is_empty());
        assert_eq!(
            config.max_concurrent_transforms,
            DEFAULT_MAX_CONCURRENT_TRANSFORMS
        );
        assert_eq!(
            config.public_max_age_seconds,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS
        );
        assert_eq!(
            config.public_stale_while_revalidate_seconds,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS
        );
        assert!(!config.allow_insecure_url_sources);
    }

    #[test]
    fn server_config_builder_with_signed_url_credentials_overwrites() {
        let root = temp_dir("cfg-builder");
        let config = ServerConfig::new(root, None)
            .with_signed_url_credentials("key1", "secret1")
            .with_signed_url_credentials("key2", "secret2");
        assert!(config.signing_keys.contains_key("key1"));
        assert!(config.signing_keys.contains_key("key2"));
    }

    // ── Characterization: route_request classification ───────────────

    #[test]
    fn route_request_returns_not_found_for_unknown_path() {
        let root = temp_dir("route-unknown");
        let config = ServerConfig::new(root, None);
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/nonexistent".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("host".to_string(), "localhost".to_string())],
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        assert_eq!(response.status, "404 Not Found");
    }

    #[test]
    fn route_request_health_returns_200() {
        let root = temp_dir("route-health");
        let config = ServerConfig::new(root, None);
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("host".to_string(), "localhost".to_string())],
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        assert_eq!(response.status, "200 OK");
    }

    // ── Characterization: TransformSlot thread safety ────────────────

    #[test]
    fn transform_slot_concurrent_acquire_respects_limit() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let limit = 4u64;
        let num_threads = 16;
        let barrier = Arc::new(Barrier::new(num_threads));
        let acquired = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let counter = Arc::clone(&counter);
                let barrier = Arc::clone(&barrier);
                let acquired = Arc::clone(&acquired);
                thread::spawn(move || {
                    barrier.wait();
                    if let Some(_slot) = super::TransformSlot::try_acquire(&counter, limit) {
                        acquired.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_millis(10));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_default() {
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 40_000_000);
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_custom() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "10000000");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 10_000_000);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_min_boundary() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "1");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 1);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_max_boundary() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "100000000");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 100_000_000);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_empty_uses_default() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 40_000_000);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_INPUT_PIXELS"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_over_max_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "100000001");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_INPUT_PIXELS"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_INPUT_PIXELS"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_default() {
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_upload_bytes, 100 * 1024 * 1024);
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_custom() {
        unsafe {
            std::env::set_var("TRUSS_MAX_UPLOAD_BYTES", "5242880");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_upload_bytes, 5 * 1024 * 1024);
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_UPLOAD_BYTES", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_UPLOAD_BYTES"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_UPLOAD_BYTES", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_UPLOAD_BYTES"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
    }

    #[test]
    fn max_body_for_multipart_uses_custom_upload_limit() {
        let headers = vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=abc".to_string(),
        )];
        let custom_limit = 5 * 1024 * 1024;
        assert_eq!(
            super::http_parse::max_body_for_headers(&headers, custom_limit),
            custom_limit
        );
    }

    #[test]
    fn health_includes_max_input_pixels() {
        let storage = temp_dir("health-pixels");
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let config = ServerConfig::new(storage, None);
        let response = route_request(request, &config);

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        assert_eq!(body["maxInputPixels"], 40_000_000);
    }

    #[test]
    fn health_includes_transform_capacity_details() {
        let storage = temp_dir("health-capacity");
        let config = ServerConfig::new(storage, None);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        let checks = body["checks"].as_array().expect("checks array");
        let capacity = checks
            .iter()
            .find(|c| c["name"] == "transformCapacity")
            .expect("transformCapacity check");
        assert_eq!(capacity["current"], 0);
        assert_eq!(capacity["max"], 64);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_rss_bytes_returns_some() {
        let rss = super::process_rss_bytes();
        assert!(rss.is_some());
        assert!(rss.unwrap() > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_free_bytes_returns_some_for_existing_dir() {
        let dir = temp_dir("disk-free");
        let free = super::disk_free_bytes(&dir);
        assert!(free.is_some());
        assert!(free.unwrap() > 0);
    }

    #[test]
    fn health_ready_returns_503_when_memory_exceeded() {
        let storage = temp_dir("health-mem");
        let mut config = ServerConfig::new(storage, None);
        // Set threshold to 1 byte — guaranteed to be exceeded.
        config.health_max_memory_bytes = Some(1);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        // On Linux, RSS > 1 byte → 503. On other platforms, memory check
        // is skipped so the response is 200.
        if cfg!(target_os = "linux") {
            assert_eq!(response.status, "503 Service Unavailable");
        }
    }

    #[test]
    fn health_includes_memory_usage_on_linux() {
        let storage = temp_dir("health-mem-report");
        let mut config = ServerConfig::new(storage, None);
        config.health_max_memory_bytes = Some(u64::MAX);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        if cfg!(target_os = "linux") {
            let checks = body["checks"].as_array().expect("checks array");
            let mem = checks
                .iter()
                .find(|c| c["name"] == "memoryUsage")
                .expect("memoryUsage check");
            assert_eq!(mem["status"], "ok");
            assert!(mem["rssBytes"].as_u64().unwrap() > 0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_free_bytes_returns_none_for_nonexistent_path() {
        let free = super::disk_free_bytes(std::path::Path::new("/nonexistent/path/xyz"));
        assert!(free.is_none());
    }

    #[test]
    fn health_ready_503_body_contains_fail_status() {
        let storage = temp_dir("health-ready-body");
        std::fs::remove_dir_all(&storage).ok();
        let config = ServerConfig::new(storage, None);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        let storage_check = checks
            .iter()
            .find(|c| c["name"] == "storageRoot")
            .expect("storageRoot check");
        assert_eq!(storage_check["status"], "fail");
    }

    #[test]
    fn health_ready_cache_disk_free_shown_when_cache_root_set() {
        let storage = temp_dir("health-ready-cache-disk");
        let cache = temp_dir("health-ready-cache-disk-cache");
        let mut config = ServerConfig::new(storage, None).with_cache_root(cache);
        config.health_cache_min_free_bytes = Some(1);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        let disk_check = checks
            .iter()
            .find(|c| c["name"] == "cacheDiskFree")
            .expect("cacheDiskFree check");
        assert_eq!(disk_check["status"], "ok");
        if cfg!(target_os = "linux") {
            assert!(disk_check["freeBytes"].as_u64().is_some());
        }
        assert_eq!(disk_check["thresholdBytes"], 1);
    }

    #[test]
    fn health_ready_no_cache_disk_free_without_cache_root() {
        let storage = temp_dir("health-ready-no-cache");
        let config = ServerConfig::new(storage, None);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().all(|c| c["name"] != "cacheDiskFree"),
            "cacheDiskFree should not appear without cache_root"
        );
    }

    #[test]
    fn health_ready_memory_check_includes_details() {
        let storage = temp_dir("health-ready-mem-detail");
        let mut config = ServerConfig::new(storage, None);
        config.health_max_memory_bytes = Some(u64::MAX);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        let mem = checks.iter().find(|c| c["name"] == "memoryUsage");
        if cfg!(target_os = "linux") {
            let mem = mem.expect("memoryUsage check present on Linux");
            assert_eq!(mem["status"], "ok");
            assert_eq!(mem["thresholdBytes"], u64::MAX);
            assert!(mem["rssBytes"].as_u64().is_some());
        } else {
            assert!(mem.is_none(), "memoryUsage should be absent on non-Linux");
        }
    }

    // ── graceful shutdown: draining flag ─────────────────────────────

    #[test]
    fn health_ready_returns_503_when_draining() {
        let storage = temp_dir("health-ready-draining");
        let config = ServerConfig::new(storage, None);
        config.draining.store(true, Ordering::Relaxed);

        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "draining" && c["status"] == "fail")
        );
    }

    #[test]
    fn health_ready_returns_ok_when_not_draining() {
        let storage = temp_dir("health-ready-not-draining");
        let config = ServerConfig::new(storage, None);
        // draining is false by default.
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready body");
        assert_eq!(body["status"], "ok");
        // Should not have a draining check entry.
        let checks = body["checks"].as_array().expect("checks array");
        assert!(!checks.iter().any(|c| c["name"] == "draining"));
    }

    // ── Drain during normal request processing (m10) ─────────────

    #[test]
    fn health_live_returns_200_while_draining() {
        let storage = temp_dir("live-draining");
        let config = ServerConfig::new(storage, None);
        config.draining.store(true, Ordering::Relaxed);

        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/live".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        // Liveness should always return 200 even when draining — only
        // readiness returns 503.
        assert_eq!(response.status, "200 OK");
    }

    #[test]
    fn normal_request_processed_while_draining() {
        let storage = temp_dir("normal-draining");
        let config = ServerConfig::new(storage, None);
        config.draining.store(true, Ordering::Relaxed);

        // A non-health, non-image request should still be routed (e.g. 404
        // because the path doesn't match any route) — it should NOT get a
        // 503 just because the server is draining.
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/nonexistent".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        // The path doesn't match any route, so we get 404 — NOT 503.
        assert_eq!(response.status, "404 Not Found");
    }
}
