use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use truss::{serve_once_with_config, sniff_artifact, MediaType, RawArtifact, ServerConfig};

fn png_bytes() -> Vec<u8> {
    let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 4, 3, ColorType::Rgba8.into())
        .expect("encode png");
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
    let handle = thread::spawn(move || serve_once_with_config(listener, &config));

    (addr, handle)
}

fn spawn_fixture_server(
    responses: Vec<(String, Vec<(String, String)>, Vec<u8>)>,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
    listener
        .set_nonblocking(true)
        .expect("configure fixture server");
    let addr = listener.local_addr().expect("fixture server addr");
    let url = format!("http://{addr}/image");
    let handle = thread::spawn(move || {
        for (status, headers, body) in responses {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
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
        "POST /images:transform HTTP/1.1\r\nHost: localhost\r\n{authorization_header}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
        "POST /images HTTP/1.1\r\nHost: localhost\r\n{authorization_header}Content-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\n\r\n",
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
    let request = format!("GET /metrics HTTP/1.1\r\nHost: localhost\r\n{authorization_header}\r\n");
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
    assert_eq!(content_type, "application/json");
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
    assert_eq!(content_type, "application/json");
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
    assert_eq!(content_type, "application/json");
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
    assert_eq!(content_type, "application/json");
    assert!(body.contains("unsupported content-encoding"));
}
