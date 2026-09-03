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

/// A method a route does not serve is 405 with `Allow`, and a path truss does not have is
/// still 404, so the two cases a caller has to tell apart are told apart.
///
/// A wrong method used to answer with the same 404 body as a path that is not there, which
/// is the answer that makes a probe stop trying rather than correct itself.
#[rstest]
#[case::post_by_path("POST", "/images/by-path", "GET, HEAD, OPTIONS")]
#[case::put_by_url("PUT", "/images/by-url", "GET, HEAD, OPTIONS")]
#[case::delete_metrics("DELETE", "/metrics", "GET, HEAD, OPTIONS")]
#[case::patch_health_live("PATCH", "/health/live", "GET, HEAD, OPTIONS")]
#[case::get_transform("GET", "/images:transform", "POST, OPTIONS")]
#[case::get_upload("GET", "/images", "POST, OPTIONS")]
fn a_method_a_route_does_not_serve_is_405_with_allow(
    #[case] method: &str,
    #[case] path: &str,
    #[case] expected_allow: &str,
) {
    let storage_root = temp_dir("method-not-allowed");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        &format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 405 Method Not Allowed"),
        "{header}"
    );
    assert!(
        header.contains(&format!("Allow: {expected_allow}")),
        "{header}"
    );
    assert_eq!(content_type, "application/problem+json");
    let problem: serde_json::Value =
        serde_json::from_slice(&body).expect("the 405 body is a problem document");
    assert_eq!(problem["status"], 405);
    assert!(
        problem["type"]
            .as_str()
            .expect("type is a string")
            .ends_with("#method-not-allowed"),
        "{problem}"
    );
}

#[test]
fn a_path_truss_does_not_serve_is_still_404() {
    let storage_root = temp_dir("unknown-path");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        "DELETE /nope HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(header.starts_with("HTTP/1.1 404 Not Found"), "{header}");
    assert!(!header.contains("Allow:"), "{header}");
}

/// An `OPTIONS` probe is what a browser, a CDN and an uptime monitor send before they fetch.
/// truss serves no CORS headers, so `Allow` is the whole of what it has to report.
#[rstest]
#[case::by_path("/images/by-path", "GET, HEAD, OPTIONS")]
#[case::transform("/images:transform", "POST, OPTIONS")]
#[case::health("/health", "GET, HEAD, OPTIONS")]
fn options_on_a_route_answers_with_allow(#[case] path: &str, #[case] expected_allow: &str) {
    let storage_root = temp_dir("options-probe");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, None));
    let response = send_raw_request(
        addr,
        &format!(
            "OPTIONS {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    assert!(header.starts_with("HTTP/1.1 204 No Content"), "{header}");
    assert!(
        header.contains(&format!("Allow: {expected_allow}")),
        "{header}"
    );
    assert!(body.is_empty(), "an OPTIONS answer carries no body");
}
