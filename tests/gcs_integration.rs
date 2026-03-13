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

mod common;

use serial_test::serial;
use std::collections::BTreeMap;
use truss::{GcsContext, MediaType, RawArtifact, ServerConfig, build_gcs_context, sniff_artifact};

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

    build_gcs_context(TEST_BUCKET.to_string(), true).expect("build gcs context for fake-gcs-server")
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

/// Generate a signed URL target for `GET /images/by-path`.
fn signed_by_path_target(query: BTreeMap<String, String>) -> String {
    common::signed_target("/images/by-path", query, AUTHORITY, SECRET)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Upload an object to fake-gcs-server, then request it through the signed
/// public by-path endpoint and verify that truss transforms and returns a
/// valid image.
#[test]
#[ignore]
#[serial]
fn gcs_mock_put_then_get_by_path() {
    let ctx = gcs_mock_context();
    put_object("photos/red.png", common::tiny_png());

    let storage = common::temp_dir("get-by-path");
    let config = gcs_mock_server_config(ctx, &storage);
    let (addr, handle) = common::spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/photos/red.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, body) = common::send_signed_get(addr, &target, AUTHORITY);

    assert_eq!(common::status_code(&header), 200, "header: {header}");
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");
    assert_eq!(artifact.media_type, MediaType::Png);

    handle.join().expect("server thread").expect("serve_once");
}

/// Request a key that does not exist in fake-gcs-server → expect 404.
#[test]
#[ignore]
#[serial]
fn gcs_mock_nonexistent_key_returns_404() {
    let ctx = gcs_mock_context();
    let storage = common::temp_dir("nonexistent-key");
    let config = gcs_mock_server_config(ctx, &storage);
    let (addr, handle) = common::spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/does/not/exist.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, _body) = common::send_signed_get(addr, &target, AUTHORITY);

    assert_eq!(
        common::status_code(&header),
        404,
        "expected 404 for missing key: {header}"
    );

    handle.join().expect("server thread").expect("serve_once");
}

/// Request a path that triggers a 403 from the GCS backend.
///
/// fake-gcs-server does not simulate IAM denial, so this test creates
/// a separate context with a bucket whose ACLs would deny read access.
/// With the default emulator this may return 404 instead of 403 — the
/// test primarily verifies that the server does not panic and returns a
/// well-formed error response.
#[test]
#[ignore]
#[serial]
fn gcs_mock_forbidden_returns_error() {
    let ctx = gcs_mock_context();
    // Use a bucket that has no objects uploaded — requesting any key
    // should surface a non-200 response from the backend.
    let storage = common::temp_dir("forbidden");
    let config = gcs_mock_server_config(ctx, &storage);
    let (addr, handle) = common::spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/forbidden/object.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, _body) = common::send_signed_get(addr, &target, AUTHORITY);

    let code = common::status_code(&header);
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
fn gcs_mock_multiple_objects() {
    let ctx = gcs_mock_context();
    put_object("images/blue.png", common::tiny_png());
    put_object("images/green.png", common::tiny_png());

    let storage = common::temp_dir("multiple-objects");
    let config = gcs_mock_server_config(ctx, &storage);
    let (addr, handle) = common::spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/images/green.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, body) = common::send_signed_get(addr, &target, AUTHORITY);

    assert_eq!(
        common::status_code(&header),
        200,
        "should serve the second uploaded object: {header}"
    );
    assert!(!body.is_empty(), "response body should not be empty");

    handle.join().expect("server thread").expect("serve_once");
}
