/// Server startup, shutdown, signal handling, and connection management.
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use super::config::LogLevel;
use super::config::ServerConfig;
use super::handler::TransformOptionsPayload;
use super::routing::handle_stream;
use super::stderr_write;

pub(super) const SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a connection may take to deliver a complete set of request headers.
///
/// This is a wall-clock budget for the whole header phase, not an inactivity timeout. A
/// socket read timeout resets on every byte, so a client sending one header line every
/// thirty seconds holds its worker forever: with a pool of [`worker_pool_size`] threads and a
/// worker dedicated to a connection from accept to close, that many trickling connections
/// take the whole server down, `/health/live` included, for the cost of a few bytes a
/// minute. The budget is what makes the header phase end.
pub(super) const HEADER_READ_DEADLINE: Duration = Duration::from_secs(15);
/// Number of worker threads held back for requests that are not transforms.
const WORKER_THREADS: usize = 8;

/// How much stack a connection worker runs on.
///
/// A worker decodes, and decoding an AVIF wants close to a megabyte and a half in a build
/// without optimizations, against the two megabytes a thread gets by default. That is not the
/// margin to leave a decoder on, and the size is a reservation of address space rather than
/// memory that is committed, so the pool costs no more for having it.
const WORKER_STACK_SIZE: usize = 8 * 1024 * 1024;

/// The size of the connection worker pool for a given transform limit.
///
/// A worker is dedicated to a connection from accept to close, so the pool has to be larger
/// than the number of transforms that may run at once. Two things depend on that difference.
/// A request that is not a transform — a health probe, `/metrics`, a request the cache can
/// answer — needs a worker while every slot is taken, and a transform beyond the limit needs
/// one in order to reach [`TransformSlot::try_acquire`](super::handler::TransformSlot) and be
/// told to retry rather than waiting in the accept queue for a slot to free up.
///
/// Sizing the pool at the larger of the limit and [`WORKER_THREADS`] left that difference at
/// zero for any limit of `WORKER_THREADS` or more, and the limit defaults to the machine's
/// core count.
fn worker_pool_size(max_concurrent_transforms: u64) -> usize {
    usize::try_from(max_concurrent_transforms)
        .unwrap_or(usize::MAX)
        .saturating_add(WORKER_THREADS)
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
    // The instant of accept travels with the connection so that the wait for a worker is
    // charged to the request rather than lost: a request answered instantly by a worker that
    // took eleven seconds to reach it is not a request that took no time.
    let (sender, receiver) = std::sync::mpsc::channel::<(TcpStream, Instant)>();

    // Spawn a pool of worker threads sized to the configured concurrency limit plus
    // WORKER_THREADS of headroom for non-transform requests such as health checks and
    // metrics, and for the transform that is turned away with a 503.  Each thread pulls
    // connections from the shared channel and handles them independently, so a slow request
    // no longer blocks all other clients.
    let receiver = Arc::new(std::sync::Mutex::new(receiver));
    let pool_size = worker_pool_size(config.max_concurrent_transforms);
    let mut workers = Vec::with_capacity(pool_size);
    for _ in 0..pool_size {
        let rx = Arc::clone(&receiver);
        let cfg = Arc::clone(&config);
        let worker = std::thread::Builder::new()
            .name("truss-worker".into())
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                loop {
                    let (stream, accepted_at) = {
                        let guard = rx.lock().expect("worker lock poisoned");
                        match guard.recv() {
                            Ok(accepted) => accepted,
                            Err(_) => break,
                        }
                    }; // MutexGuard dropped here — before handle_stream runs.
                    if let Err(err) = handle_stream(stream, accepted_at, &cfg) {
                        cfg.log_warn(&format!("failed to handle connection: {err}"));
                    }
                }
            })
            .expect("failed to start a connection worker");
        workers.push(worker);
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

    // The drain deadline, set once the shutdown signal is observed. Until it
    // elapses the loop keeps accepting: a load balancer probes readiness over a
    // new connection, so a drain period that stops accepting cannot deliver the
    // 503 it exists for, and every request that arrives in the window is
    // parked in the accept backlog and answered by nobody.
    let mut drain_deadline: Option<Instant> = None;

    loop {
        let remaining = drain_deadline
            .map(|deadline: Instant| deadline.saturating_duration_since(Instant::now()));
        if remaining.is_some_and(|left| left.is_zero()) {
            break;
        }

        // Wait for activity on the listener or shutdown pipe. On Unix we use
        // poll(2) to block efficiently; on Windows we fall back to polling the
        // draining flag with a short sleep. While draining the wait is bounded
        // so the deadline is noticed even with no traffic at all.
        wait_for_accept_or_shutdown(&listener, shutdown_read_fd, &config.draining, remaining);

        // The shutdown pipe fires once; the draining flag is what stays set, and
        // it is also the only signal available on Windows.
        let signalled =
            poll_shutdown_pipe(shutdown_read_fd) || config.draining.load(Ordering::SeqCst);
        if signalled && drain_deadline.is_none() {
            let drain_secs = config.shutdown_drain_secs;
            config.log(&format!(
                "shutdown: drain started, waiting {drain_secs}s for load balancers"
            ));
            if drain_secs == 0 {
                break;
            }
            drain_deadline = Some(Instant::now() + Duration::from_secs(drain_secs));
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                // Accepted connections are always blocking for the workers.
                let _ = stream.set_nonblocking(false);
                if sender.send((stream, Instant::now())).is_err() {
                    break;
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // Spurious wakeup — retry.
            }
            Err(err) => return Err(err),
        }
    }

    config.log("shutdown: drain complete, closing listener");

    // Close the listener before draining the workers. Leaving it bound would put
    // the worker-drain window in the same position the shutdown drain used to be
    // in: connections completed by the kernel and accepted by nobody. Refusing is
    // what lets a client fail over instead of waiting.
    drop(listener);

    // Stop dispatching new connections to workers.
    drop(sender);
    // Worker drain deadline: 15s so that total shutdown (drain + worker drain)
    // fits within Kubernetes default terminationGracePeriodSeconds of 30s.
    let deadline = Instant::now() + Duration::from_secs(15);
    for worker in workers {
        let remaining = deadline.saturating_duration_since(Instant::now());
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
    // A console control handler for a close, a log-off or a system shutdown is holding the
    // process open until this is set; everything the drain was for has happened by now.
    #[cfg(windows)]
    SHUTDOWN_COMPLETE.store(true, Ordering::SeqCst);
    Ok(())
}

