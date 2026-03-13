/// Server startup, shutdown, signal handling, and connection management.
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::time::Duration;

use super::config::{LogLevel, ServerConfig};
use super::handler::TransformOptionsPayload;
use super::routing::handle_stream;
use super::stderr_write;

pub(super) const SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Number of worker threads for handling incoming connections concurrently.
const WORKER_THREADS: usize = 8;

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
    for (ok, name) in super::handler::storage_health_check(&config) {
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
                    cfg.log_warn(&format!("failed to handle connection: {err}"));
                }
            }
        }));
    }

    // Install signal handler for graceful shutdown.  The handler sets the
    // shared `draining` flag (so /health/ready returns 503 immediately) and
    // writes a byte to a self-pipe to wake the accept loop.
    let (shutdown_read_fd, shutdown_write_fd) = create_shutdown_pipe()?;
    install_signal_handler(
        Arc::clone(&config.draining),
        shutdown_write_fd,
        Arc::clone(&config.log_level),
    );

    // Spawn a background thread to hot-reload presets when TRUSS_PRESETS_FILE changes.
    if let Some(ref path) = config.presets_file_path {
        let presets = Arc::clone(&config.presets);
        let draining = Arc::clone(&config.draining);
        let cfg = Arc::clone(&config);
        let path = path.clone();
        std::thread::Builder::new()
            .name("preset-watcher".into())
            .spawn(move || preset_watcher(presets, path, draining, cfg))
            .expect("failed to spawn preset watcher thread");
    }

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
/// Global log level, cycled by SIGUSR1 (Unix only).
static GLOBAL_LOG_LEVEL: std::sync::atomic::AtomicPtr<AtomicU8> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(unix)]
fn install_signal_handler(draining: Arc<AtomicBool>, write_fd: i32, log_level: Arc<AtomicU8>) {
    // Store the write fd and draining pointer in globals accessible from the
    // async-signal-safe handler.
    SHUTDOWN_PIPE_WR.store(write_fd, Ordering::SeqCst);
    // SAFETY: `Arc::into_raw` leaks intentionally — the pointer remains valid
    // for the process lifetime.  The signal handler only calls `AtomicBool::store`
    // and `libc::write`, both of which are async-signal-safe.
    let ptr = Arc::into_raw(draining).cast_mut();
    GLOBAL_DRAINING.store(ptr, Ordering::SeqCst);
    // SAFETY: same as above — leaked intentionally for the process lifetime.
    let lvl_ptr = Arc::into_raw(log_level).cast_mut();
    GLOBAL_LOG_LEVEL.store(lvl_ptr, Ordering::SeqCst);

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

        let mut sa_usr1: libc::sigaction = std::mem::zeroed();
        sa_usr1.sa_sigaction = sigusr1_handler as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa_usr1.sa_mask);
        sa_usr1.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGUSR1, &sa_usr1, std::ptr::null_mut());
    }
}

/// SIGUSR1 handler: cycles the log level.
///
/// This is async-signal-safe because it only performs atomic load/store
/// operations and a raw `libc::write` to stderr.
#[cfg(unix)]
extern "C" fn sigusr1_handler(_sig: libc::c_int) {
    let ptr = GLOBAL_LOG_LEVEL.load(Ordering::SeqCst);
    if ptr.is_null() {
        return;
    }
    let level_atomic = unsafe { &*ptr };
    let current = level_atomic.load(Ordering::SeqCst);
    let next = LogLevel::from_u8(current).cycle();
    level_atomic.store(next as u8, Ordering::SeqCst);

    // Write a log message directly to stderr (async-signal-safe).
    let msg = match next {
        LogLevel::Error => b"[log] level changed to error\n" as &[u8],
        LogLevel::Warn => b"[log] level changed to warn\n",
        LogLevel::Info => b"[log] level changed to info\n",
        LogLevel::Debug => b"[log] level changed to debug\n",
    };
    unsafe { libc::write(2, msg.as_ptr().cast(), msg.len()) };
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
fn install_signal_handler(draining: Arc<AtomicBool>, _write_fd: i32, _log_level: Arc<AtomicU8>) {
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

// ---------------------------------------------------------------------------
// Preset hot-reload watcher
// ---------------------------------------------------------------------------

/// Polling interval for the preset file watcher.
const PRESET_WATCH_INTERVAL: Duration = Duration::from_secs(5);

/// Background thread that watches `TRUSS_PRESETS_FILE` for changes and reloads
/// presets atomically. On parse failure, the previous valid presets are kept.
pub(super) fn preset_watcher(
    presets: Arc<std::sync::RwLock<std::collections::HashMap<String, TransformOptionsPayload>>>,
    path: std::path::PathBuf,
    draining: Arc<AtomicBool>,
    config: Arc<ServerConfig>,
) {
    use super::config::parse_presets_file;
    use std::fs;

    let mut last_modified = fs::metadata(&path).and_then(|m| m.modified()).ok();

    loop {
        std::thread::sleep(PRESET_WATCH_INTERVAL);

        if draining.load(Ordering::Relaxed) {
            break;
        }

        let current_modified = match fs::metadata(&path).and_then(|m| m.modified()) {
            Ok(mtime) => Some(mtime),
            Err(err) => {
                config.log_warn(&format!(
                    "[presets] failed to stat `{}`: {err}",
                    path.display()
                ));
                continue;
            }
        };

        if current_modified == last_modified {
            continue;
        }

        match parse_presets_file(&path) {
            Ok(new_presets) => {
                let count = new_presets.len();
                *presets.write().expect("presets lock poisoned") = new_presets;
                last_modified = current_modified;
                config.log(&format!(
                    "[presets] reloaded {count} presets from `{}`",
                    path.display()
                ));
            }
            Err(err) => {
                config.log_warn(&format!(
                    "[presets] reload failed for `{}`: {err} (keeping previous presets)",
                    path.display()
                ));
                // Do NOT update last_modified here — the file may have been read
                // mid-write (torn read). By keeping the old mtime, the watcher
                // will retry on the next poll cycle and pick up the completed file.
            }
        }
    }
}
