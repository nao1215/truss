mod common;

use common::{
    png_bytes, send_public_get_request, send_public_get_request_with_headers,
    send_transform_request, signed_target, spawn_fixture_server, spawn_server, split_response,
    temp_dir,
};
use std::collections::BTreeMap;
use std::fs;
use truss::{MediaType, RawArtifact, ServerConfig, sniff_artifact};

#[test]
fn serve_once_transforms_a_signed_public_path_request() {
    let storage_root = temp_dir("public-path");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn serve_once_transforms_a_signed_public_url_request() {
    let storage_root = temp_dir("public-url");
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
    let target = signed_target(
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
    let response = send_public_get_request(addr, &target, "cdn.example.com");

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
fn serve_once_rejects_requests_without_a_bearer_token() {
    let storage_root = temp_dir("unauthorized");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/image.png"}}"#,
        None,
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 401 Unauthorized"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("authorization required"));
}

// ---------------------------------------------------------------------------
// Signed public GET failure cases
// ---------------------------------------------------------------------------

#[test]
fn serve_once_rejects_expired_signed_public_request() {
    let storage_root = temp_dir("expired-sig");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "1".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 401 Unauthorized"));
    assert!(body.to_lowercase().contains("expired"));
}

#[test]
fn serve_once_rejects_signed_public_request_with_wrong_secret() {
    let storage_root = temp_dir("wrong-sig");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "wrong-secret",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);

    assert!(header.starts_with("HTTP/1.1 401 Unauthorized"));
}

#[test]
fn serve_once_rejects_signed_public_request_with_accept_json() {
    let storage_root = temp_dir("accept-json");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("Accept", "application/json")],
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);

    assert!(header.starts_with("HTTP/1.1 406 Not Acceptable"));
}

#[test]
fn serve_once_rejects_signed_public_request_with_unknown_query_parameter() {
    let storage_root = temp_dir("unknown-param");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
            ("unknown".to_string(), "value".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert!(body.to_lowercase().contains("is not supported"));
}
