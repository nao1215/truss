//! Server lifecycle integration tests.
//!
//! These cover the parts of `serve_with_config` that `serve_once_with_config` cannot reach,
//! which is the worker pool and the shutdown sequence:
//! - A request that is not a transform is answered while every transform slot is taken.
//! - A transform beyond the limit is answered 503 rather than made to wait.
//! - The `draining` flag causes `/health/ready` to return 503.
//! - In-flight requests complete before the server exits.
//! - The `serve_with_config` function terminates when a signal is received.

mod common;

use common::{large_png_bytes, split_response, status_code, temp_dir};
use serial_test::serial;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use truss::{ServerConfig, serve_once_with_config, serve_with_config};

fn send_health_ready(addr: SocketAddr) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write request");
    stream.flush().expect("flush");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn send_health_live(addr: SocketAddr) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write request");
    stream.flush().expect("flush");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

/// When the draining flag is NOT set, `/health/ready` returns 200.
#[test]
fn health_ready_returns_200_when_not_draining() {
    let storage = temp_dir("shutdown-ready-200");
    let config = ServerConfig::new(storage, None);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let handle = thread::spawn(move || serve_once_with_config(listener, config));

    let response = send_health_ready(addr);
    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200, got: {header}"
    );

    handle.join().expect("server thread").expect("serve_once");
}

/// When the draining flag IS set, `/health/ready` returns 503.
#[test]
fn health_ready_returns_503_when_draining() {
    let storage = temp_dir("shutdown-ready-503");
    let config = ServerConfig::new(storage, None);

    // Set draining before starting the server.
    config.draining.store(true, Ordering::SeqCst);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let handle = thread::spawn(move || serve_once_with_config(listener, config));

    let response = send_health_ready(addr);
    let (header, _, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 503"),
        "expected 503, got: {header}"
    );
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("draining"),
        "response should mention draining: {body_str}"
    );

    handle.join().expect("server thread").expect("serve_once");
}

/// `/health/live` still returns 200 even when draining — liveness is always
/// reported as long as the process is running.
#[test]
fn health_live_returns_200_when_draining() {
    let storage = temp_dir("shutdown-live-200");
    let config = ServerConfig::new(storage, None);
    config.draining.store(true, Ordering::SeqCst);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let handle = thread::spawn(move || serve_once_with_config(listener, config));

    let response = send_health_live(addr);
    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200 for liveness, got: {header}"
    );

    handle.join().expect("server thread").expect("serve_once");
}

/// `serve_with_config` exits when the draining flag is set externally.
/// This tests the accept-loop shutdown path by setting `draining` from
/// another thread after the server starts listening.
#[cfg(unix)]
#[test]
#[serial]
fn serve_with_config_exits_on_draining_flag() {
    let storage = temp_dir("shutdown-exit");
    let config = ServerConfig::new(storage, None);
    // Use a zero drain period so shutdown is immediate after signal.
    let mut config = config;
    config.shutdown_drain_secs = 0;

    let draining = Arc::clone(&config.draining);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let _addr = listener.local_addr().expect("addr");

    let handle = thread::spawn(move || serve_with_config(listener, config));

    // Give the server time to start its accept loop.
    thread::sleep(Duration::from_millis(100));

    // Simulate a shutdown signal by sending SIGTERM to ourselves.
    // The installed signal handler will set the draining flag and wake the
    // accept loop via the self-pipe.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGTERM);
    }

    // The server should exit within a reasonable time.
    let result = handle.join().expect("server thread should not panic");
    assert!(
        result.is_ok(),
        "serve_with_config should return Ok on graceful shutdown"
    );

    // Verify the draining flag was set.
    assert!(
        draining.load(Ordering::SeqCst),
        "draining flag should be true after shutdown"
    );
}

/// An in-flight request completes even after the draining flag is set.
/// We start `serve_with_config`, send a request, set draining while the
/// connection is open, and verify the client still receives a valid response.
#[test]
#[serial]
fn in_flight_request_completes_during_drain() {
    let storage = temp_dir("shutdown-inflight");
    let mut config = ServerConfig::new(storage, None);
    config.shutdown_drain_secs = 0;

    let draining = Arc::clone(&config.draining);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let handle = thread::spawn(move || serve_with_config(listener, config));

    // Give the server time to start.
    thread::sleep(Duration::from_millis(100));

    // Send a health check request.
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set timeout");
    stream
        .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write request");
    stream.flush().expect("flush");

    // Read the response — should complete normally.
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "in-flight request should complete successfully: {header}"
    );

    // Set draining flag directly to trigger server shutdown.
    draining.store(true, Ordering::SeqCst);

    // Connect once more so the accept loop wakes up and notices the flag.
    let _ = TcpStream::connect(addr);

    handle
        .join()
        .expect("server thread")
        .expect("serve_with_config");
}

