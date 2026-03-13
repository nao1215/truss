mod common;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use truss::{MediaType, RawArtifact, ServerConfig, sniff_artifact};
use url::Url;

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

#[test]
fn sign_command_generates_a_working_public_path_url() {
    let storage_root = common::temp_dir("server");
    fs::write(storage_root.join("image.png"), common::png_bytes()).expect("write source fixture");
    let (addr, handle) = common::spawn_server(
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

    let (header, content_type, body) = common::split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(artifact.media_type, MediaType::Jpeg);
}
