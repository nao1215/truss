//! Integration tests that exercise the GCS storage backend against
//! [fake-gcs-server](https://github.com/fsouza/fake-gcs-server) running in Docker.
//!
//! These tests are `#[ignore]`d by default and require:
//!
//! ```bash
//! docker run -d --name fake-gcs-server -p 4443:4443 \
//!   fsouza/fake-gcs-server -scheme http -port 4443
//! ```
//!
//! Run with:
//!
//! ```bash
//! cargo test --features gcs --test gcs_integration -- --ignored
//! ```

#![cfg(feature = "gcs")]

use hmac::{Hmac, Mac};
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use truss::{
    GcsContext, MediaType, RawArtifact, ServerConfig, build_gcs_context, serve_once_with_config,
    sniff_artifact,
};

/// fake-gcs-server default HTTP endpoint.
const GCS_MOCK_ENDPOINT: &str = "http://localhost:4443";
/// Bucket created for each test.
const TEST_BUCKET: &str = "truss-gcs-integration-test";
/// Signed URL credentials used across all tests.
const KEY_ID: &str = "gcs-mock-dev";
const SECRET: &str = "gcs-mock-secret";
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

/// Build a [`GcsContext`] pointing at the local fake-gcs-server instance.
fn gcs_mock_context() -> GcsContext {
    // SAFETY: These tests are `#[ignore]`d and run sequentially via
    // `--test-threads=1` in practice. The env vars are set before any
    // multi-threaded work begins.
    unsafe {
        std::env::set_var("TRUSS_GCS_ENDPOINT", GCS_MOCK_ENDPOINT);
        // fake-gcs-server does not require authentication.
        std::env::set_var("GOOGLE_APPLICATION_CREDENTIALS_JSON", "{}");
    }

    build_gcs_context(TEST_BUCKET.to_string()).expect("build gcs context for fake-gcs-server")
}

/// Create a persistent temp directory that is not automatically deleted.
fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("truss-gcs-integration-{name}-{unique}"));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

/// Build a [`ServerConfig`] with GCS backend and signed URL credentials.
fn gcs_mock_server_config(ctx: GcsContext, storage: &std::path::Path) -> ServerConfig {
    ServerConfig::new(storage.to_path_buf(), None)
        .with_signed_url_credentials(KEY_ID, SECRET)
        .with_gcs_context(ctx)
}

/// PUT an object into the fake-gcs-server bucket via its REST API.
///
/// fake-gcs-server accepts uploads via the GCS JSON API.
fn put_object(key: &str, body: Vec<u8>) {
    // Create bucket first (idempotent for fake-gcs-server).
    let create_bucket_url = format!("{GCS_MOCK_ENDPOINT}/storage/v1/b");
    let bucket_payload = format!(r#"{{"name":"{TEST_BUCKET}"}}"#);
    let _ = ureq::post(&create_bucket_url)
        .header("Content-Type", "application/json")
        .send(bucket_payload.as_bytes());

    // Upload object via the upload endpoint.
    let upload_url = format!(
        "{GCS_MOCK_ENDPOINT}/upload/storage/v1/b/{TEST_BUCKET}/o?uploadType=media&name={key}"
    );
    ureq::post(&upload_url)
        .header("Content-Type", "application/octet-stream")
        .send(body.as_slice())
        .expect("upload object to fake-gcs-server");
}

fn spawn_server(
    config: ServerConfig,
) -> (SocketAddr, std::thread::JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = std::thread::spawn(move || serve_once_with_config(listener, &config));
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

/// Upload an object to fake-gcs-server, then request it through the signed
/// public by-path endpoint and verify that truss transforms and returns a
/// valid image.
#[test]
#[ignore]
fn gcs_mock_put_then_get_by_path() {
    let ctx = gcs_mock_context();
    put_object("photos/red.png", tiny_png());

    let storage = temp_dir("get-by-path");
    let config = gcs_mock_server_config(ctx, &storage);
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

/// Request a key that does not exist in fake-gcs-server → expect 404.
#[test]
#[ignore]
fn gcs_mock_nonexistent_key_returns_404() {
    let ctx = gcs_mock_context();
    let storage = temp_dir("nonexistent-key");
    let config = gcs_mock_server_config(ctx, &storage);
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

/// Upload a second object and retrieve it to verify independent keys work.
#[test]
#[ignore]
fn gcs_mock_multiple_objects() {
    let ctx = gcs_mock_context();
    put_object("images/blue.png", tiny_png());
    put_object("images/green.png", tiny_png());

    let storage = temp_dir("multiple-objects");
    let config = gcs_mock_server_config(ctx, &storage);
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
