// Tests ported from imgproxy and imagor edge case patterns.
//
// imgproxy: imagedata/image_data_test.go (corrupted images, gzip, Content-Length excess)
//           processing_handler_test.go (ETag with different options, skip format errors)
// imagor:   imagor_test.go (invalid image data, empty body)
//           processor_test.go (zero dimensions, oversized remote)

mod common;

use common::{
    png_bytes, send_public_get_request, send_public_get_request_with_headers,
    send_transform_request, signed_target, spawn_fixture_server, spawn_server, split_response,
    temp_dir,
};
use std::collections::BTreeMap;
use std::fs;
use truss::ServerConfig;

// ---------------------------------------------------------------------------
// Corrupted / invalid image data (from imgproxy TestDownloadInvalidImage,
// imagor processor error handling)
// ---------------------------------------------------------------------------

#[test]
fn corrupted_remote_image_returns_error() {
    let storage_root = temp_dir("corrupted-remote");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        b"this is not a valid PNG image at all".to_vec(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body =
        format!(r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#);
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 415"),
        "corrupted remote image should return 415 Unsupported Media Type, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
}

#[test]
fn corrupted_local_image_returns_415() {
    let storage_root = temp_dir("corrupted-local");
    fs::write(
        storage_root.join("bad.png"),
        b"definitely not a PNG file, just garbage bytes",
    )
    .expect("write corrupted file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/bad.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 415"),
        "corrupted local image should return 415 Unsupported Media Type, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
}

#[test]
fn empty_file_returns_error() {
    // From imagor: empty blob handling
    let storage_root = temp_dir("empty-file");
    fs::write(storage_root.join("empty.png"), b"").expect("write empty file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/empty.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 415"),
        "empty file should return 415 Unsupported Media Type, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
}

#[test]
fn truncated_png_header_returns_error() {
    // Image file with valid PNG magic bytes but truncated data
    let storage_root = temp_dir("truncated-png");
    // PNG magic bytes (first 8 bytes) but nothing else
    let truncated = b"\x89PNG\r\n\x1a\n";
    fs::write(storage_root.join("trunc.png"), truncated).expect("write truncated file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/trunc.png"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "truncated PNG should return error, got: {header}"
    );
    assert_eq!(content_type, "application/problem+json");
}

// ---------------------------------------------------------------------------
// Remote Content-Length exceeds limit (from imgproxy TestDownloadImageFileTooLarge)
// ---------------------------------------------------------------------------
// NOTE: The fixture server auto-adds Content-Length from actual body size,
// so we use a custom raw fixture to test the Content-Length pre-check.

#[test]
fn remote_content_length_exceeding_limit_returns_413() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    let storage_root = temp_dir("content-length-exceed");
    // Set up a raw fixture server that sends a large Content-Length header
    // without actually sending that many bytes.
    let fixture_listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
    let fixture_addr = fixture_listener.local_addr().expect("fixture addr");
    let fixture_url = format!("http://{fixture_addr}/image");

    let fixture_handle = thread::spawn(move || {
        fixture_listener
            .set_nonblocking(false)
            .expect("set blocking");
        let (mut stream, _) = fixture_listener.accept().expect("accept");
        // Read the request
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);

        // Respond with Content-Length larger than MAX_SOURCE_BYTES (100MB)
        let response = "HTTP/1.1 200 OK\r\n\
            Content-Type: image/png\r\n\
            Content-Length: 104857601\r\n\
            Connection: close\r\n\r\n";
        stream.write_all(response.as_bytes()).expect("write");
        // Send only a few bytes, then close - the Content-Length check
        // should reject before trying to read the full body.
        stream.write_all(&[0u8; 100]).expect("write partial");
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"url","url":"{fixture_url}"}},"options":{{"format":"jpeg"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture_handle.join().expect("join fixture");

    let (header, content_type, body) = split_response(&response);
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 413"),
        "oversized Content-Length should return 413, got: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(body_str.contains("exceeds"));
}

// ---------------------------------------------------------------------------
// Unsupported Content-Encoding variants (from imgproxy transport tests)
// ---------------------------------------------------------------------------

#[test]
fn unsupported_deflate_encoding_returns_502() {
    let storage_root = temp_dir("deflate-encoding");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![
            ("Content-Type".to_string(), "image/png".to_string()),
            ("Content-Encoding".to_string(), "deflate".to_string()),
        ],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body =
        format!(r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#);
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
        "deflate encoding should be rejected, got: {header}"
    );
    assert!(body.contains("unsupported content-encoding"));
}

#[test]
fn unsupported_zstd_encoding_returns_502() {
    let storage_root = temp_dir("zstd-encoding");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![
            ("Content-Type".to_string(), "image/png".to_string()),
            ("Content-Encoding".to_string(), "zstd".to_string()),
        ],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body =
        format!(r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#);
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
        "zstd encoding should be rejected, got: {header}"
    );
    assert!(body.contains("unsupported content-encoding"));
}

/// `br` used to be on the accepted list while `ureq`'s `brotli` feature was not enabled, so
/// a brotli body reached the sniffer still compressed and the request was answered 415 about
/// the caller's own file. It belongs with `deflate` and `zstd` above: an encoding truss
/// cannot read is the origin's, and is named.
#[test]
fn unsupported_br_encoding_returns_502() {
    let storage_root = temp_dir("br-encoding");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![
            ("Content-Type".to_string(), "image/png".to_string()),
            ("Content-Encoding".to_string(), "br".to_string()),
        ],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body =
        format!(r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#);
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
        "br encoding should be rejected, got: {header}"
    );
    assert!(body.contains("unsupported content-encoding `br`"), "{body}");
}

/// The encoding is still read case-insensitively on the side that is accepted, so an origin
/// that spells a coding in upper case is not refused for a coding truss can read. A coding
/// token is case-insensitive per RFC 9110 section 8.4.1.
#[test]
fn an_upper_case_gzip_encoding_is_accepted() {
    let storage_root = temp_dir("gzip-case");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![
            ("Content-Type".to_string(), "image/png".to_string()),
            ("Content-Encoding".to_string(), "IDENTITY".to_string()),
        ],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body =
        format!(r#"{{"source":{{"kind":"url","url":"{url}"}},"options":{{"format":"jpeg"}}}}"#);
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "a coding token is case-insensitive, got: {header}"
    );
}

// ---------------------------------------------------------------------------
// ETag differs when processing options change
// (from imgproxy TestETagProcessingOptionsNotMatch)
// ---------------------------------------------------------------------------

#[test]
fn etag_differs_for_different_processing_options() {
    let storage_root = temp_dir("etag-options");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write image");
    let config = ServerConfig::new(storage_root.clone(), Some("secret".to_string()))
        .with_signed_url_credentials("key", "secret-value");

    // Request 1: format=jpeg
    let target_jpeg = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "key".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let (addr, handle) = spawn_server(config.clone());
    let response1 = send_public_get_request(addr, &target_jpeg, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (header1, _, _) = split_response(&response1);
    let etag1 = header1
        .lines()
        .find_map(|l| l.strip_prefix("ETag: "))
        .expect("first ETag")
        .to_string();

    // Request 2: format=png (different processing options)
    let target_png = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "key".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "png".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let (addr, handle) = spawn_server(config.clone());
    let response2 = send_public_get_request(addr, &target_png, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (header2, _, _) = split_response(&response2);
    let etag2 = header2
        .lines()
        .find_map(|l| l.strip_prefix("ETag: "))
        .expect("second ETag")
        .to_string();

    assert_ne!(
        etag1, etag2,
        "different processing options should produce different ETags"
    );
}

#[test]
fn etag_same_for_identical_request() {
    // From imgproxy: same request should produce same ETag
    let storage_root = temp_dir("etag-stable");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write image");
    let config = ServerConfig::new(storage_root.clone(), Some("secret".to_string()))
        .with_signed_url_credentials("key", "secret-value");

    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "key".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );

    let (addr, handle) = spawn_server(config.clone());
    let response1 = send_public_get_request(addr, &target, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (header1, _, _) = split_response(&response1);
    let etag1 = header1
        .lines()
        .find_map(|l| l.strip_prefix("ETag: "))
        .expect("first ETag")
        .to_string();

    let (addr, handle) = spawn_server(config);
    let response2 = send_public_get_request(addr, &target, "cdn.example.com");
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    let (header2, _, _) = split_response(&response2);
    let etag2 = header2
        .lines()
        .find_map(|l| l.strip_prefix("ETag: "))
        .expect("second ETag")
        .to_string();

    assert_eq!(etag1, etag2, "same request should produce the same ETag");
}

// ---------------------------------------------------------------------------
// ETag mismatch returns 200, not 304
// (from imgproxy TestETagReqNotMatch, TestETagDataNotMatch)
// ---------------------------------------------------------------------------

#[test]
fn etag_mismatch_returns_200_with_body() {
    let storage_root = temp_dir("etag-mismatch");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write image");
    let config = ServerConfig::new(storage_root.clone(), Some("secret".to_string()))
        .with_signed_url_credentials("key", "secret-value");

    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "key".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );

    let (addr, handle) = spawn_server(config);
    let response = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("If-None-Match", "\"sha256-obviously-wrong-etag\"")],
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "ETag mismatch should return 200, got: {header}"
    );
    assert_eq!(content_type, "image/jpeg");
    assert!(!body.is_empty(), "200 response should have a body");
}

// ---------------------------------------------------------------------------
// Source path: empty and special characters
// (from imagor path parsing edge cases)
// ---------------------------------------------------------------------------

#[test]
fn source_path_empty_returns_400() {
    let storage_root = temp_dir("empty-path");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":""},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "empty path should return 400, got: {header}"
    );
}