/// Serves exactly one request with an explicit server configuration.
///
/// # Errors
///
/// Returns an [`io::Error`] when accepting the next connection fails or when a response cannot
/// be written to the socket.
pub fn serve_once_with_config(listener: TcpListener, config: ServerConfig) -> io::Result<()> {
    let (stream, _) = listener.accept()?;
    let accepted_at = Instant::now();
    handle_stream(stream, accepted_at, &config)
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
    timeout: Option<Duration>,
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
    // Block indefinitely (-1 timeout) unless a drain deadline bounds the wait.
    // Signal delivery will interrupt with EINTR, which is fine — we just
    // re-check the shutdown conditions.
    let timeout_ms = match timeout {
        None => -1,
        Some(remaining) => i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX),
    };
    unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
}

#[cfg(windows)]
fn wait_for_accept_or_shutdown(
    _listener: &std::net::TcpListener,
    _shutdown_read_fd: i32,
    draining: &AtomicBool,
    timeout: Option<Duration>,
) {
    // On Windows, poll(2) is not available for the listener socket, so the loop
    // spins on a short sleep. Once the drain deadline is set the flag is already
    // true, so the remaining time is what decides whether to sleep at all.
    const NAP: Duration = Duration::from_millis(10);
    match timeout {
        Some(remaining) if remaining.is_zero() => {}
        Some(remaining) => std::thread::sleep(NAP.min(remaining)),
        None => {
            if !draining.load(Ordering::SeqCst) {
                std::thread::sleep(NAP);
            }
        }
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
#[cfg(unix)]
static SHUTDOWN_PIPE_WR: AtomicI32 = AtomicI32::new(-1);
/// Global draining flag set by the signal handler.
static GLOBAL_DRAINING: std::sync::atomic::AtomicPtr<AtomicBool> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());
/// Global log level, cycled by SIGUSR1 (Unix only).
#[cfg(unix)]
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

/// Set once the accept loop has finished draining, so a console control handler that the
/// operating system is timing knows it can let the process go.
#[cfg(windows)]
static SHUTDOWN_COMPLETE: AtomicBool = AtomicBool::new(false);

/// How long a close, log-off or shutdown handler waits for the drain before returning.
///
/// Returning from the handler for one of those three is what lets Windows terminate the
/// process, so the wait is the drain. Windows itself allows about five seconds before it kills
/// the process regardless — the exact figure is the `WaitToKillServiceTimeout` and
/// `HungAppTimeout` registry values — so waiting past that would only be waiting to be killed.
#[cfg(windows)]
const CONSOLE_CLOSE_DRAIN_BUDGET: Duration = Duration::from_secs(4);

#[cfg(windows)]
mod console_ctrl {
    //! The console control events Windows delivers in place of the signals it does not raise.
    //!
    //! `SIGTERM` is a constant the C runtime defines and the operating system never raises, so
    //! a handler for it never runs; `SIGINT` is raised, but only for Ctrl+C in a console, which
    //! left every other way of stopping the process terminating it without a drain. These are
    //! declared here rather than pulled in with a Windows binding crate, because the two
    //! functions and the five constants below are the whole of what the server needs.

    pub(super) type Bool = i32;
    pub(super) type Dword = u32;

    pub(super) const TRUE: Bool = 1;
    /// Says the handler did not handle the event, which passes it to the next one in the chain
    /// and, if none takes it, to the default terminator.
    pub(super) const FALSE: Bool = 0;

    /// Ctrl+C in a console. The process keeps running after the handler returns.
    pub(super) const CTRL_C_EVENT: Dword = 0;
    /// Ctrl+Break in a console, and what `GenerateConsoleCtrlEvent` sends to a process group.
    /// The process keeps running after the handler returns.
    pub(super) const CTRL_BREAK_EVENT: Dword = 1;
    /// The console window was closed. Windows terminates the process when the handler returns.
    pub(super) const CTRL_CLOSE_EVENT: Dword = 2;
    /// The user is logging off. Windows terminates the process when the handler returns.
    pub(super) const CTRL_LOGOFF_EVENT: Dword = 5;
    /// The system is shutting down. Windows terminates the process when the handler returns.
    pub(super) const CTRL_SHUTDOWN_EVENT: Dword = 6;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(Dword) -> Bool>,
            add: Bool,
        ) -> Bool;
    }
}

