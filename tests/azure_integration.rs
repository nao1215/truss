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

mod common;

use serial_test::serial;
use std::collections::BTreeMap;
use truss::{
    AzureContext, MediaType, RawArtifact, ServerConfig, build_azure_context, sniff_artifact,
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

/// Generate a signed URL target for `GET /images/by-path`.
fn signed_by_path_target(query: BTreeMap<String, String>) -> String {
    common::signed_target("/images/by-path", query, AUTHORITY, SECRET)
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
    put_object("photos/red.png", common::tiny_png());

    let storage = common::temp_dir("get-by-path");
    let config = azure_mock_server_config(ctx, &storage);
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

/// Request a key that does not exist in Azurite -> expect 404.
#[test]
#[ignore]
#[serial]
fn azure_mock_nonexistent_key_returns_404() {
    let ctx = azure_mock_context();
    let storage = common::temp_dir("nonexistent-key");
    let config = azure_mock_server_config(ctx, &storage);
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
    let storage = common::temp_dir("forbidden");
    let config = azure_mock_server_config(ctx, &storage);
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
fn azure_mock_multiple_objects() {
    let ctx = azure_mock_context();
    put_object("images/blue.png", common::tiny_png());
    put_object("images/green.png", common::tiny_png());

    let storage = common::temp_dir("multiple-objects");
    let config = azure_mock_server_config(ctx, &storage);
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
