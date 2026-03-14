#![allow(dead_code)]
// Shared across multiple integration-test crates; each crate uses only a subset.

use hmac::{Hmac, Mac};
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::str::FromStr;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use truss::{
    CropRegion, Fit, MediaType, OptimizeMode, Position, Rgba8, Rotation, ServerConfig,
    SignedUrlSource, SignedWatermarkParams, TargetQuality, TransformOptions,
    serve_once_with_config, sign_public_url,
};
use url::Url;

pub fn png_bytes() -> Vec<u8> {
    let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 4, 3, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

/// Small 2x2 PNG suitable for cloud integration tests where image content
/// does not matter and a minimal payload is preferred.
pub fn tiny_png() -> Vec<u8> {
    let image = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 2, 2, ColorType::Rgba8.into())
        .expect("encode tiny png");
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

fn find_header_terminator(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn read_fixture_request(stream: &mut TcpStream) {
    stream
        .set_nonblocking(false)
        .expect("configure fixture stream blocking mode");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("configure fixture stream timeout");

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && std::time::Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => panic!("read fixture request headers: {error}"),
        };
        if read == 0 {
            panic!("fixture request ended before headers were complete");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_header_terminator(&buffer) {
            break index;
        }
    };

    let header_text = std::str::from_utf8(&buffer[..header_end]).expect("fixture request utf8");
    let content_length = header_text
        .split("\r\n")
        .filter_map(|line| line.split_once(':'))
        .find_map(|(name, value)| {
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .map(|value| {
            value
                .parse::<usize>()
                .expect("fixture content-length should be numeric")
        })
        .unwrap_or(0);

    let mut body = buffer.len().saturating_sub(header_end + 4);
    while body < content_length {
        let read = match stream.read(&mut chunk) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && std::time::Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10));
                continue;
            }
            Err(error) => panic!("read fixture request body: {error}"),
        };
        if read == 0 {
            panic!("fixture request body was truncated");
        }
        body += read;
    }
}

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
            read_fixture_request(&mut stream);
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

fn parse_bool_query(value: &str, name: &str) -> bool {
    match value {
        "true" => true,
        "false" => false,
        _ => panic!("invalid boolean value for `{name}`: {value}"),
    }
}

fn parse_source(route: &str, query: &BTreeMap<String, String>) -> SignedUrlSource {
    match route {
        "/images/by-path" => SignedUrlSource::Path {
            path: query
                .get("path")
                .cloned()
                .expect("signed path requests must include `path`"),
            version: query.get("version").cloned(),
        },
        "/images/by-url" => SignedUrlSource::Url {
            url: query
                .get("url")
                .cloned()
                .expect("signed URL requests must include `url`"),
            version: query.get("version").cloned(),
        },
        _ => panic!("unsupported signed route: {route}"),
    }
}

fn parse_transform_options(query: &BTreeMap<String, String>) -> TransformOptions {
    TransformOptions {
        width: query
            .get("width")
            .map(|value| value.parse().expect("parse signed width")),
        height: query
            .get("height")
            .map(|value| value.parse().expect("parse signed height")),
        fit: query
            .get("fit")
            .map(|value| Fit::from_str(value).expect("parse signed fit")),
        position: query
            .get("position")
            .map(|value| Position::from_str(value).expect("parse signed position")),
        format: query
            .get("format")
            .map(|value| MediaType::from_str(value).expect("parse signed format")),
        quality: query
            .get("quality")
            .map(|value| value.parse().expect("parse signed quality")),
        optimize: query
            .get("optimize")
            .map(|value| OptimizeMode::from_str(value).expect("parse signed optimize"))
            .unwrap_or(OptimizeMode::None),
        target_quality: query
            .get("targetQuality")
            .map(|value| TargetQuality::from_str(value).expect("parse signed target quality")),
        background: query
            .get("background")
            .map(|value| Rgba8::from_hex(value).expect("parse signed background")),
        rotate: query
            .get("rotate")
            .map(|value| Rotation::from_str(value).expect("parse signed rotation"))
            .unwrap_or(Rotation::Deg0),
        auto_orient: query
            .get("autoOrient")
            .map(|value| parse_bool_query(value, "autoOrient"))
            .unwrap_or(true),
        strip_metadata: query
            .get("stripMetadata")
            .map(|value| parse_bool_query(value, "stripMetadata"))
            .unwrap_or(true),
        preserve_exif: query
            .get("preserveExif")
            .map(|value| parse_bool_query(value, "preserveExif"))
            .unwrap_or(false),
        blur: query
            .get("blur")
            .map(|value| value.parse().expect("parse signed blur")),
        sharpen: query
            .get("sharpen")
            .map(|value| value.parse().expect("parse signed sharpen")),
        crop: query
            .get("crop")
            .map(|value| CropRegion::from_str(value).expect("parse signed crop")),
        deadline: None,
    }
}

