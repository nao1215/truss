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
        "expires": 4_102_444_800_u64,
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
        "expires": 4_102_444_800_u64,
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

    let mut options = TransformOptions::default();
    options.width = Some(1200);
    options.height = Some(628);
    options.fit = Some(Fit::Cover);
    options.position = Some(Position::Top);
    options.format = Some(MediaType::Webp);
    options.optimize = OptimizeMode::Lossy;
    options.target_quality = Some(
        "psnr:41"
            .parse::<TargetQuality>()
            .expect("parse target quality"),
    );
    options.background = Some(Rgba8::from_hex("ffffff").expect("parse color"));
    options.rotate = Rotation::DEG_180;
    options.strip_metadata = false;
    options.crop = Some("0,0,1200,628".parse().expect("parse crop"));
    options.sharpen = Some(1.25);
    options.grayscale = true;
    options.without_enlargement = true;

    let mut watermark = SignedWatermarkParams::new("https://cdn.example.com/logo.png");
    watermark.position = Some("bottom-right".to_string());
    watermark.opacity = Some(70);
    watermark.margin = Some(24);

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
        1_900_000_000,
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
            "grayscale": true,
            "withoutEnlargement": true,
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
        "expires": 1_900_000_000,
        "method": "HEAD",
    }));

    assert_eq!(js_signed_url, rust_signed_url);
}

/// The two official signers refuse the same option sets.
///
/// They already agree on the canonical string they produce for a valid request. What
/// drifted is what they accept: the npm package validated the request-invariant matrix
/// while `truss sign` serialized whatever it was handed, so the CLI minted URLs that
/// answer 400 at request time, long after the process that wrote them has gone.
#[test]
fn both_signers_refuse_the_same_always_invalid_option_sets() {
    if !node_is_available() {
        eprintln!("skipping: node is not available");
        return;
    }

    let cases: [(&[&str], serde_json::Value, &str); 4] = [
        (
            &["--fit", "cover"],
            json!({"fit": "cover"}),
            "fit requires both width and height",
        ),
        (
            &["--position", "center"],
            json!({"position": "center"}),
            "position requires both width and height",
        ),
        (
            &["--quality", "101"],
            json!({"quality": 101}),
            "quality must be between 1 and 100",
        ),
        (
            &["--width", "0"],
            json!({"width": 0}),
            "width must be greater than zero",
        ),
    ];

    for (cli_args, transforms, message) in cases {
        let node = run_typescript_signer(json!({
            "baseUrl": "https://cdn.example.com",
            "source": {"kind": "path", "path": "/image.png"},
            "transforms": transforms,
            "keyId": "public-dev",
            "secret": "secret-value",
            "expires": 4_102_444_800_u64,
        }));
        assert!(
            !node.status.success(),
            "the npm signer must refuse {cli_args:?}: {}",
            String::from_utf8_lossy(&node.stdout)
        );
        assert!(
            String::from_utf8_lossy(&node.stderr).contains(message),
            "the npm signer names the rule for {cli_args:?}: {}",
            String::from_utf8_lossy(&node.stderr)
        );

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
        for arg in cli_args {
            command.arg(arg);
        }
        let cli = command.output().expect("run truss sign");
        assert!(
            !cli.status.success(),
            "truss sign must refuse {cli_args:?}: {}",
            String::from_utf8_lossy(&cli.stdout)
        );
        assert!(
            String::from_utf8_lossy(&cli.stderr).contains(message),
            "truss sign names the same rule for {cli_args:?}: {}",
            String::from_utf8_lossy(&cli.stderr)
        );
    }
}

/// The credentials and the source neither signer should mint a URL for.
///
/// A server refuses to start with an empty key id or secret in `TRUSS_SIGNING_KEYS`, and
/// the by-path route refuses an empty `path`, so a URL carrying one is answered 400 or 401
/// for as long as it exists. Each signer says so in its own vocabulary, `--key-id` on the
/// command line and `keyId` in JavaScript, which is why the messages are asserted per side
/// rather than shared.
#[test]
fn both_signers_refuse_credentials_and_a_source_no_server_accepts() {
    if !node_is_available() {
        eprintln!("skipping: node is not available");
        return;
    }

    let cases: [(&str, &str, &str, &str, &str); 3] = [
        ("--key-id", "", "keyId", "key id must not be empty", "keyId"),
        (
            "--secret",
            "",
            "secret",
            "secret must not be empty",
            "secret must be a non-empty string",
        ),
        ("--path", "", "path", "path must not be empty", "path"),
    ];

    for (flag, value, field, cli_message, node_message) in cases {
        let mut input = json!({
            "baseUrl": "https://cdn.example.com",
            "source": {"kind": "path", "path": "/image.png"},
            "keyId": "public-dev",
            "secret": "secret-value",
            "expires": 4_102_444_800_u64,
        });
        if field == "path" {
            input["source"]["path"] = json!(value);
        } else {
            input[field] = json!(value);
        }

        let node = run_typescript_signer(input);
        assert!(
            !node.status.success(),
            "the npm signer must refuse an empty {field}: {}",
            String::from_utf8_lossy(&node.stdout)
        );
        assert!(
            String::from_utf8_lossy(&node.stderr).contains(node_message),
            "the npm signer names the field for an empty {field}: {}",
            String::from_utf8_lossy(&node.stderr)
        );

        let mut arguments = vec![
            ("--base-url", "https://cdn.example.com"),
            ("--path", "/image.png"),
            ("--key-id", "public-dev"),
            ("--secret", "secret-value"),
            ("--expires", "4102444800"),
        ];
        for argument in &mut arguments {
            if argument.0 == flag {
                argument.1 = value;
            }
        }

        let mut command = Command::new(env!("CARGO_BIN_EXE_truss"));
        command.arg("sign");
        for (name, argument) in arguments {
            command.arg(name).arg(argument);
        }
        let cli = command.output().expect("run truss sign");
        assert!(
            !cli.status.success(),
            "truss sign must refuse {flag} '': {}",
            String::from_utf8_lossy(&cli.stdout)
        );
        assert!(
            String::from_utf8_lossy(&cli.stderr).contains(cli_message),
            "truss sign names the rule for {flag} '': {}",
            String::from_utf8_lossy(&cli.stderr)
        );
        assert!(
            cli.stdout.is_empty(),
            "truss sign writes no URL when it refuses"
        );
    }
}

