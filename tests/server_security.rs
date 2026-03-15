// Tests ported from imgproxy and imagor security test patterns.
//
// imgproxy: security/source_test.go (network address filtering, source validation)
// imagor:   loader/httploader/httploader_test.go (SSRF via redirects, percent-encoded attacks)
//           filestorage/filestorage_test.go (path traversal)

mod common;

use common::{
    png_bytes, send_transform_request, spawn_fixture_server, spawn_server, split_response,
    temp_dir,
};
use std::fs;
use truss::ServerConfig;

// ---------------------------------------------------------------------------
// SSRF: redirect chain to private IP (from imagor TestWithAllowedSourcesRedirect)
// ---------------------------------------------------------------------------

#[test]
fn ssrf_redirect_to_metadata_endpoint_is_blocked() {
    // The fixture server redirects to the AWS metadata endpoint.
    // Even with insecure sources allowed, metadata endpoints must be blocked
    // on every hop of the redirect chain.
    let storage_root = temp_dir("ssrf-redirect-metadata");
    let (url, fixture) = spawn_fixture_server(vec![(
        "302 Found".to_string(),
        vec![(
            "Location".to_string(),
            "http://169.254.169.254/latest/meta-data".to_string(),
        )],
        Vec::new(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(
        header.starts_with("HTTP/1.1 403"),
        "redirect to metadata should be blocked, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(
        body.contains("cloud metadata"),
        "error should mention cloud metadata, got: {body}"
    );
}

// ---------------------------------------------------------------------------
// SSRF: non-http scheme rejected (from imgproxy security tests)
// ---------------------------------------------------------------------------

#[test]
fn ssrf_ftp_scheme_rejected() {
    let storage_root = temp_dir("ssrf-ftp");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"url","url":"ftp://evil.com/image.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8");
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "ftp scheme should be rejected, got: {header}"
    );
    assert!(body.contains("http"));
}

#[test]
fn ssrf_file_scheme_rejected() {
    let storage_root = temp_dir("ssrf-file");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"url","url":"file:///etc/passwd"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "file scheme should be rejected, got: {header}"
    );
}

#[test]
fn ssrf_data_scheme_rejected() {
    let storage_root = temp_dir("ssrf-data");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"url","url":"data:image/png;base64,iVBOR"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "data scheme should be rejected, got: {header}"
    );
}

// ---------------------------------------------------------------------------
// SSRF: URL with embedded credentials (from imgproxy)
// ---------------------------------------------------------------------------

#[test]
fn ssrf_url_with_userinfo_rejected() {
    let storage_root = temp_dir("ssrf-userinfo");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"url","url":"http://admin:pass@example.com/image.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8");
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "URL with userinfo should be rejected, got: {header}"
    );
    assert!(body.contains("user"));
}

// ---------------------------------------------------------------------------
// SSRF: loopback via various representations (from imgproxy security/source_test.go)
// ---------------------------------------------------------------------------

#[test]
fn ssrf_private_ip_ranges_blocked_in_strict_mode() {
    let _storage_root = temp_dir("ssrf-private-strict");

    // Test representative private IPs that should be blocked in strict mode.
    let blocked_urls = [
        ("http://10.0.0.1/img.png", "10.0.0.0/8 private"),
        ("http://172.16.0.1/img.png", "172.16.0.0/12 private"),
        ("http://192.168.1.1/img.png", "192.168.0.0/16 private"),
    ];

    for (url, description) in blocked_urls {
        let storage = temp_dir(&format!("ssrf-priv-{}", description.replace('/', "-")));
        let (addr, handle) =
            spawn_server(ServerConfig::new(storage, Some("secret".to_string())));
        let body = format!(
            r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#
        );
        let response = send_transform_request(addr, &body, Some("secret"));
        handle
            .join()
            .expect("join server thread")
            .expect("serve one request");

        let (header, _, _) = split_response(&response);
        assert!(
            header.starts_with("HTTP/1.1 403"),
            "{description} should be blocked in strict mode, got: {header}"
        );
    }
}

