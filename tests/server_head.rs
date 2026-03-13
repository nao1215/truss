mod common;

use common::{send_raw_request, spawn_server, split_response, temp_dir};
use rstest::rstest;
use truss::ServerConfig;

#[rstest]
#[case::health_live("/health/live", 200)]
#[case::health_ready("/health/ready", 200)]
#[case::metrics("/metrics", 200)]
#[case::unknown_route("/nonexistent", 404)]
fn head_request_returns_expected_status_with_empty_body(
    #[case] path: &str,
    #[case] expected_status: u16,
) {
    let storage_root = temp_dir("head-test");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        &format!("HEAD {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    assert!(
        header.starts_with(&format!("HTTP/1.1 {expected_status}")),
        "expected {expected_status}, got: {header}"
    );
    assert!(body.is_empty(), "HEAD response body must be empty");
}