/// A base URL with a path prefix is a deployment behind a proxy that serves truss under
/// it, and both signers have to emit the same URL for it, with the same signature the
/// prefix-free base URL produces.
#[test]
fn both_signers_keep_a_path_in_the_base_url() {
    if !node_is_available() {
        eprintln!("skipping: node is not available");
        return;
    }

    let input = |base_url: &str| {
        json!({
            "baseUrl": base_url,
            "source": {"kind": "path", "path": "/image.png"},
            "keyId": "public-dev",
            "secret": "secret-value",
            "expires": 4_102_444_800_u64,
        })
    };

    let node_prefixed = sign_with_typescript_package(input("https://cdn.example.com/img"));
    let node_plain = sign_with_typescript_package(input("https://cdn.example.com"));

    let rust_prefixed = sign_public_url_with_method(
        "GET",
        "https://cdn.example.com/img",
        SignedUrlSource::Path {
            path: "/image.png".to_string(),
            version: None,
        },
        &TransformOptions::default(),
        "public-dev",
        "secret-value",
        4_102_444_800,
        None,
        None,
    )
    .expect("sign with the Rust signer");

    assert_eq!(node_prefixed, rust_prefixed);
    assert_eq!(
        Url::parse(&rust_prefixed).expect("parse").path(),
        "/img/images/by-path"
    );

    let signature = |url: &str| {
        Url::parse(url)
            .expect("parse")
            .query_pairs()
            .find(|(name, _)| name == "signature")
            .expect("a signature")
            .1
            .into_owned()
    };
    assert_eq!(
        signature(&node_prefixed),
        signature(&node_plain),
        "the canonical string carries the endpoint path, not the base URL's"
    );
}

/// The command line signs the same HEAD URL the npm signer does.
///
/// The signature covers the HTTP method, so a URL signed for GET is answered 401 for a HEAD
/// and the two are separate URLs over one transform. The npm signer has taken a `method`
/// since it shipped and `truss sign` had no way to ask, which is the drift this pins: the
/// two signers have to agree on the HEAD URL the way the rest of this file pins the GET one.
#[test]
fn both_signers_produce_the_same_head_url() {
    if !node_is_available() {
        eprintln!("skipping: node is not available");
        return;
    }

    let mut cli = Command::new(env!("CARGO_BIN_EXE_truss"));
    cli.arg("sign")
        .arg("--base-url")
        .arg("https://images.example.com")
        .arg("--path")
        .arg("image.png")
        .arg("--key-id")
        .arg("public-demo")
        .arg("--secret")
        .arg("secret-value")
        .arg("--expires")
        .arg("1900000000")
        .arg("--width")
        .arg("800")
        .arg("--format")
        .arg("webp")
        .arg("--method")
        .arg("HEAD");
    let output = cli.output().expect("run truss sign --method HEAD");
    assert!(
        output.status.success(),
        "truss sign --method HEAD must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cli_url = String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_string();

    let js_url = sign_with_typescript_package(json!({
        "baseUrl": "https://images.example.com",
        "source": { "kind": "path", "path": "image.png" },
        "transforms": { "width": 800, "format": "webp" },
        "keyId": "public-demo",
        "secret": "secret-value",
        "expires": 1_900_000_000u64,
        "method": "HEAD",
    }));

    assert_eq!(cli_url, js_url);

    // The GET URL over the same transform is a different URL, which is the whole reason the
    // flag has to exist rather than the method being assumed.
    let mut get_cli = Command::new(env!("CARGO_BIN_EXE_truss"));
    get_cli
        .arg("sign")
        .arg("--base-url")
        .arg("https://images.example.com")
        .arg("--path")
        .arg("image.png")
        .arg("--key-id")
        .arg("public-demo")
        .arg("--secret")
        .arg("secret-value")
        .arg("--expires")
        .arg("1900000000")
        .arg("--width")
        .arg("800")
        .arg("--format")
        .arg("webp");
    let get_url = String::from_utf8(get_cli.output().expect("run truss sign").stdout)
        .expect("utf-8")
        .trim()
        .to_string();
    assert_ne!(cli_url, get_url);
}

/// A method the signed routes do not serve is refused where the URL is minted.
///
/// `truss sign --method POST` could only produce a URL that never verifies, and the process
/// that could report the mistake is gone by the time it is fetched, which is the reason
/// `signing_input_error` refuses an empty key id here rather than at request time.
#[test]
fn the_cli_refuses_a_method_the_signed_routes_do_not_serve() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_truss"));
    command
        .arg("sign")
        .arg("--base-url")
        .arg("https://images.example.com")
        .arg("--path")
        .arg("image.png")
        .arg("--key-id")
        .arg("public-demo")
        .arg("--secret")
        .arg("secret-value")
        .arg("--expires")
        .arg("1900000000")
        .arg("--method")
        .arg("POST");
    let output = command.output().expect("run truss sign --method POST");
    assert!(
        !output.status.success(),
        "truss sign must refuse an unsupported method: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unsupported signed URL method `POST`"),
        "the message names the method: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
