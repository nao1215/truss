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

/// `Vary` describes the resource, not the request that happened to arrive. A
/// request with no `Accept` gets the default representation of a URL whose
/// representation still depends on `Accept`, so it has to say so: a shared
/// cache that stores a response with no `Vary` serves it to every client.
#[test]
fn serve_once_public_get_reports_vary_accept_when_the_request_omits_accept() {
    let storage_root = temp_dir("public-no-accept");
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
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _) = split_response(&response);
    assert!(header.starts_with("HTTP/1.1 200 OK"), "got: {header}");
    assert_eq!(content_type, "image/png");
    assert!(
        header.lines().any(|line| line == "Vary: Accept"),
        "a negotiable URL must report Vary: Accept even when the request sent no Accept: {header}"
    );
}

/// The narrow half of the same rule: an explicit `format` takes negotiation out
/// of the picture, so `Accept` cannot have influenced the answer and the header
/// would only split a CDN's entries for nothing.
#[test]
fn serve_once_public_get_omits_vary_accept_when_the_format_is_explicit() {
    let storage_root = temp_dir("public-explicit-format");
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
    let response = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("Accept", "image/avif,image/webp")],
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _) = split_response(&response);
    assert!(header.starts_with("HTTP/1.1 200 OK"), "got: {header}");
    assert_eq!(content_type, "image/jpeg");
    assert!(
        !header.lines().any(|line| line == "Vary: Accept"),
        "an explicitly formatted URL does not vary on Accept: {header}"
    );
}

/// The 406 is a selected response for the same resource, so it varies on the
/// header that produced it.
#[test]
fn serve_once_public_get_reports_vary_accept_on_not_acceptable() {
    let storage_root = temp_dir("public-not-acceptable");
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
        &[("Accept", "text/html")],
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 406"),
        "expected 406, got: {header}"
    );
    assert!(
        header.lines().any(|line| line == "Vary: Accept"),
        "the 406 was selected by Accept and must say so: {header}"
    );
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

/// Two requests differing only in an equivalent `Accept` header share one cache entry.
///
/// Negotiation's whole output is the format, which the key already carries. Including the
/// raw header meant every distinct string wrote its own copy of the same image: there are
/// unboundedly many equivalent strings, they come straight off the request, and with the
/// default `TRUSS_CACHE_MAX_BYTES` of 0 nothing reclaims them.
#[test]
fn serve_once_shares_one_cache_entry_across_equivalent_accept_headers() {
    let storage_root = temp_dir("accept-cache-sharing");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let cache_root = temp_dir("accept-cache-sharing-cache");
    let config = ServerConfig::new(storage_root, Some("secret".to_string()))
        .with_signed_url_credentials("public-dev", "secret-value")
        .with_cache_root(cache_root.clone());
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

    let (addr, handle) = spawn_server(config.clone());
    let first = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("Accept", "image/webp")],
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (first_header, first_content_type, first_body) = split_response(&first);
    assert!(first_header.contains("Cache-Status: \"truss\"; fwd=miss"));

    // A different string with the same meaning: webp is still the preferred type.
    let (addr, handle) = spawn_server(config);
    let second = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("Accept", "image/webp,image/png;q=0.5")],
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (second_header, second_content_type, second_body) = split_response(&second);

    assert_eq!(first_content_type, second_content_type);
    assert_eq!(first_body, second_body);
    assert!(
        second_header.contains("Cache-Status: \"truss\"; hit"),
        "the second request should hit the entry the first wrote: {second_header}"
    );
    // Negotiation still happened, so the response still varies on Accept.
    assert!(second_header.lines().any(|line| line == "Vary: Accept"));

    let entries = walk_files(&cache_root);
    assert_eq!(
        entries, 1,
        "two equivalent Accept headers wrote {entries} cache entries"
    );
}

/// Counts the regular files under `root`, at any depth.
fn walk_files(root: &std::path::Path) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() { walk_files(&path) } else { 1 }
        })
        .sum()
}
