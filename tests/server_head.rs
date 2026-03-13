mod common;

use common::{send_raw_request, spawn_server, split_response, temp_dir};
use truss::ServerConfig;

#[test]
fn head_health_live_returns_200_with_empty_body() {
    let storage_root = temp_dir("head-health-live");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        "HEAD /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200, got: {header}"
    );
    assert!(body.is_empty(), "HEAD response body must be empty");
}

#[test]
fn head_health_ready_returns_200_with_empty_body() {
    let storage_root = temp_dir("head-health-ready");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        "HEAD /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200, got: {header}"
    );
    assert!(body.is_empty(), "HEAD response body must be empty");
}

#[test]
fn head_metrics_returns_200_with_empty_body() {
    let storage_root = temp_dir("head-metrics");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        "HEAD /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200, got: {header}"
    );
    assert!(body.is_empty(), "HEAD response body must be empty");
}

#[test]
fn head_unknown_route_returns_404_with_empty_body() {
    let storage_root = temp_dir("head-unknown");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        "HEAD /nonexistent HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 404"),
        "expected 404, got: {header}"
    );
    assert!(body.is_empty(), "HEAD response body must be empty");
}