/// Reads one response with a bounded wait, returning `None` when the server
/// accepted the connection but wrote nothing before the deadline. A plain
/// `read_to_end` cannot tell that case apart from a slow answer, and it is
/// exactly the case these tests exist to catch.
#[cfg(unix)]
fn probe_with_deadline(addr: SocketAddr, path: &str, wait: Duration) -> Option<String> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(wait)).expect("set timeout");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .ok()?;
    stream.flush().ok()?;

    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    if response.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&response).into_owned())
}

/// A load balancer probes readiness over a new connection, so the drain period
/// is only useful if new connections are still served while it runs.
#[cfg(unix)]
#[test]
#[serial]
fn health_ready_answers_503_on_a_new_connection_during_the_drain_window() {
    let storage = temp_dir("drain-ready-503");
    let mut config = ServerConfig::new(storage, None);
    config.shutdown_drain_secs = 4;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || serve_with_config(listener, config));

    thread::sleep(Duration::from_millis(200));
    unsafe {
        libc::kill(libc::getpid(), libc::SIGTERM);
    }
    thread::sleep(Duration::from_millis(300));

    let ready = probe_with_deadline(addr, "/health/ready", Duration::from_secs(1))
        .expect("readiness probe during the drain window must be answered, not left hanging");
    assert!(
        ready.starts_with("HTTP/1.1 503"),
        "expected 503 while draining, got: {ready}"
    );

    let live = probe_with_deadline(addr, "/health/live", Duration::from_secs(1))
        .expect("liveness probe during the drain window must be answered");
    assert!(
        live.starts_with("HTTP/1.1 200"),
        "expected 200 for liveness while draining, got: {live}"
    );

    handle
        .join()
        .expect("server thread")
        .expect("serve_with_config");
}

/// The drain window has to end, and the listener with it. The assertion runs after
/// `serve_with_config` has returned, so what it pins is that the listener is gone by
/// then rather than left bound — a refused connection is what tells a client to fail
/// over instead of waiting.
#[cfg(unix)]
#[test]
#[serial]
fn connections_are_refused_once_the_drain_window_has_elapsed() {
    let storage = temp_dir("drain-window-ends");
    let mut config = ServerConfig::new(storage, None);
    config.shutdown_drain_secs = 1;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || serve_with_config(listener, config));

    thread::sleep(Duration::from_millis(200));
    unsafe {
        libc::kill(libc::getpid(), libc::SIGTERM);
    }

    handle
        .join()
        .expect("server thread")
        .expect("serve_with_config");

    assert!(
        TcpStream::connect(addr).is_err(),
        "the listener must be closed once the drain window has elapsed"
    );
}

/// Opens a connection, sends a request line and one header, and leaves it unterminated so
/// the worker stays in the header phase until the connection is dropped.
///
/// This holds a worker without spending any CPU, which is what makes the pool-size
/// assertions below deterministic on a loaded CI machine.
fn hold_a_worker(addr: SocketAddr) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\n")
        .expect("write partial request");
    stream.flush().expect("flush");
    stream
}

/// A health probe is answered while every worker a transform could occupy is busy.
///
/// The pool used to be `max(max_concurrent_transforms, WORKER_THREADS)`, which is the
/// transform limit itself for any limit of eight or more, and the limit defaults to the
/// machine's core count. Eight busy connections then left nothing to read the probe's
/// socket, so `/health/live` answered only once a transform had finished — measured at
/// eleven seconds against a limit of eight on a 32-core machine, which a liveness probe
/// reads as a dead process.
#[cfg(unix)]
#[test]
#[serial]
fn a_health_probe_is_answered_while_every_transform_worker_is_busy() {
    let storage = temp_dir("pool-health-headroom");
    let mut config = ServerConfig::new(storage, None);
    config.max_concurrent_transforms = 8;
    config.shutdown_drain_secs = 0;

    let draining = Arc::clone(&config.draining);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || serve_with_config(listener, config));
    thread::sleep(Duration::from_millis(200));

    let held: Vec<TcpStream> = (0..8).map(|_| hold_a_worker(addr)).collect();
    thread::sleep(Duration::from_millis(200));

    let live = probe_with_deadline(addr, "/health/live", Duration::from_secs(2)).expect(
        "a liveness probe must not wait behind the transform workers for the header deadline",
    );
    assert!(
        live.starts_with("HTTP/1.1 200"),
        "expected 200 for liveness, got: {live}"
    );

    drop(held);
    draining.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(addr);
    handle
        .join()
        .expect("server thread")
        .expect("serve_with_config");
}

