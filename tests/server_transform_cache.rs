mod common;

use common::{
    png_bytes, send_public_get_request, send_public_get_request_with_headers,
    send_transform_request, signed_target, spawn_server, split_response, temp_dir,
};
use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::Ordering;
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

/// A variant already on disk costs no CPU to produce, so it is what a saturated server can
/// still answer. The pre-flight cache lookup that makes that true was reachable only for a
/// request that named its own format, so the caller who relied on `Accept` negotiation --
/// the arrangement `docs/api-reference.md` describes for a CDN -- was shed instead.
///
/// Saturation is expressed by setting the in-flight counter to the limit rather than by
/// holding a real transform, so the test states the condition it means and does not race.
#[test]
fn serve_once_answers_a_warm_negotiated_variant_while_every_transform_slot_is_taken() {
    let storage_root = temp_dir("saturated-negotiated");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let cache_root = temp_dir("saturated-negotiated-cache");
    let config = ServerConfig::new(storage_root, Some("secret".to_string()))
        .with_signed_url_credentials("public-dev", "secret-value")
        .with_cache_root(cache_root);
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

    // Warm the entry. The URL names no format, so the output is negotiated from Accept.
    let (addr, handle) = spawn_server(config.clone());
    let warm = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("Accept", "image/webp")],
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (warm_header, warm_content_type, warm_body) = split_response(&warm);
    assert!(
        warm_header.contains("Cache-Status: \"truss\"; fwd=miss"),
        "the first request writes the entry: {warm_header}"
    );

    // Every transform slot is taken.
    config
        .transforms_in_flight
        .store(config.max_concurrent_transforms, Ordering::Relaxed);

    let (addr, handle) = spawn_server(config.clone());
    let response = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("Accept", "image/webp")],
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (header, content_type, body) = split_response(&response);

    assert!(
        header.starts_with("HTTP/1.1 200 OK"),
        "a cached variant costs no transform slot to serve: {header}"
    );
    assert!(
        header.contains("Cache-Status: \"truss\"; hit"),
        "the warm entry should answer it: {header}"
    );
    assert_eq!(content_type, warm_content_type);
    assert_eq!(body, warm_body);
}

