mod common;

use common::{
    png_bytes, send_raw_request, signed_target_with_method, spawn_fixture_server, spawn_server,
    split_response, temp_dir,
};
use rstest::rstest;
use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use truss::ServerConfig;

const FIXED_HEAD_BY_PATH_TARGET: &str = "/images/by-path?expires=1900000000&format=webp&keyId=public-demo&path=image.png&signature=29332b4813792a5982ed3071633a26407bd5335654f7a10e11729e75f545dc5a&width=800";

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

fn send_head_request(addr: SocketAddr, target: &str, host: &str) -> Vec<u8> {
    send_raw_request(
        addr,
        &format!("HEAD {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"),
    )
}

#[test]
fn head_public_by_path_returns_headers_with_empty_body() {
    let storage_root = temp_dir("head-public-by-path");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-demo", "secret-value"),
    );
    let response = send_head_request(addr, FIXED_HEAD_BY_PATH_TARGET, "images.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    assert!(header.starts_with("HTTP/1.1 200 OK"), "{header}");
    assert_eq!(content_type, "image/webp");
    assert!(body.is_empty(), "HEAD response body must be empty");
    assert!(header.contains("ETag: \"sha256-"), "{header}");
    assert!(
        header.contains("Cache-Control: public, max-age=3600, stale-while-revalidate=60"),
        "{header}"
    );
}

#[test]
fn head_public_by_url_returns_headers_with_empty_body() {
    let storage_root = temp_dir("head-public-by-url");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value")
            .with_insecure_url_sources(true),
    );
    let target = signed_target_with_method(
        "HEAD",
        "/images/by-url",
        BTreeMap::from([
            ("url".to_string(), url),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_head_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    assert!(header.starts_with("HTTP/1.1 200 OK"), "{header}");
    assert_eq!(content_type, "image/jpeg");
    assert!(body.is_empty(), "HEAD response body must be empty");
    assert!(header.contains("ETag: \"sha256-"), "{header}");
    assert!(
        header.contains("Cache-Control: public, max-age=3600, stale-while-revalidate=60"),
        "{header}"
    );
}