/// A transform beyond the limit is told to retry rather than made to wait for a slot.
///
/// `try_acquire` runs inside a worker, so with as many workers as slots a request could not
/// reach it until a slot was already free and the 503 never fired. Twenty-four concurrent
/// transforms against eight slots answered twenty-four times 200, the excess after up to
/// 12.4 seconds; the same burst against seven slots, one below `WORKER_THREADS`, answered
/// seventeen times 503 in about five milliseconds each.
#[cfg(unix)]
#[test]
#[serial]
fn a_transform_beyond_the_limit_is_refused_rather_than_queued() {
    let storage = temp_dir("pool-transform-503");
    std::fs::write(storage.join("source.png"), large_png_bytes()).expect("write source");
    let mut config = ServerConfig::new(storage, Some("test-token".to_string()));
    config.max_concurrent_transforms = 8;
    config.shutdown_drain_secs = 0;

    let draining = Arc::clone(&config.draining);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || serve_with_config(listener, config));
    thread::sleep(Duration::from_millis(200));

    // Every request asks for a different width so none of them shares an answer, and the
    // output is large enough that a slot is held for longer than it takes the rest of the
    // burst to arrive.
    let workers: Vec<_> = (0..24)
        .map(|i| {
            thread::spawn(move || {
                let body = format!(
                    r#"{{"source":{{"kind":"path","path":"source.png"}},"options":{{"width":{},"height":2000,"fit":"fill","format":"png"}}}}"#,
                    1900 + i
                );
                let response = common::send_transform_request(addr, &body, Some("test-token"));
                let (header, _, _) = split_response(&response);
                status_code(&header)
            })
        })
        .collect();

    let statuses: Vec<u16> = workers
        .into_iter()
        .map(|worker| worker.join().expect("request thread"))
        .collect();

    let refused = statuses.iter().filter(|status| **status == 503).count();
    assert!(
        refused > 0,
        "a burst of 24 transforms against 8 slots must refuse some of them, got {statuses:?}"
    );
    assert!(
        statuses.iter().filter(|status| **status == 200).count() > 0,
        "the slots that exist must still be used, got {statuses:?}"
    );

    draining.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(addr);
    handle
        .join()
        .expect("server thread")
        .expect("serve_with_config");
}

/// The wait for a worker is charged to the request that waited.
///
/// `latency_ms` and `truss_http_request_duration_seconds` both start from the same instant,
/// and it used to be the moment the request's headers finished parsing, which is after the
/// connection has left the accept queue. Three probes a client measured at 10.97 seconds
/// were logged as `"latency_ms":0`, so the backlog a saturated server actually has was
/// invisible in the only two places an operator looks.
#[cfg(unix)]
#[test]
#[serial]
fn a_request_that_waited_for_a_worker_is_logged_with_the_wait() {
    let storage = temp_dir("pool-latency-queue");
    let mut config = ServerConfig::new(storage, None);
    config.max_concurrent_transforms = 1;
    config.shutdown_drain_secs = 0;

    let lines: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    config.log_handler = Some(Arc::new(move |msg: &str| {
        sink.lock().expect("log sink").push(msg.to_string());
    }));

    let draining = Arc::clone(&config.draining);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || serve_with_config(listener, config));
    thread::sleep(Duration::from_millis(200));

    // One more connection than the pool holds, so the probe below finds no free worker.
    let held: Vec<TcpStream> = (0..16).map(|_| hold_a_worker(addr)).collect();
    thread::sleep(Duration::from_millis(200));

    let probe =
        thread::spawn(move || probe_with_deadline(addr, "/health/live", Duration::from_secs(10)));
    thread::sleep(Duration::from_millis(700));
    drop(held);

    let live = probe
        .join()
        .expect("probe thread")
        .expect("the probe must be answered once a worker frees up");
    assert!(
        live.starts_with("HTTP/1.1 200"),
        "expected 200 for liveness, got: {live}"
    );

    let logged: Vec<String> = lines
        .lock()
        .expect("log sink")
        .iter()
        .filter(|line| line.contains("\"path\":\"/health/live\""))
        .cloned()
        .collect();
    let entry = logged
        .last()
        .expect("the liveness probe must appear in the access log");
    let latency: u64 = entry
        .split("\"latency_ms\":")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| panic!("no latency_ms in {entry}"));
    assert!(
        latency >= 400,
        "a probe that waited about 700ms for a worker was logged as {latency}ms: {entry}"
    );

    draining.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(addr);
    handle
        .join()
        .expect("server thread")
        .expect("serve_with_config");
}
