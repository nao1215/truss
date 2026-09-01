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

/// `truss sign` refuses an option set the server refuses under every input, rather than
/// minting a URL that is valid as a signature and dead as a request.
///
/// A signed URL is normally written somewhere other than where it is fetched, so a
/// refusal that arrives at request time arrives without the process that produced it.
/// `@nao1215/truss-url-signer` has always refused these at the call site; this is the
/// signer inside the binary catching up.
#[test]
fn sign_command_refuses_options_the_server_would_always_refuse() {
    let cases: [(&[&str], &str); 4] = [
        (&["--fit", "cover"], "fit requires both width and height"),
        (
            &["--position", "center"],
            "position requires both width and height",
        ),
        (&["--quality", "101"], "quality must be between 1 and 100"),
        (&["--width", "0"], "width must be greater than zero"),
    ];

    for (args, message) in cases {
        let mut command = Command::new(env!("CARGO_BIN_EXE_truss"));
        command
            .arg("sign")
            .arg("--base-url")
            .arg("https://cdn.example.com")
            .arg("--path")
            .arg("/image.png")
            .arg("--key-id")
            .arg("public-dev")
            .arg("--secret")
            .arg("secret-value")
            .arg("--expires")
            .arg("4102444800");
        for arg in args {
            command.arg(arg);
        }
        let output = command.output().expect("run truss sign");
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        assert!(
            !output.status.success(),
            "{args:?} must not produce a URL: {stdout}"
        );
        assert!(
            stdout.trim().is_empty(),
            "{args:?} must write nothing a caller could pipe onward: {stdout}"
        );
        assert!(
            stderr.contains(message),
            "{args:?} must name the rule it broke: {stderr}"
        );
        assert!(
            stderr.contains("(invalid-options)"),
            "{args:?} must report the class the same options get everywhere else: {stderr}"
        );
    }
}
