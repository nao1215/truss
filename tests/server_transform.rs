use hmac::{Hmac, Mac};
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use truss::{MediaType, RawArtifact, ServerConfig, serve_once_with_config, sniff_artifact};

fn png_bytes() -> Vec<u8> {
    let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 4, 3, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

/// Larger PNG suitable as a watermark base image (the main image must be larger than the
/// watermark). 64x64 is large enough to accept a 4x3 watermark with default margin.
fn large_png_bytes() -> Vec<u8> {
    let image = RgbaImage::from_pixel(64, 64, Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 64, 64, ColorType::Rgba8.into())
        .expect("encode large png");
    bytes
}

fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("truss-server-integration-{name}-{unique}"));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn spawn_server(config: ServerConfig) -> (SocketAddr, thread::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = thread::spawn(move || serve_once_with_config(listener, config));

    (addr, handle)
}

type FixtureResponse = (String, Vec<(String, String)>, Vec<u8>);

fn spawn_fixture_server(responses: Vec<FixtureResponse>) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
    listener
        .set_nonblocking(true)
        .expect("configure fixture server");
    let addr = listener.local_addr().expect("fixture server addr");
    let url = format!("http://{addr}/image");
    let handle = thread::spawn(move || {
        let mut served_any = false;
        for (status, headers, body) in responses {
            let timeout = if served_any {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(10)
            };
            let deadline = std::time::Instant::now() + timeout;
            let mut accepted = None;
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok(stream) => {
                        accepted = Some(stream);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept fixture request: {error}"),
                }
            }

            let Some((mut stream, _)) = accepted else {
                break;
            };
            served_any = true;
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let mut header = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            for (name, value) in headers {
                header.push_str(&format!("{name}: {value}\r\n"));
            }
            header.push_str("\r\n");
            stream
                .write_all(header.as_bytes())
                .expect("write fixture headers");
            stream.write_all(&body).expect("write fixture body");
            stream.flush().expect("flush fixture response");
        }
    });

    (url, handle)
}

