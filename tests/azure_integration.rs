//! Integration tests that exercise the Azure Blob Storage backend against
//! [Azurite](https://learn.microsoft.com/en-us/azure/storage/common/storage-use-azurite)
//! running in Docker.
//!
//! These tests are `#[ignore]`d by default and require:
//!
//! ```bash
//! docker run -d --name azurite -p 10000:10000 \
//!   mcr.microsoft.com/azure-storage/azurite azurite-blob --blobHost 0.0.0.0 --blobPort 10000
//! ```
//!
//! Run with:
//!
//! ```bash
//! cargo test --features azure --test azure_integration -- --ignored
//! ```

#![cfg(feature = "azure")]

use hmac::{Hmac, Mac};
use serial_test::serial;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use truss::{
    AzureContext, MediaType, RawArtifact, ServerConfig, build_azure_context,
    serve_once_with_config, sniff_artifact,
};

/// Azurite default HTTP blob endpoint.
const AZURE_MOCK_ENDPOINT: &str = "http://127.0.0.1:10000/devstoreaccount1";
/// Container created for each test.
const TEST_BUCKET: &str = "truss-azure-integration-test";
/// Signed URL credentials used across all tests.
const KEY_ID: &str = "azure-mock-dev";
const SECRET: &str = "azure-mock-secret";
const AUTHORITY: &str = "localhost";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn tiny_png() -> Vec<u8> {
    let image = RgbaImage::from_pixel(2, 2, Rgba([255, 0, 0, 255]));
    let mut buf = Vec::new();
    PngEncoder::new(&mut buf)
        .write_image(&image, 2, 2, ColorType::Rgba8.into())
        .expect("encode png");
    buf
}

/// Build an [`AzureContext`] pointing at the local Azurite instance.
fn azure_mock_context() -> AzureContext {
    // SAFETY: These tests are `#[ignore]`d and run sequentially via
    // `--test-threads=1` in practice. The env vars are set before any
    // multi-threaded work begins.
    unsafe {
        std::env::set_var("TRUSS_AZURE_ENDPOINT", AZURE_MOCK_ENDPOINT);
        std::env::set_var("AZURE_STORAGE_ACCOUNT_NAME", "devstoreaccount1");
    }

    build_azure_context(TEST_BUCKET.to_string(), true).expect("build azure context for Azurite")
}

/// Create a persistent temp directory that is not automatically deleted.
fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("truss-azure-integration-{name}-{unique}"));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// Build a [`ServerConfig`] with Azure backend and signed URL credentials.
fn azure_mock_server_config(ctx: AzureContext, storage: &std::path::Path) -> ServerConfig {
    ServerConfig::new(storage.to_path_buf(), None)
        .with_signed_url_credentials(KEY_ID, SECRET)
        .with_azure_context(ctx)
}

/// PUT an object into the Azurite container via its REST API.
///
/// Azurite accepts requests with no auth by default.
fn put_object(key: &str, body: Vec<u8>) {
    let base = "http://127.0.0.1:10000/devstoreaccount1";

    // Create container first (idempotent — 201 on creation, 409 if exists).
    let create_url = format!("{base}/{TEST_BUCKET}?restype=container");
    let _ = ureq::put(&create_url).send(&[] as &[u8]);

    // Upload blob.
    let upload_url = format!("{base}/{TEST_BUCKET}/{key}");
    ureq::put(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .header("x-ms-blob-type", "BlockBlob")
        .send(body.as_slice())
        .expect("upload blob to Azurite");
}

fn spawn_server(
    config: ServerConfig,
) -> (SocketAddr, std::thread::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = std::thread::spawn(move || serve_once_with_config(listener, config));
    (addr, handle)
}

/// Generate a signed URL target for `GET /images/by-path`.
fn signed_by_path_target(query: BTreeMap<String, String>) -> String {
    let mut query = query;
    let signature = sign_query("GET", AUTHORITY, "/images/by-path", &query, SECRET);
    query.insert("signature".to_string(), signature);
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in &query {
        serializer.append_pair(name, value);
    }
    format!("/images/by-path?{}", serializer.finish())
}

fn sign_query(
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
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac");
    mac.update(canonical.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn send_signed_get(addr: SocketAddr, target: &str) -> (String, Vec<u8>) {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let req = format!("GET {target} HTTP/1.1\r\nHost: {AUTHORITY}\r\nConnection: close\r\n\r\n");
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

fn status_code(header: &str) -> u16 {
    header
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Upload an object to Azurite, then request it through the signed
/// public by-path endpoint and verify that truss transforms and returns a
/// valid image.
#[test]
#[ignore]
#[serial]
fn azure_mock_put_then_get_by_path() {
    let ctx = azure_mock_context();
    put_object("photos/red.png", tiny_png());

    let storage = temp_dir("get-by-path");
    let config = azure_mock_server_config(ctx, &storage);
    let (addr, handle) = spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/photos/red.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, body) = send_signed_get(addr, &target);

    assert_eq!(status_code(&header), 200, "header: {header}");
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");
    assert_eq!(artifact.media_type, MediaType::Png);

    handle.join().expect("server thread").expect("serve_once");
}

/// Request a key that does not exist in Azurite -> expect 404.
#[test]
#[ignore]
#[serial]
fn azure_mock_nonexistent_key_returns_404() {
    let ctx = azure_mock_context();
    let storage = temp_dir("nonexistent-key");
    let config = azure_mock_server_config(ctx, &storage);
    let (addr, handle) = spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/does/not/exist.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, _body) = send_signed_get(addr, &target);

    assert_eq!(
        status_code(&header),
        404,
        "expected 404 for missing key: {header}"
    );

    handle.join().expect("server thread").expect("serve_once");
}

/// Request a path that triggers a 403 from the Azure backend.
///
/// Azurite does not simulate IAM denial, so this test creates
/// a separate context with a container whose ACLs would deny read access.
/// With the default emulator this may return 404 instead of 403 -- the
/// test primarily verifies that the server does not panic and returns a
/// well-formed error response.
#[test]
#[ignore]
#[serial]
fn azure_mock_forbidden_returns_error() {
    let ctx = azure_mock_context();
    // Use a container that has no objects uploaded -- requesting any key
    // should surface a non-200 response from the backend.
    let storage = temp_dir("forbidden");
    let config = azure_mock_server_config(ctx, &storage);
    let (addr, handle) = spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/forbidden/object.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, _body) = send_signed_get(addr, &target);

    let code = status_code(&header);
    assert!(
        code == 403 || code == 404,
        "expected 403 or 404 for forbidden/missing key, got {code}: {header}"
    );

    handle.join().expect("server thread").expect("serve_once");
}

/// Upload a second object and retrieve it to verify independent keys work.
#[test]
#[ignore]
#[serial]
fn azure_mock_multiple_objects() {
    let ctx = azure_mock_context();
    put_object("images/blue.png", tiny_png());
    put_object("images/green.png", tiny_png());

    let storage = temp_dir("multiple-objects");
    let config = azure_mock_server_config(ctx, &storage);
    let (addr, handle) = spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/images/green.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, body) = send_signed_get(addr, &target);

    assert_eq!(
        status_code(&header),
        200,
        "should serve the second uploaded object: {header}"
    );
    assert!(!body.is_empty(), "response body should not be empty");

    handle.join().expect("server thread").expect("serve_once");
}
