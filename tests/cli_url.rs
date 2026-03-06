use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use truss::{MediaType, RawArtifact, sniff_artifact};

fn png_bytes() -> Vec<u8> {
    let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 4, 3, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

fn temp_file_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    std::env::temp_dir().join(format!("truss-integration-{name}-{unique}.bin"))
}

fn spawn_http_server(
    body: Vec<u8>,
    content_type: &'static str,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("server addr");
    let url = format!("http://{addr}/image");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).expect("write headers");
        stream.write_all(&body).expect("write body");
        stream.flush().expect("flush response");
    });

    (url, handle)
}

#[test]
fn inspect_url_reads_remote_png() {
    let (url, handle) = spawn_http_server(png_bytes(), "image/png");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("inspect")
        .arg("--url")
        .arg(url)
        .output()
        .expect("run truss inspect");

    handle.join().expect("join server thread");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"format\": \"png\""));
    assert!(stdout.contains("\"width\": 4"));
    assert!(stdout.contains("\"height\": 3"));
}

#[test]
fn convert_url_writes_a_local_output_file() {
    let (url, handle) = spawn_http_server(png_bytes(), "image/png");
    let output_path = temp_file_path("convert-url-output").with_extension("jpg");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("--url")
        .arg(url)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run truss convert");

    handle.join().expect("join server thread");

    assert!(output.status.success(), "{output:?}");

    let bytes = fs::read(&output_path).expect("read converted output");
    let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff converted output");
    let _ = fs::remove_file(&output_path);

    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn convert_url_can_infer_avif_output_from_the_file_extension() {
    let (url, handle) = spawn_http_server(png_bytes(), "image/png");
    let output_path = temp_file_path("convert-url-output-avif").with_extension("avif");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("--url")
        .arg(url)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run truss convert");

    handle.join().expect("join server thread");

    assert!(output.status.success(), "{output:?}");

    let bytes = fs::read(&output_path).expect("read converted output");
    let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff converted output");
    let _ = fs::remove_file(&output_path);

    assert_eq!(artifact.media_type, MediaType::Avif);
}