fn send_transform_request(addr: SocketAddr, body: &str, authorization: Option<&str>) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    let authorization_header = authorization
        .map(|value| format!("Authorization: Bearer {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /images:transform HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{authorization_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn send_upload_request(
    addr: SocketAddr,
    body: &[u8],
    boundary: &str,
    authorization: Option<&str>,
) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    let authorization_header = authorization
        .map(|value| format!("Authorization: Bearer {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /images HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{authorization_header}Content-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write request");
    stream.write_all(body).expect("write body");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn send_metrics_request(addr: SocketAddr, authorization: Option<&str>) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    let authorization_header = authorization
        .map(|value| format!("Authorization: Bearer {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{authorization_header}\r\n"
    );
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn send_public_get_request(addr: SocketAddr, target: &str, host: &str) -> Vec<u8> {
    send_public_get_request_with_headers(addr, target, host, &[])
}

fn send_public_get_request_with_headers(
    addr: SocketAddr,
    target: &str,
    host: &str,
    headers: &[(&str, &str)],
) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    let mut request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n");
    request.push_str("Connection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

fn upload_body(file_bytes: &[u8], options_json: Option<&str>) -> (String, Vec<u8>) {
    let boundary = "truss-integration-boundary".to_string();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file_bytes);
    body.extend_from_slice(b"\r\n");

    if let Some(options_json) = options_json {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{options_json}\r\n"
            )
            .as_bytes(),
        );
    }

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

fn split_response(response: &[u8]) -> (String, String, Vec<u8>) {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("find header terminator");
    let header = String::from_utf8(response[..header_end].to_vec()).expect("utf8 header");
    let content_type = header
        .lines()
        .find_map(|line| line.strip_prefix("Content-Type: "))
        .unwrap_or_default()
        .to_string();

    (header, content_type, response[(header_end + 4)..].to_vec())
}

fn sign_public_query(
    method: &str,
    authority: &str,
    path: &str,
    query: &BTreeMap<String, String>,
    secret: &str,
) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in query {
        if name != "signature" {
            serializer.append_pair(name, value);
        }
    }
    let canonical = format!("{method}\n{authority}\n{path}\n{}", serializer.finish());
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("create hmac");
    mac.update(canonical.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn signed_target(
    path: &str,
    query: BTreeMap<String, String>,
    authority: &str,
    secret: &str,
) -> String {
    let mut query = query;
    let signature = sign_public_query("GET", authority, path, &query, secret);
    query.insert("signature".to_string(), signature);
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in query {
        serializer.append_pair(&name, &value);
    }
    format!("{path}?{}", serializer.finish())
}

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
fn serve_once_transforms_an_uploaded_file_over_http() {
    let storage_root = temp_dir("upload-success");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let (boundary, body) = upload_body(&png_bytes(), Some(r#"{"format":"jpeg"}"#));
    let response = send_upload_request(addr, &body, &boundary, Some("secret"));

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
fn serve_once_rejects_uploads_without_a_file_field() {
    let storage_root = temp_dir("upload-missing-file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"jpeg\"}}\r\n--{boundary}--\r\n"
    )
    .into_bytes();
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("requires a `file` field"));
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

// ---------------------------------------------------------------------------
// A. Signed public GET failure cases
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

// ---------------------------------------------------------------------------
// B. Multipart/upload failure cases
// ---------------------------------------------------------------------------

#[test]
fn serve_once_rejects_upload_with_empty_file_field() {
    let storage_root = temp_dir("upload-empty-file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n\r\n--{boundary}--\r\n"
    )
    .into_bytes();
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("empty"));
}

#[test]
fn serve_once_rejects_upload_with_duplicate_file_field() {
    let storage_root = temp_dir("upload-dup-file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image2.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("multiple"));
}

#[test]
fn serve_once_rejects_upload_with_duplicate_options_field() {
    let storage_root = temp_dir("upload-dup-options");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"jpeg\"}}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"png\"}}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("multiple"));
}

#[test]
fn serve_once_rejects_upload_with_invalid_json_in_options() {
    let storage_root = temp_dir("upload-bad-json");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{invalid json\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("JSON"));
}

#[test]
fn serve_once_rejects_upload_with_wrong_content_type_on_options() {
    let storage_root = temp_dir("upload-wrong-ct");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: text/plain\r\n\r\n{{\"format\":\"jpeg\"}}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("application/json"));
}

#[test]
fn serve_once_rejects_upload_with_unknown_field_name() {
    let storage_root = temp_dir("upload-unknown-field");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"extra\"\r\nContent-Type: text/plain\r\n\r\nsome data\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("unsupported field"));
}

// ---------------------------------------------------------------------------
// C. Remote redirect failure cases
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
    let mut responses: Vec<FixtureResponse> = Vec::new();
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

// ---------------------------------------------------------------------------
// D. Watermark support
// ---------------------------------------------------------------------------

#[test]
fn test_json_transform_with_watermark() {
    let storage_root = temp_dir("wm-json");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    // Spawn a fixture server to serve the watermark image.
    let (wm_url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.png"}},"options":{{"format":"png"}},"watermark":{{"url":"{wm_url}","position":"center","opacity":80,"margin":0}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK but got header: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "image/png");
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff watermarked output");
    assert_eq!(artifact.media_type, MediaType::Png);
}

#[test]
fn test_multipart_upload_with_watermark() {
    let storage_root = temp_dir("wm-multipart");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-wm-boundary";
    let png = large_png_bytes();
    let mut body = Vec::new();

    // file part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    // watermark part (small image used as watermark)
    let wm_png = png_bytes();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark\"; filename=\"wm.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&wm_png);
    body.extend_from_slice(b"\r\n");

    // watermark_position part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark_position\"\r\n\r\nbottom-right\r\n"
        )
        .as_bytes(),
    );

    // watermark_opacity part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark_opacity\"\r\n\r\n75\r\n"
        )
        .as_bytes(),
    );

    // watermark_margin part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark_margin\"\r\n\r\n5\r\n"
        )
        .as_bytes(),
    );

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK but got header: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "image/png");
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff watermarked upload");
    assert_eq!(artifact.media_type, MediaType::Png);
}

#[test]
fn test_multipart_upload_rejects_duplicate_watermark() {
    let storage_root = temp_dir("wm-dup");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-wm-dup-boundary";
    let png = png_bytes();
    let mut body = Vec::new();

    // file part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    // first watermark part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark\"; filename=\"wm1.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    // second watermark part (duplicate)
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark\"; filename=\"wm2.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("multiple"));
}

#[test]
fn test_public_get_with_watermark_query_params() {
    let storage_root = temp_dir("wm-public-get");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    // Spawn a fixture server to serve the watermark image.
    let (wm_url, fixture) = spawn_fixture_server(vec![(
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
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("watermarkUrl".to_string(), wm_url),
            ("watermarkPosition".to_string(), "bottom-right".to_string()),
            ("watermarkOpacity".to_string(), "60".to_string()),
            ("watermarkMargin".to_string(), "5".to_string()),
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
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK but got header: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "image/png");
    let artifact =
        sniff_artifact(RawArtifact::new(body, None)).expect("sniff watermarked public output");
    assert_eq!(artifact.media_type, MediaType::Png);
}

#[test]
fn test_watermark_opacity_zero_rejected() {
    let storage_root = temp_dir("wm-opacity-zero");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");

    let (wm_url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.png"}},"watermark":{{"url":"{wm_url}","opacity":0}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("opacity"));
}

#[test]
fn test_watermark_opacity_over_100_rejected() {
    let storage_root = temp_dir("wm-opacity-over");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");

    let (wm_url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.png"}},"watermark":{{"url":"{wm_url}","opacity":101}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("opacity"));
}

#[test]
fn test_orphaned_watermark_params_without_url() {
    let storage_root = temp_dir("wm-orphan-params");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value")
            .with_insecure_url_sources(true),
    );

    // watermarkPosition/Opacity/Margin WITHOUT watermarkUrl should be rejected.
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("watermarkPosition".to_string(), "center".to_string()),
            ("watermarkOpacity".to_string(), "80".to_string()),
            ("watermarkMargin".to_string(), "5".to_string()),
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
    let body = String::from_utf8(body).expect("utf8 response body");
    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("watermarkUrl"));
}

#[test]
fn test_empty_watermark_url_rejected() {
    let storage_root = temp_dir("wm-empty-url");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );

    // Empty watermark URL should be rejected when watermark object is present.
    let body = r#"{"source":{"kind":"path","path":"/image.png"},"watermark":{"url":""}}"#;
    let response = send_transform_request(addr, body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");
    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("url"));
}

#[cfg(feature = "svg")]
#[test]
fn test_svg_source_with_watermark_rejected() {
    let storage_root = temp_dir("wm-svg-reject");
    let svg_content = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"><rect fill=\"red\" width=\"100\" height=\"100\"/></svg>";
    fs::write(storage_root.join("image.svg"), svg_content).expect("write svg fixture");

    let (wm_url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.svg"}},"watermark":{{"url":"{wm_url}"}}}}"#
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
        header.starts_with("HTTP/1.1 400 Bad Request"),
        "expected 400 but got: {header}\nbody: {body}"
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("svg"));
}

#[test]
fn test_watermark_url_redirect_followed() {
    let storage_root = temp_dir("wm-redirect");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    let (wm_url, fixture) = spawn_fixture_server(vec![
        (
            "302 Found".to_string(),
            vec![
                ("Content-Type".to_string(), "text/plain".to_string()),
                ("Location".to_string(), "/redirected".to_string()),
            ],
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
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.png"}},"watermark":{{"url":"{wm_url}"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, _content_type, body) = split_response(&response);
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1"),
        "expected valid HTTP response but got: {header}\nbody: {body_str}"
    );
}
