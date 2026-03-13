#![allow(dead_code)]

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
use truss::{ServerConfig, serve_once_with_config};

pub fn png_bytes() -> Vec<u8> {
    let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 4, 3, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

/// Larger PNG suitable as a watermark base image (the main image must be larger than the
/// watermark). 64x64 is large enough to accept a 4x3 watermark with default margin.
/// Uses a visibly different color from `png_bytes()` so watermark compositing tests are
/// meaningful.
pub fn large_png_bytes() -> Vec<u8> {
    let image = RgbaImage::from_pixel(64, 64, Rgba([200, 100, 50, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 64, 64, ColorType::Rgba8.into())
        .expect("encode large png");
    bytes
}

pub fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("truss-server-integration-{name}-{unique}"));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

pub fn spawn_server(config: ServerConfig) -> (SocketAddr, thread::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = thread::spawn(move || serve_once_with_config(listener, config));

    (addr, handle)
}

pub type FixtureResponse = (String, Vec<(String, String)>, Vec<u8>);

pub fn spawn_fixture_server(responses: Vec<FixtureResponse>) -> (String, thread::JoinHandle<()>) {
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
                Duration::from_secs(10)
            } else {
                Duration::from_secs(15)
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
                        thread::sleep(Duration::from_millis(5));
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
            // Shut down the write half explicitly so the OS sends a clean FIN
            // rather than an RST.  On Windows, dropping a TcpStream that still
            // has unread data in the kernel buffer may trigger RST, which can
            // cause the *next* connection to the same listener to fail with
            // WSAECONNABORTED (os error 10053).
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });

    (url, handle)
}

pub fn send_transform_request(
    addr: SocketAddr,
    body: &str,
    authorization: Option<&str>,
) -> Vec<u8> {
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

pub fn send_upload_request(
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

pub fn send_metrics_request(addr: SocketAddr, authorization: Option<&str>) -> Vec<u8> {
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

pub fn send_public_get_request(addr: SocketAddr, target: &str, host: &str) -> Vec<u8> {
    send_public_get_request_with_headers(addr, target, host, &[])
}

pub fn send_public_get_request_with_headers(
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

pub fn upload_body(file_bytes: &[u8], options_json: Option<&str>) -> (String, Vec<u8>) {
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

pub fn split_response(response: &[u8]) -> (String, String, Vec<u8>) {
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

pub fn send_raw_request(addr: SocketAddr, request: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).expect("connect to test server");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

pub fn sign_public_query(
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

pub fn signed_target(
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
