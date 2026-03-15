mod common;

use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::process::Output;
use std::time::Duration;
use truss::{
    Fit, MediaType, OptimizeMode, Position, RawArtifact, Rgba8, Rotation, ServerConfig,
    SignedUrlSource, SignedWatermarkParams, TargetQuality, TransformOptions,
    sign_public_url_with_method, sniff_artifact,
};
use url::Url;

fn node_is_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_typescript_signer(input: serde_json::Value) -> Output {
    let script = r#"
import { signPublicUrl } from "./packages/truss-url-signer/index.js";

const input = JSON.parse(process.env.TRUSS_SIGN_INPUT);
console.log(signPublicUrl(input));
"#;

    Command::new("node")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .arg("--input-type=module")
        .arg("-e")
        .arg(script)
        .env("TRUSS_SIGN_INPUT", input.to_string())
        .output()
        .expect("run TypeScript signer package")
}

fn sign_with_typescript_package(input: serde_json::Value) -> String {
    let output = run_typescript_signer(input);

    assert!(
        output.status.success(),
        "node signer failed: status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    String::from_utf8(output.stdout)
        .expect("utf8 stdout")
        .trim()
        .to_string()
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
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout for test server response");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout for test server request");
    let request = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    response
}

#[test]
fn typescript_signer_generates_a_working_public_path_url() {
    if !node_is_available() {
        eprintln!("skipping TypeScript signer test because `node` is unavailable");
        return;
    }

    let storage_root = common::temp_dir("ts-signer-path");
    fs::write(storage_root.join("image.png"), common::png_bytes()).expect("write source fixture");
    let (addr, handle) = common::spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value")
            .with_insecure_url_sources(true),
    );

    let signed_url = sign_with_typescript_package(json!({
        "baseUrl": format!("http://{addr}"),
        "source": {
            "kind": "path",
            "path": "/image.png",
        },
        "transforms": {
            "format": "jpeg",
            "width": 400,
        },
        "keyId": "public-dev",
        "secret": "secret-value",
        "expires": 4102444800u64,
    }));

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

#[test]
fn typescript_signer_generates_a_working_public_remote_url() {
    if !node_is_available() {
        eprintln!("skipping TypeScript signer test because `node` is unavailable");
        return;
    }

    let storage_root = common::temp_dir("ts-signer-url");
    let (fixture_url, fixture_handle) = common::spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        common::png_bytes(),
    )]);
    let (addr, handle) = common::spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value")
            .with_insecure_url_sources(true),
    );

    let signed_url = sign_with_typescript_package(json!({
        "baseUrl": format!("http://{addr}"),
        "source": {
            "kind": "url",
            "url": fixture_url,
            "version": "v1",
        },
        "transforms": {
            "format": "webp",
            "width": 256,
            "optimize": "lossy",
            "targetQuality": "ssim:0.98",
        },
        "keyId": "public-dev",
        "secret": "secret-value",
        "expires": 4102444800u64,
    }));

    let response = send_get_request(&signed_url);

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture_handle.join().expect("join fixture server");

    let (header, content_type, body) = common::split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/webp");
    assert_eq!(artifact.media_type, MediaType::Webp);
}

#[test]
fn typescript_signer_matches_rust_head_canonicalization_with_preset_and_watermark() {
    if !node_is_available() {
        eprintln!("skipping TypeScript signer test because `node` is unavailable");
        return;
    }

    let options = TransformOptions {
        width: Some(1200),
        height: Some(628),
        fit: Some(Fit::Cover),
        position: Some(Position::Top),
        format: Some(MediaType::Webp),
        optimize: OptimizeMode::Lossy,
        target_quality: Some(
            "psnr:41"
                .parse::<TargetQuality>()
                .expect("parse target quality"),
        ),
        background: Some(Rgba8::from_hex("ffffff").expect("parse color")),
        rotate: Rotation::Deg180,
        strip_metadata: false,
        crop: Some("0,0,1200,628".parse().expect("parse crop")),
        sharpen: Some(1.25),
        ..TransformOptions::default()
    };
    let watermark = SignedWatermarkParams {
        url: "https://cdn.example.com/logo.png".to_string(),
        position: Some("bottom-right".to_string()),
        opacity: Some(70),
        margin: Some(24),
    };

    let rust_signed_url = sign_public_url_with_method(
        "HEAD",
        "https://images.example.com",
        SignedUrlSource::Url {
            url: "https://origin.example.com/banner.png".to_string(),
            version: Some("v4".to_string()),
        },
        &options,
        "public-demo",
        "secret-value",
        1900000000,
        Some(&watermark),
        Some("social-card"),
    )
    .expect("generate signed URL via Rust signer");
    let js_signed_url = sign_with_typescript_package(json!({
        "baseUrl": "https://images.example.com",
        "source": {
            "kind": "url",
            "url": "https://origin.example.com/banner.png",
            "version": "v4",
        },
        "transforms": {
            "width": 1200,
            "height": 628,
            "fit": "cover",
            "position": "top",
            "format": "webp",
            "optimize": "lossy",
            "targetQuality": "psnr:41",
            "background": "ffffff",
            "rotate": 180,
            "stripMetadata": false,
            "crop": "0,0,1200,628",
            "sharpen": 1.25,
        },
        "watermark": {
            "url": "https://cdn.example.com/logo.png",
            "position": "bottom-right",
            "opacity": 70,
            "margin": 24,
        },
        "preset": "social-card",
        "keyId": "public-demo",
        "secret": "secret-value",
        "expires": 1900000000,
        "method": "HEAD",
    }));

    assert_eq!(js_signed_url, rust_signed_url);
}