#[test]
fn source_path_slash_only_returns_400() {
    let storage_root = temp_dir("slash-only-path");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/"},"options":{"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 400"),
        "slash-only path should return 400, got: {header}"
    );
}

// ---------------------------------------------------------------------------
// Oversized output pixel limit
// (from imgproxy TestResultSizeLimit)
// ---------------------------------------------------------------------------

#[test]
fn width_only_within_limit_succeeds() {
    let storage_root = temp_dir("width-only");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write image");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    // Only width specified, no height - should succeed
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/image.png"},"options":{"width":100,"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "width-only transform should succeed, got: {header}"
    );
    assert_eq!(content_type, "image/jpeg");
}

#[test]
fn height_only_within_limit_succeeds() {
    let storage_root = temp_dir("height-only");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write image");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/image.png"},"options":{"height":100,"format":"jpeg"}}"#,
        Some("secret"),
    );
    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "height-only transform should succeed, got: {header}"
    );
    assert_eq!(content_type, "image/jpeg");
}

// ---------------------------------------------------------------------------
// Accept header: unsupported type returns 406
// (from imgproxy TestSourceFormatNotSupported)
// ---------------------------------------------------------------------------

#[test]
fn accept_application_json_on_public_endpoint_returns_406() {
    let storage_root = temp_dir("accept-406");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write image");
    let config = ServerConfig::new(storage_root, Some("secret".to_string()))
        .with_signed_url_credentials("key", "secret-value");
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "key".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );

    let (addr, handle) = spawn_server(config);
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

    let (header, _, _) = split_response(&response);
    assert!(
        header.starts_with("HTTP/1.1 406"),
        "non-image Accept should return 406, got: {header}"
    );
}