fn parse_watermark(query: &BTreeMap<String, String>) -> Option<SignedWatermarkParams> {
    query.get("watermarkUrl").map(|url| SignedWatermarkParams {
        url: url.clone(),
        position: query.get("watermarkPosition").cloned(),
        opacity: query
            .get("watermarkOpacity")
            .map(|value| value.parse().expect("parse signed watermark opacity")),
        margin: query
            .get("watermarkMargin")
            .map(|value| value.parse().expect("parse signed watermark margin")),
    })
}

fn is_supported_signed_query_name(route: &str, name: &str) -> bool {
    matches!(
        name,
        "keyId"
            | "expires"
            | "signature"
            | "version"
            | "width"
            | "height"
            | "fit"
            | "position"
            | "format"
            | "quality"
            | "optimize"
            | "targetQuality"
            | "background"
            | "rotate"
            | "autoOrient"
            | "stripMetadata"
            | "preserveExif"
            | "crop"
            | "blur"
            | "sharpen"
            | "watermarkUrl"
            | "watermarkPosition"
            | "watermarkOpacity"
            | "watermarkMargin"
            | "preset"
    ) || matches!(
        (route, name),
        ("/images/by-path", "path") | ("/images/by-url", "url")
    )
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

fn can_use_production_signer(method: &str, route: &str, query: &BTreeMap<String, String>) -> bool {
    if method != "GET"
        || !query
            .keys()
            .all(|name| is_supported_signed_query_name(route, name))
    {
        return false;
    }

    let has_supported_source = match route {
        "/images/by-path" => query.contains_key("path"),
        "/images/by-url" => query.contains_key("url"),
        _ => false,
    };
    if !has_supported_source {
        return false;
    }

    let has_orphaned_watermark_params = query.contains_key("watermarkPosition")
        || query.contains_key("watermarkOpacity")
        || query.contains_key("watermarkMargin");
    !has_orphaned_watermark_params || query.contains_key("watermarkUrl")
}

pub fn signed_target_with_method(
    method: &str,
    route: &str,
    query: BTreeMap<String, String>,
    authority: &str,
    secret: &str,
) -> String {
    if !can_use_production_signer(method, route, &query) {
        let mut query = query;
        query.insert(
            "signature".to_string(),
            sign_public_query(method, authority, route, &query, secret),
        );
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in query {
            serializer.append_pair(&name, &value);
        }
        return format!("{route}?{}", serializer.finish());
    }

    let key_id = query
        .get("keyId")
        .expect("signed requests must include `keyId`");
    let expires = query
        .get("expires")
        .expect("signed requests must include `expires`")
        .parse()
        .expect("parse signed expires");
    let options = parse_transform_options(&query);
    let source = parse_source(route, &query);
    let watermark = parse_watermark(&query);
    let preset = query.get("preset").map(String::as_str);

    let signed_url = sign_public_url(
        &format!("http://{authority}"),
        source,
        &options,
        key_id,
        secret,
        expires,
        watermark.as_ref(),
        preset,
    )
    .expect("generate signed target via production signer");
    let parsed = Url::parse(&signed_url).expect("parse signed target");
    match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    }
}

pub fn signed_target(
    route: &str,
    query: BTreeMap<String, String>,
    authority: &str,
    secret: &str,
) -> String {
    signed_target_with_method("GET", route, query, authority, secret)
}

pub fn send_signed_get(addr: SocketAddr, target: &str, authority: &str) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let req = format!("GET {target} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).expect("write request");
    stream.flush().ok();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let raw_str = String::from_utf8_lossy(&raw);

    let header_end = raw_str.find("\r\n\r\n").unwrap_or(raw.len());
    let header = raw_str[..header_end].to_string();
    let body = raw[(header_end + 4).min(raw.len())..].to_vec();
    (header, body)
}

pub fn status_code(header: &str) -> u16 {
    header
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
