//! Integration tests that exercise the S3 storage backend against
//! [adobe/s3mock](https://github.com/adobe/S3Mock) running in Docker.
//!
//! These tests are `#[ignore]`d by default and require:
//!
//! ```bash
//! docker run -d --name s3mock -p 9090:9090 adobe/s3mock:latest
//! ```
//!
//! Run with:
//!
//! ```bash
//! cargo test --features s3 --test s3_integration -- --ignored
//! ```

#![cfg(feature = "s3")]

mod common;

use serial_test::serial;
use std::collections::BTreeMap;
use truss::{
    MediaType, RawArtifact, S3Context, ServerConfig, build_s3_context, sniff_artifact,
};

/// S3Mock default HTTP endpoint.
const S3MOCK_ENDPOINT: &str = "http://localhost:9090";
/// Bucket created for each test.
const TEST_BUCKET: &str = "truss-integration-test";
/// Signed URL credentials used across all tests.
const KEY_ID: &str = "s3mock-dev";
const SECRET: &str = "s3mock-secret";
const AUTHORITY: &str = "localhost";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build an [`S3Context`] pointing at the local s3mock instance with
/// path-style addressing enabled.
fn s3mock_context() -> S3Context {
    // SAFETY: These tests are `#[ignore]`d and run sequentially via
    // `--test-threads=1` in practice. The env vars are set before any
    // multi-threaded work begins.
    unsafe {
        std::env::set_var("AWS_ACCESS_KEY_ID", "test");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");
        std::env::set_var("AWS_REGION", "us-east-1");
        std::env::set_var("AWS_ENDPOINT_URL", S3MOCK_ENDPOINT);
        std::env::set_var("TRUSS_S3_FORCE_PATH_STYLE", "true");
    }

    build_s3_context(TEST_BUCKET.to_string(), true).expect("build s3 context for s3mock")
}

/// Build a [`ServerConfig`] with S3 backend and signed URL credentials.
fn s3mock_server_config(ctx: S3Context, storage: &std::path::Path) -> ServerConfig {
    ServerConfig::new(storage.to_path_buf(), None)
        .with_signed_url_credentials(KEY_ID, SECRET)
        .with_s3_context(ctx)
}

/// PUT an object into the s3mock bucket via the S3 client.
fn put_object(ctx: &S3Context, key: &str, body: Vec<u8>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let _ = ctx.client.create_bucket().bucket(TEST_BUCKET).send().await;
        ctx.client
            .put_object()
            .bucket(TEST_BUCKET)
            .key(key)
            .body(body.into())
            .send()
            .await
            .expect("put_object to s3mock");
    });
}

/// Generate a signed URL target for `GET /images/by-path`.
fn signed_by_path_target(query: BTreeMap<String, String>) -> String {
    common::signed_target("/images/by-path", query, AUTHORITY, SECRET)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Upload an object to s3mock, then request it through the signed public
/// by-path endpoint and verify that truss transforms and returns a valid image.
#[test]
#[ignore]
#[serial]
fn s3mock_put_then_get_by_path() {
    let ctx = s3mock_context();
    put_object(&ctx, "photos/red.png", common::tiny_png());

    let storage = common::temp_dir("get-by-path");
    let config = s3mock_server_config(ctx, &storage);
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

/// Request a key that does not exist in s3mock → expect 404.
#[test]
#[ignore]
#[serial]
fn s3mock_nonexistent_key_returns_404() {
    let ctx = s3mock_context();
    let storage = common::temp_dir("nonexistent-key");
    let config = s3mock_server_config(ctx, &storage);
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

/// Verify that force_path_style is effective: s3mock only supports
/// path-style addressing, so if virtual-hosted style were used the
/// request would fail with a DNS error instead of returning 200.
#[test]
#[ignore]
#[serial]
fn s3mock_force_path_style_works() {
    let ctx = s3mock_context();
    put_object(&ctx, "style-test/image.png", common::tiny_png());

    let storage = common::temp_dir("force-path-style");
    let config = s3mock_server_config(ctx, &storage);
    let (addr, handle) = common::spawn_server(config);

    let target = signed_by_path_target(BTreeMap::from([
        ("path".to_string(), "/style-test/image.png".to_string()),
        ("keyId".to_string(), KEY_ID.to_string()),
        ("expires".to_string(), "4102444800".to_string()),
        ("format".to_string(), "png".to_string()),
    ]));
    let (header, body) = common::send_signed_get(addr, &target, AUTHORITY);

    assert_eq!(
        common::status_code(&header),
        200,
        "path-style request should succeed against s3mock: {header}"
    );
    assert!(!body.is_empty(), "response body should not be empty");

    handle.join().expect("server thread").expect("serve_once");
}
