use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use truss::{MediaType, RawArtifact, ServerConfig, serve_once_with_config, sniff_artifact};
use url::Url;

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
    let path = std::env::temp_dir().join(format!("truss-cli-sign-{name}-{unique}"));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn spawn_server(config: ServerConfig) -> (SocketAddr, thread::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = thread::spawn(move || serve_once_with_config(listener, &config));

    (addr, handle)
}

fn send_get_request(url: &str) -> Vec<u8> {
    let url = Url::parse(url).expect("parse signed URL");
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().expect("host")),
        None => url.host_str().expect("host").to_string(),
    };
    let target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_string(),
    };
    let mut stream = TcpStream::connect(host.as_str()).expect("connect to test server");
    let request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
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
fn sign_command_generates_a_working_public_path_url() {
    let storage_root = temp_dir("server");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("sign")
        .arg("--base-url")
        .arg(format!("http://{addr}"))
        .arg("--path")
        .arg("/image.png")
        .arg("--key-id")
        .arg("public-dev")
        .arg("--secret")
        .arg("secret-value")
        .arg("--expires")
        .arg("4102444800")
        .arg("--format")
        .arg("jpeg")
        .output()
        .expect("run truss sign");

    assert!(output.status.success(), "{output:?}");
    let signed_url = String::from_utf8(output.stdout)
        .expect("utf8 stdout")
        .trim()
        .to_string();

    let response = send_get_request(&signed_url);

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