/// The other half of the rule: the limit still sheds a request that has no cached answer,
/// so the fix above cannot be "stop counting slots".
#[test]
fn serve_once_still_sheds_an_uncached_transform_while_every_slot_is_taken() {
    let storage_root = temp_dir("saturated-miss");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let cache_root = temp_dir("saturated-miss-cache");
    let config =
        ServerConfig::new(storage_root, Some("secret".to_string())).with_cache_root(cache_root);
    config
        .transforms_in_flight
        .store(config.max_concurrent_transforms, Ordering::Relaxed);

    let (addr, handle) = spawn_server(config);
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/image.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 body");
    assert!(
        header.starts_with("HTTP/1.1 503"),
        "an uncached transform still needs a slot: {header}"
    );
    assert!(body.contains("too many concurrent transforms"), "{body}");
    // The answer says to retry later, so it has to say when: an immediate retry is more of
    // the load this response was sent to shed.
    assert!(
        header.lines().any(|line| line == "Retry-After: 1"),
        "a shed request carries the delay through the writer: {header}"
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

/// A warning the transform raises rides on the response as a `Truss-Warning` header, on the
/// miss that produced it and on the hit that replays the entry, and is absent when there
/// is nothing to warn about. The fixture carries EXIF orientation 6, and `autoOrient=false`
/// with the default strip is the combination that drops it.
#[test]
fn serve_once_carries_transform_warnings_as_headers_on_miss_and_hit() {
    let storage_root = temp_dir("warning-header");
    fs::write(
        storage_root.join("tagged.jpg"),
        include_bytes!("../integration/fixtures/exif-rotated.jpg"),
    )
    .expect("write source fixture");
    let cache_root = temp_dir("warning-header-cache");
    let config = ServerConfig::new(storage_root, Some("secret".to_string()))
        .with_signed_url_credentials("public-dev", "secret-value")
        .with_cache_root(cache_root);
    let params = |auto_orient: Option<&str>| {
        let mut params = BTreeMap::from([
            ("path".to_string(), "/tagged.jpg".to_string()),
            ("format".to_string(), "png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
        ]);
        if let Some(value) = auto_orient {
            params.insert("autoOrient".to_string(), value.to_string());
        }
        signed_target("/images/by-path", params, "cdn.example.com", "secret-value")
    };
    let warning_lines = |header: &str| -> Vec<String> {
        header
            .lines()
            .filter(|line| line.starts_with("Truss-Warning: "))
            .map(str::to_string)
            .collect()
    };

    let dropped = params(Some("false"));
    let (addr, handle) = spawn_server(config.clone());
    let first = send_public_get_request(addr, &dropped, "cdn.example.com");
    handle.join().expect("join").expect("serve");
    let (first_header, _, _) = split_response(&first);
    assert!(first_header.contains("Cache-Status: \"truss\"; fwd=miss"));
    let first_warnings = warning_lines(&first_header);
    assert_eq!(first_warnings.len(), 1, "{first_header}");
    assert!(
        first_warnings[0].contains("EXIF orientation 6"),
        "{first_warnings:?}"
    );

    let (addr, handle) = spawn_server(config.clone());
    let second = send_public_get_request(addr, &dropped, "cdn.example.com");
    handle.join().expect("join").expect("serve");
    let (second_header, _, _) = split_response(&second);
    assert!(
        second_header.contains("Cache-Status: \"truss\"; hit"),
        "{second_header}"
    );
    assert_eq!(
        warning_lines(&second_header),
        first_warnings,
        "the hit should repeat the warning the miss produced"
    );

    let (addr, handle) = spawn_server(config);
    let applied = send_public_get_request(addr, &params(None), "cdn.example.com");
    handle.join().expect("join").expect("serve");
    let (applied_header, _, _) = split_response(&applied);
    assert!(
        applied_header.starts_with("HTTP/1.1 200"),
        "{applied_header}"
    );
    assert!(
        warning_lines(&applied_header).is_empty(),
        "nothing to warn about: {applied_header}"
    );
}

/// An option set the transform refuses under every input stays refused when the cache
/// already holds the entry an equivalent valid request wrote.
///
/// `fit` and `position` are the two options `compute_cache_key` leaves out when width and
/// height are not both present, so a request carrying an invalid `fit` hashes to the key of
/// the valid request without it. With the option check running inside the transform, after
/// the lookup, the same signed URL answered 400 on a cold cache and 200 on a warm one, and
/// which of the two a caller saw was decided by what some other client had asked for.
#[test]
fn serve_once_public_get_refuses_fit_without_both_dimensions_on_a_warm_cache() {
    let storage_root = temp_dir("fit-without-dimensions");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let cache_root = temp_dir("fit-without-dimensions-cache");
    let config = ServerConfig::new(storage_root, Some("secret".to_string()))
        .with_signed_url_credentials("public-dev", "secret-value")
        .with_cache_root(cache_root);

    let valid_query = BTreeMap::from([
        ("path".to_string(), "/image.png".to_string()),
        ("keyId".to_string(), "public-dev".to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]);
    let mut invalid_query = valid_query.clone();
    invalid_query.insert("fit".to_string(), "cover".to_string());

    let warm_target = signed_target(
        "/images/by-path",
        valid_query,
        "cdn.example.com",
        "secret-value",
    );
    let invalid_target = signed_target(
        "/images/by-path",
        invalid_query,
        "cdn.example.com",
        "secret-value",
    );

    let (addr, handle) = spawn_server(config.clone());
    let warm_up = send_public_get_request(addr, &warm_target, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (warm_header, _, _) = split_response(&warm_up);
    assert!(
        warm_header.starts_with("HTTP/1.1 200 OK"),
        "the warm-up request must write a cache entry: {warm_header}"
    );

    let (addr, handle) = spawn_server(config);
    let response = send_public_get_request(addr, &invalid_target, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 400 Bad Request"),
        "fit without both dimensions is refused whatever the cache holds: {header}\n{body}"
    );
    assert!(
        body.contains("fit requires both width and height"),
        "the refusal names the rule the caller broke: {body}"
    );
}

/// The same rule for `position`, which shares the conditional the cache key uses.
#[test]
fn serve_once_public_get_refuses_position_without_both_dimensions_on_a_warm_cache() {
    let storage_root = temp_dir("position-without-dimensions");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let cache_root = temp_dir("position-without-dimensions-cache");
    let config = ServerConfig::new(storage_root, Some("secret".to_string()))
        .with_signed_url_credentials("public-dev", "secret-value")
        .with_cache_root(cache_root);

    let valid_query = BTreeMap::from([
        ("path".to_string(), "/image.png".to_string()),
        ("keyId".to_string(), "public-dev".to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]);
    let mut invalid_query = valid_query.clone();
    invalid_query.insert("position".to_string(), "center".to_string());

    let warm_target = signed_target(
        "/images/by-path",
        valid_query,
        "cdn.example.com",
        "secret-value",
    );
    let invalid_target = signed_target(
        "/images/by-path",
        invalid_query,
        "cdn.example.com",
        "secret-value",
    );

    let (addr, handle) = spawn_server(config.clone());
    let _ = send_public_get_request(addr, &warm_target, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (addr, handle) = spawn_server(config);
    let response = send_public_get_request(addr, &invalid_target, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 400 Bad Request"),
        "position without both dimensions is refused whatever the cache holds: {header}\n{body}"
    );
    assert!(
        body.contains("position requires both width and height"),
        "the refusal names the rule the caller broke: {body}"
    );
}
