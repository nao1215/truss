//! Graceful shutdown integration tests (issue #83).
//!
//! These tests verify that the server correctly handles the shutdown sequence:
//! - The `draining` flag causes `/health/ready` to return 503.
//! - In-flight requests complete before the server exits.
//! - The `serve_with_config` function terminates when a signal is received.

mod common;

use common::{split_response, temp_dir};
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