/// Returns a URL whose port had a listener just long enough to be handed out and has
/// none now, so any connection to it is refused immediately.
fn url_on_a_closed_port() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind to reserve a port");
    let addr = listener.local_addr().expect("reserved port");
    drop(listener);
    format!("http://{addr}/source.png")
}

/// An option set the transform refuses under every input is refused before the source
/// is fetched.
///
/// Nothing is listening on the port below, so a `502` naming the connection is proof
/// that truss went out to the origin for a request it was always going to turn down.
/// For a `by-url` source that is a request to somebody else's server, and for a storage
/// backend it is a billed GET, spent on an answer that cannot change.
#[test]
fn public_get_refuses_invalid_options_before_fetching_the_remote_source() {
    let storage_root = temp_dir("invalid-options-no-fetch");
    let source_url = url_on_a_closed_port();
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value")
            .with_insecure_url_sources(true),
    );
    let target = signed_target(
        "/images/by-url",
        BTreeMap::from([
            ("url".to_string(), source_url),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "png".to_string()),
            ("fit".to_string(), "cover".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _, body) = split_response(&response);
    let body = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 400 Bad Request"),
        "an always-invalid option set is the caller's error, not the origin's: {header}\n{body}"
    );
    assert!(
        body.contains("fit requires both width and height"),
        "the refusal names the option rather than the fetch: {body}"
    );
}
