mod common;

use common::{
    png_bytes, send_public_get_request, send_public_get_request_with_headers,
    send_transform_request, signed_target, spawn_server, split_response, temp_dir,
};
use std::collections::BTreeMap;
use std::fs;
use truss::{MediaType, RawArtifact, ServerConfig, sniff_artifact};

#[test]
fn serve_once_private_transform_sets_no_store_and_safety_headers() {
    let storage_root = temp_dir("private-headers");
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
    assert!(header.lines().any(|line| line == "Cache-Control: no-store"));
    assert!(header.contains("ETag: \"sha256-"));
    assert!(
        header
            .lines()
            .any(|line| line == "X-Content-Type-Options: nosniff")
    );
    assert!(
        header
            .lines()
            .any(|line| line == "Content-Disposition: inline; filename=\"truss.jpeg\"")
    );
    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn serve_once_public_get_negotiates_accept_and_sets_cache_headers() {
    let storage_root = temp_dir("public-negotiate");
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
        &[("Accept", "image/avif,image/webp;q=0.8")],
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/avif");
    assert!(
        header.lines().any(|line| {
            line == "Cache-Control: public, max-age=3600, stale-while-revalidate=60"
        })
    );
    assert!(header.contains("ETag: \"sha256-"));
    assert!(header.lines().any(|line| line == "Vary: Accept"));
    assert!(
        header
            .lines()
            .any(|line| line == "X-Content-Type-Options: nosniff")
    );
    assert!(
        header
            .lines()
            .any(|line| line == "Content-Disposition: inline; filename=\"truss.avif\"")
    );
    assert_eq!(artifact.media_type, MediaType::Avif);
}

#[test]
fn serve_once_public_get_returns_not_modified_for_matching_etag() {
    let storage_root = temp_dir("public-etag");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let config = ServerConfig::new(storage_root.clone(), Some("secret".to_string()))
        .with_signed_url_credentials("public-dev", "secret-value");
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

    let (addr, handle) = spawn_server(config.clone());
    let first_response = send_public_get_request(addr, &target, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (first_header, _, _) = split_response(&first_response);
    let etag = first_header
        .lines()
        .find_map(|line| line.strip_prefix("ETag: "))
        .expect("etag header")
        .to_string();

    let (addr, handle) = spawn_server(config);
    let second_response = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("If-None-Match", &etag)],
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&second_response);

    assert!(header.starts_with("HTTP/1.1 304 Not Modified"));
    assert!(content_type.is_empty());
    assert!(body.is_empty());
    assert!(header.contains("ETag: "));
    assert!(
        header.lines().any(|line| {
            line == "Cache-Control: public, max-age=3600, stale-while-revalidate=60"
        })
    );
}