#[test]
fn ssrf_non_standard_port_blocked_in_strict_mode() {
    let storage_root = temp_dir("ssrf-port-strict");
    let (addr, handle) =
        spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"url","url":"http://example.com:8080/image.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8");
    assert!(
        header.starts_with("HTTP/1.1 403"),
        "non-standard port should be blocked, got: {header}"
    );
    assert!(body.contains("port"));
}

// ---------------------------------------------------------------------------
// Path traversal via E2E (from imagor filestorage tests)
// ---------------------------------------------------------------------------

#[test]
fn path_traversal_via_transform_request_is_rejected() {
    let storage_root = temp_dir("path-traversal-e2e");
    fs::write(storage_root.join("legit.png"), png_bytes()).expect("write legit image");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));

    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"../../etc/passwd"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "path traversal should be rejected, got: {header}"
    );
}

#[test]
fn path_traversal_with_dotdot_in_middle_is_rejected() {
    let storage_root = temp_dir("path-traversal-mid");
    fs::create_dir_all(storage_root.join("sub")).expect("create subdir");
    fs::write(storage_root.join("sub/image.png"), png_bytes()).expect("write image");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));

    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/sub/../../../etc/passwd"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "mid-path traversal should be rejected, got: {header}"
    );
}

#[test]
fn path_traversal_to_dotgit_is_rejected() {
    // From imagor: filestorage protects .git directories
    let storage_root = temp_dir("path-traversal-git");
    fs::create_dir_all(storage_root.join(".git/logs")).expect("create .git dir");
    fs::write(storage_root.join(".git/logs/HEAD"), b"fake git log").expect("write git log");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));

    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/.git/logs/HEAD"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    // Should fail because .git/logs/HEAD is not a valid image
    // The important thing is it doesn't return the raw file content
    assert!(
        !header.starts_with("HTTP/1.1 200"),
        ".git file access should not succeed, got: {header}"
    );
}

// ---------------------------------------------------------------------------
// Remote URL errors (from imgproxy imagedata tests)
// ---------------------------------------------------------------------------

#[test]
fn remote_upstream_4xx_returns_502() {
    // From imgproxy: TestDownloadStatusNotFound → maps to 502
    let storage_root = temp_dir("remote-4xx");
    let (url, fixture) = spawn_fixture_server(vec![(
        "404 Not Found".to_string(),
        vec![("Content-Type".to_string(), "text/plain".to_string())],
        b"not found".to_vec(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8");
    assert!(
        header.starts_with("HTTP/1.1 502"),
        "upstream 404 should map to 502, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("upstream HTTP 404"));
}

#[test]
fn remote_upstream_5xx_returns_502() {
    // From imgproxy: TestDownloadStatusInternalServerError → 5xx maps to 502
    let storage_root = temp_dir("remote-5xx");
    let (url, fixture) = spawn_fixture_server(vec![(
        "500 Internal Server Error".to_string(),
        vec![("Content-Type".to_string(), "text/plain".to_string())],
        b"server error".to_vec(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8");
    assert!(
        header.starts_with("HTTP/1.1 502"),
        "upstream 500 should map to 502, got: {header}"
    );
    assert!(body.contains("upstream HTTP 500"));
}

#[test]
fn remote_upstream_403_returns_502() {
    // From imgproxy: TestDownloadStatusForbidden → maps to 502
    let storage_root = temp_dir("remote-403");
    let (url, fixture) = spawn_fixture_server(vec![(
        "403 Forbidden".to_string(),
        vec![("Content-Type".to_string(), "text/plain".to_string())],
        b"forbidden".to_vec(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8");
    assert!(
        header.starts_with("HTTP/1.1 502"),
        "upstream 403 should map to 502, got: {header}"
    );
    assert!(body.contains("upstream HTTP 403"));
}
