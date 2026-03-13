mod common;

use common::{
    png_bytes, send_metrics_request, send_raw_request, send_transform_request,
    spawn_fixture_server, spawn_server, split_response, temp_dir,
};
use serial_test::serial;
use std::fs;
use truss::{MediaType, RawArtifact, ServerConfig, sniff_artifact};

#[test]
fn serve_once_transforms_a_path_source_over_http() {
    let storage_root = temp_dir("success");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/image.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(artifact.media_type, MediaType::Jpeg);
    assert_eq!(artifact.metadata.width, Some(4));
    assert_eq!(artifact.metadata.height, Some(3));
}

#[test]
fn serve_once_rejects_private_url_sources_by_default() {
    let storage_root = temp_dir("url-blocked");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"url","url":"http://127.0.0.1:8080/image.png"}}"#,
        Some("secret"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 403 Forbidden"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("port is not allowed"));
}

#[test]
fn serve_once_transforms_a_url_source_when_insecure_allowance_is_enabled() {
    let storage_root = temp_dir("url-success");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        "{{\"source\":{{\"kind\":\"url\",\"url\":\"{url}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn serve_once_follows_remote_redirects_for_url_sources() {
    let storage_root = temp_dir("url-redirect");
    let (url, fixture) = spawn_fixture_server(vec![
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
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let response = send_transform_request(
        addr,
        &format!(r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#),
        Some("secret"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn serve_once_rejects_unsupported_remote_content_encoding() {
    let storage_root = temp_dir("encoding");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![
            ("Content-Type".to_string(), "image/png".to_string()),
            ("Content-Encoding".to_string(), "compress".to_string()),
        ],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        "{{\"source\":{{\"kind\":\"url\",\"url\":\"{url}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 502 Bad Gateway"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("unsupported content-encoding"));
}

#[test]
fn serve_once_rejects_oversized_output_with_413() {
    let storage_root = temp_dir("limit-exceeded");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    // 8193 * 8192 = 67_116_032 > MAX_OUTPUT_PIXELS (67_108_864)
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/image.png"},"options":{"width":8193,"height":8192}}"#,
        Some("secret"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 413 Payload Too Large"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("output image"));
    assert!(body.contains("limit"));
}

#[test]
fn serve_once_exposes_metrics_with_bearer_auth() {
    let storage_root = temp_dir("metrics");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_metrics_request(addr, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "text/plain; version=0.0.4; charset=utf-8");
    assert!(body.contains("truss_http_requests_total"));
    assert!(body.contains("truss_http_responses_total"));
}

// ---------------------------------------------------------------------------
// Remote redirect failure cases
// ---------------------------------------------------------------------------

#[test]
fn serve_once_rejects_redirect_without_location_header() {
    let storage_root = temp_dir("redirect-no-location");
    let (url, fixture) = spawn_fixture_server(vec![("302 Found".to_string(), vec![], Vec::new())]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        "{{\"source\":{{\"kind\":\"url\",\"url\":\"{url}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 502 Bad Gateway"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("Location"));
}

#[test]
fn serve_once_rejects_redirect_limit_exceeded() {
    let storage_root = temp_dir("redirect-limit");
    let mut responses: Vec<common::FixtureResponse> = Vec::new();
    for _ in 0..6 {
        responses.push((
            "302 Found".to_string(),
            vec![("Location".to_string(), "/image".to_string())],
            Vec::new(),
        ));
    }
    responses.push((
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    ));
    let (url, fixture) = spawn_fixture_server(responses);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        "{{\"source\":{{\"kind\":\"url\",\"url\":\"{url}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 508 Loop Detected"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("redirect"));
}

// ── Health, routing, and error-path integration tests ───────────────

#[test]
#[serial]
fn serve_once_health_endpoint_returns_200() {
    let storage = temp_dir("health-200");
    let config = ServerConfig::new(storage, None);
    let (addr, handle) = spawn_server(config);

    let response = send_raw_request(
        addr,
        "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200 from /health, got: {header}"
    );
    assert_eq!(content_type, "application/json");
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("\"status\":\"ok\""),
        "expected JSON health body, got: {body_str}"
    );
}

#[test]
#[serial]
fn serve_once_health_token_rejects_unauthenticated() {
    let storage = temp_dir("health-auth-reject");
    let mut config = ServerConfig::new(storage, None);
    config.health_token = Some("health-secret".to_string());
    let (addr, handle) = spawn_server(config);

    let response = send_raw_request(
        addr,
        "GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 401"),
        "expected 401 from /health without token, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(
        header.contains("WWW-Authenticate: Bearer"),
        "expected WWW-Authenticate header in: {header}"
    );
    let body_str = String::from_utf8_lossy(&_body);
    assert!(
        body_str.contains("health endpoint requires authentication"),
        "expected auth error message, got: {body_str}"
    );
}

#[test]
#[serial]
fn serve_once_health_token_rejects_wrong_token() {
    let storage = temp_dir("health-auth-wrong");
    let mut config = ServerConfig::new(storage, None);
    config.health_token = Some("health-secret".to_string());
    let (addr, handle) = spawn_server(config);

    let response = send_raw_request(
        addr,
        "GET /health HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer wrong-token\r\nConnection: close\r\n\r\n",
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 401"),
        "expected 401 from /health with wrong token, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
}

#[test]
#[serial]
fn serve_once_health_token_accepts_valid_token() {
    let storage = temp_dir("health-auth-ok");
    let mut config = ServerConfig::new(storage, None);
    config.health_token = Some("health-secret".to_string());
    let (addr, handle) = spawn_server(config);

    let response = send_raw_request(
        addr,
        "GET /health HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer health-secret\r\nConnection: close\r\n\r\n",
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200 from /health with valid token, got: {header}"
    );
    assert_eq!(content_type, "application/json");
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        body_str.contains("\"status\":\"ok\""),
        "expected JSON health body, got: {body_str}"
    );
}

#[test]
#[serial]
fn serve_once_health_live_unaffected_by_health_token() {
    let storage = temp_dir("health-live-no-auth");
    let mut config = ServerConfig::new(storage, None);
    config.health_token = Some("health-secret".to_string());
    let (addr, handle) = spawn_server(config);

    let response = send_raw_request(
        addr,
        "GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200 from /health/live without token, got: {header}"
    );
}

#[test]
#[serial]
fn serve_once_health_ready_unaffected_by_health_token() {
    let storage = temp_dir("health-ready-no-auth");
    let mut config = ServerConfig::new(storage, None);
    config.health_token = Some("health-secret".to_string());
    let (addr, handle) = spawn_server(config);

    let response = send_raw_request(
        addr,
        "GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected 200 from /health/ready without token, got: {header}"
    );
}

#[test]
#[serial]
fn serve_once_returns_404_for_unknown_path() {
    let storage = temp_dir("not-found-404");
    let config = ServerConfig::new(storage, Some("secret".to_string()));
    let (addr, handle) = spawn_server(config);

    let response = send_raw_request(
        addr,
        "GET /nonexistent HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 404"),
        "expected 404 for unknown path, got: {header}"
    );
}

#[test]
#[serial]
fn serve_once_strips_crlf_from_x_request_id() {
    let storage = temp_dir("crlf-strip");
    fs::create_dir_all(storage.join("images")).expect("create images dir");
    fs::write(storage.join("images/test.png"), png_bytes()).expect("write test image");

    let config = ServerConfig::new(storage, Some("secret".to_string()));
    let (addr, handle) = spawn_server(config);

    let body = r#"{"source":{"kind":"path","path":"images/test.png"}}"#;
    let request = format!(
        "POST /images:transform HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer secret\r\nX-Request-Id: evil\r\ninjected-header: yes\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let response = send_raw_request(addr, &request);
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);
    assert!(
        !header.contains("injected-header"),
        "CRLF injection should not produce new headers in response"
    );
    assert!(
        !header.contains("X-Request-Id: evil\r\ninjected-header"),
        "X-Request-Id with CRLF should be stripped"
    );
}

#[test]
#[serial]
fn serve_once_rejects_post_transform_without_content_type() {
    let storage = temp_dir("no-content-type");
    let config = ServerConfig::new(storage, Some("secret".to_string()));
    let (addr, handle) = spawn_server(config);

    let body = r#"{"source":{"kind":"path","path":"test.png"}}"#;
    let request = format!(
        "POST /images:transform HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer secret\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let response = send_raw_request(addr, &request);
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 415"),
        "expected 415 for missing Content-Type, got: {header}"
    );
}

#[test]
#[serial]
fn serve_once_rejects_invalid_json_body() {
    let storage = temp_dir("invalid-json");
    let config = ServerConfig::new(storage, Some("secret".to_string()));
    let (addr, handle) = spawn_server(config);

    let body = "not valid json at all";
    let request = format!(
        "POST /images:transform HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAuthorization: Bearer secret\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let response = send_raw_request(addr, &request);
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "expected 400 for invalid JSON body, got: {header}"
    );
}

#[test]
#[serial]
fn serve_once_returns_404_for_missing_source_path() {
    let storage = temp_dir("missing-source");
    let config = ServerConfig::new(storage, Some("secret".to_string()));
    let (addr, handle) = spawn_server(config);

    let body = r#"{"source":{"kind":"path","path":"does-not-exist.png"}}"#;
    let response = send_transform_request(addr, body, Some("secret"));
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 404"),
        "expected 404 for missing source file, got: {header}"
    );
}