#[cfg(windows)]
fn install_signal_handler(draining: Arc<AtomicBool>, _write_fd: i32, _log_level: Arc<AtomicU8>) {
    // Store the draining pointer in the global so the control handler can set it.
    let ptr = Arc::into_raw(draining).cast_mut();
    GLOBAL_DRAINING.store(ptr, Ordering::SeqCst);

    // SAFETY: the handler is a plain `extern "system"` function with no state of its own; the
    // pointer above is leaked for the process lifetime, which is what makes reading it in the
    // handler sound.
    unsafe {
        console_ctrl::SetConsoleCtrlHandler(Some(console_ctrl_handler), console_ctrl::TRUE);
    }
}

/// Starts the drain for every console control event Windows delivers.
///
/// Returning `TRUE` says the event is handled, which for Ctrl+C and Ctrl+Break stops the
/// default terminator and lets the accept loop drain on its own schedule. For a window close, a
/// log-off and a system shutdown, returning is itself what lets Windows terminate the process,
/// so the handler waits for the drain instead of returning immediately, up to the budget the
/// operating system allows before it stops asking.
#[cfg(windows)]
unsafe extern "system" fn console_ctrl_handler(event: console_ctrl::Dword) -> console_ctrl::Bool {
    let terminating = match event {
        console_ctrl::CTRL_C_EVENT | console_ctrl::CTRL_BREAK_EVENT => false,
        console_ctrl::CTRL_CLOSE_EVENT
        | console_ctrl::CTRL_LOGOFF_EVENT
        | console_ctrl::CTRL_SHUTDOWN_EVENT => true,
        // An event this does not know is one for another handler to answer.
        _ => return console_ctrl::FALSE,
    };

    let ptr = GLOBAL_DRAINING.load(Ordering::SeqCst);
    if !ptr.is_null() {
        unsafe { (*ptr).store(true, Ordering::SeqCst) };
    }

    if terminating {
        let deadline = Instant::now() + CONSOLE_CLOSE_DRAIN_BUDGET;
        while !SHUTDOWN_COMPLETE.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    console_ctrl::TRUE
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use serial_test::serial;

    #[cfg(unix)]
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::sync::atomic::AtomicU8;

    #[cfg(unix)]
    struct ShutdownSignalGuard {
        previous_draining: *mut AtomicBool,
        previous_write_fd: i32,
        draining: *mut AtomicBool,
        read_fd: i32,
        write_fd: i32,
    }

    #[cfg(unix)]
    impl Drop for ShutdownSignalGuard {
        fn drop(&mut self) {
            GLOBAL_DRAINING.store(self.previous_draining, Ordering::SeqCst);
            SHUTDOWN_PIPE_WR.store(self.previous_write_fd, Ordering::SeqCst);
            close_shutdown_pipe(self.read_fd, self.write_fd);
            // SAFETY: `draining` was allocated with Box::into_raw in the test setup above.
            unsafe { drop(Box::from_raw(self.draining)) };
        }
    }

    #[cfg(unix)]
    struct LogLevelGuard {
        previous: *mut AtomicU8,
        log_level: *mut AtomicU8,
    }

    #[cfg(unix)]
    impl Drop for LogLevelGuard {
        fn drop(&mut self) {
            GLOBAL_LOG_LEVEL.store(self.previous, Ordering::SeqCst);
            // SAFETY: `log_level` was allocated with Box::into_raw in the test setup above.
            unsafe { drop(Box::from_raw(self.log_level)) };
        }
    }

    /// The pool has to exceed the transform limit at every limit, not only at small ones.
    ///
    /// Sizing it at the larger of the two left no worker for a health probe, and no worker
    /// for the transform that should have been answered 503, whenever the limit reached
    /// `WORKER_THREADS`. The limit defaults to the core count, so that was every ordinary
    /// server.
    #[test]
    fn the_worker_pool_exceeds_the_transform_limit_at_every_limit() {
        for limit in [1_u64, 2, 4, 7, 8, 9, 32, 1024] {
            let pool = worker_pool_size(limit);
            assert!(
                pool >= limit as usize + WORKER_THREADS,
                "limit {limit} sized the pool at {pool}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn wait_for_accept_or_shutdown_returns_when_pipe_is_ready() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let (read_fd, write_fd) = create_shutdown_pipe().expect("create shutdown pipe");
        let byte: u8 = 1;
        let written = unsafe { libc::write(write_fd, (&byte as *const u8).cast(), 1) };
        assert_eq!(written, 1, "write shutdown wakeup byte");

        let draining = AtomicBool::new(false);
        let start = Instant::now();
        wait_for_accept_or_shutdown(&listener, read_fd, &draining, None);

        assert!(
            start.elapsed() < Duration::from_millis(200),
            "wait_for_accept_or_shutdown should return immediately when the pipe is readable"
        );
        assert!(poll_shutdown_pipe(read_fd));

        close_shutdown_pipe(read_fd, write_fd);
    }

    /// During the drain window the wait has to be bounded, otherwise the loop
    /// never notices the deadline on a server with no traffic.
    #[cfg(unix)]
    #[test]
    fn wait_for_accept_or_shutdown_honours_the_drain_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let (read_fd, write_fd) = create_shutdown_pipe().expect("create shutdown pipe");

        let draining = AtomicBool::new(true);
        let start = Instant::now();
        wait_for_accept_or_shutdown(
            &listener,
            read_fd,
            &draining,
            Some(Duration::from_millis(50)),
        );

        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a bounded wait must return without traffic on the listener"
        );

        close_shutdown_pipe(read_fd, write_fd);
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn signal_handler_sets_draining_and_wakes_shutdown_pipe() {
        let (read_fd, write_fd) = create_shutdown_pipe().expect("create shutdown pipe");
        let draining = Box::into_raw(Box::new(AtomicBool::new(false)));
        let previous_draining = GLOBAL_DRAINING.swap(draining, Ordering::SeqCst);
        let previous_write_fd = SHUTDOWN_PIPE_WR.swap(write_fd, Ordering::SeqCst);
        let _restore = ShutdownSignalGuard {
            previous_draining,
            previous_write_fd,
            draining,
            read_fd,
            write_fd,
        };

        signal_handler(libc::SIGTERM);

        assert!(unsafe { &*draining }.load(Ordering::SeqCst));
        assert!(poll_shutdown_pipe(read_fd));
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn sigusr1_handler_cycles_global_log_level() {
        let log_level = Box::into_raw(Box::new(AtomicU8::new(LogLevel::Info as u8)));
        let previous = GLOBAL_LOG_LEVEL.swap(log_level, Ordering::SeqCst);
        let _restore = LogLevelGuard {
            previous,
            log_level,
        };

        sigusr1_handler(libc::SIGUSR1);

        assert_eq!(
            unsafe { &*log_level }.load(Ordering::SeqCst),
            LogLevel::Debug as u8
        );
    }
}
