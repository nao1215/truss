use super::remote::MAX_SOURCE_BYTES;
use super::response::{
    HttpResponse, bad_gateway_response, bad_request_response, payload_too_large_response,
};

/// Shared S3 client state constructed once at startup and threaded through
/// [`super::ServerConfig`].  The client is cheaply cloneable (`Arc` internally)
/// and safe to share across worker threads.
///
/// A multi-threaded Tokio runtime is stored alongside the client so that
/// worker threads can call `runtime.block_on(...)` concurrently without
/// creating a new runtime per request.
pub struct S3Context {
    pub client: aws_sdk_s3::Client,
    pub default_bucket: String,
    /// The endpoint URL (e.g. `AWS_ENDPOINT_URL` for MinIO) used to construct
    /// the client, or `None` when the default AWS endpoint is used. Stored for
    /// cache-key isolation so that two S3-compatible services sharing a bucket
    /// name cannot poison each other's cached artifacts.
    pub endpoint_url: Option<String>,
    runtime: tokio::runtime::Runtime,
}

impl std::fmt::Debug for S3Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Context")
            .field("default_bucket", &self.default_bucket)
            .field("endpoint_url", &self.endpoint_url)
            .field("client", &"..")
            .finish()
    }
}

impl S3Context {
    /// Returns `true` if the configured bucket is reachable.
    ///
    /// Issues a GetObject for a key that is extremely unlikely to exist.
    /// Most service-level responses (NoSuchKey, AccessDenied) prove that S3
    /// accepted the request and the bucket exists, so they count as
    /// "reachable".  `NoSuchBucket` is explicitly treated as *unreachable*
    /// because it indicates a misconfigured bucket name.
    ///
    /// This deliberately avoids HeadBucket, which requires `s3:ListBucket` —
    /// a permission that read-only image-serving roles should not need.
    pub fn check_reachable(&self) -> bool {
        use aws_sdk_s3::error::SdkError;
        use std::time::Duration;

        let client = self.client.clone();
        let bucket = self.default_bucket.clone();
        self.runtime.block_on(async {
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                client
                    .get_object()
                    .bucket(&bucket)
                    .key("__truss_health_probe__")
                    .send(),
            )
            .await;

            match result {
                Ok(Ok(_)) => true,
                Ok(Err(SdkError::ServiceError(e))) => {
                    // NoSuchBucket means the bucket name is wrong — not reachable.
                    e.err().meta().code() != Some("NoSuchBucket")
                }
                _ => false,
            }
        })
    }
}

#[cfg(test)]
impl S3Context {
    pub(crate) fn for_test(default_bucket: &str, endpoint_url: Option<&str>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let sdk_config = runtime.block_on(aws_config::load_defaults(
            aws_config::BehaviorVersion::latest(),
        ));
        let client = aws_sdk_s3::Client::new(&sdk_config);
        S3Context {
            client,
            default_bucket: default_bucket.to_string(),
            endpoint_url: endpoint_url.map(|s| s.to_string()),
            runtime,
        }
    }
}

// StorageBackend has been moved to mod.rs to support multiple storage features.

/// Builds the S3 client from the default AWS SDK environment (`AWS_REGION`,
/// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optionally
/// `AWS_ENDPOINT_URL` for S3-compatible services like MinIO).
///
/// When `TRUSS_S3_FORCE_PATH_STYLE` is set to `1`, `true`, `yes`, or `on`,
/// the client uses path-style addressing (`http://endpoint/bucket/key`)
/// instead of virtual-hosted-style (`http://bucket.endpoint/key`). This is
/// required for most S3-compatible services (MinIO, LocalStack, adobe/s3mock).
pub fn build_s3_context(
    default_bucket: String,
    allow_insecure: bool,
) -> Result<S3Context, std::io::Error> {
    // Two Tokio workers: one drives the HTTP/TLS I/O while the other
    // ensures tokio::time::timeout timers fire even when a request stalls.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let sdk_config = runtime.block_on(aws_config::load_defaults(
        aws_config::BehaviorVersion::latest(),
    ));
    let endpoint_url = sdk_config.endpoint_url().map(|s| s.to_string());
    if let Some(ref url) = endpoint_url {
        super::remote::validate_backend_endpoint_url(url, "AWS_ENDPOINT_URL", allow_insecure)?;
    }
    let force_path_style = matches!(
        std::env::var("TRUSS_S3_FORCE_PATH_STYLE")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    );
    let s3_config = aws_sdk_s3::config::Builder::from(&sdk_config)
        .force_path_style(force_path_style)
        .build();
    let client = aws_sdk_s3::Client::from_conf(s3_config);
    Ok(S3Context {
        client,
        default_bucket,
        endpoint_url,
        runtime,
    })
}

/// Fetches an object from S3 and returns its body bytes.
///
/// Uses the shared multi-threaded Tokio runtime stored in [`S3Context`] so
/// that multiple worker threads can issue concurrent S3 requests without
/// creating a runtime per call.
pub(super) fn read_s3_source_bytes(
    bucket: &str,
    key: &str,
    s3: &S3Context,
    timeout_secs: u64,
) -> Result<Vec<u8>, HttpResponse> {
    validate_s3_key(key)?;

    s3.runtime.block_on(async {
        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let output = s3
                .client
                .get_object()
                .bucket(bucket)
                .key(key)
                .send()
                .await
                .map_err(map_s3_get_object_error)?;

            if let Some(len) = output.content_length()
                && len as u64 > MAX_SOURCE_BYTES
            {
                return Err(payload_too_large_response(
                    "S3 object exceeds the source size limit",
                ));
            }

            let capacity_hint = output
                .content_length()
                .map(|l| (l as usize).min(MAX_SOURCE_BYTES as usize + 1))
                .unwrap_or(0);

            use tokio::io::AsyncReadExt;
            let mut limited = output.body.into_async_read().take(MAX_SOURCE_BYTES + 1);
            let mut buf = Vec::with_capacity(capacity_hint);
            limited.read_to_end(&mut buf).await.map_err(|e| {
                super::stderr_write(&format!("s3 error: failed to read object body: {e}"));
                bad_gateway_response("failed to read S3 object body")
            })?;

            if buf.len() as u64 > MAX_SOURCE_BYTES {
                return Err(payload_too_large_response(
                    "S3 object exceeds the source size limit",
                ));
            }
            Ok(buf)
        })
        .await;
        match result {
            Ok(inner) => inner,
            Err(_) => {
                super::stderr_write(&format!(
                    "s3 error: download timed out after {timeout_secs}s"
                ));
                Err(bad_gateway_response("object storage download timed out"))
            }
        }
    })
}

/// Validates that an S3 key does not contain dangerous characters or
/// path-traversal sequences.
fn validate_s3_key(key: &str) -> Result<(), HttpResponse> {
    if key.is_empty() {
        return Err(bad_request_response("S3 key must not be empty"));
    }
    if key.contains('\0') || key.contains('\n') || key.contains('\r') {
        return Err(bad_request_response(
            "S3 key contains invalid characters (null, newline, or carriage return)",
        ));
    }
    if key.len() > 1024 {
        return Err(bad_request_response(
            "S3 key exceeds the maximum allowed length of 1024 bytes",
        ));
    }
    Ok(())
}

fn map_s3_get_object_error(
    err: aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::get_object::GetObjectError>,
) -> HttpResponse {
    use aws_sdk_s3::error::SdkError;

    match &err {
        SdkError::ServiceError(service_err) => {
            if service_err.err().is_no_such_key() {
                return super::response::not_found_response(
                    "source image was not found in object storage",
                );
            }
            // Surface 403 as a distinct error so operators can diagnose
            // IAM / KMS / bucket-policy issues.  Note: when the IAM role
            // lacks s3:ListBucket, AWS returns 403 for non-existent keys
            // instead of 404 — but we intentionally do NOT hide that here,
            // because the same 403 also fires for genuine access-denied on
            // existing objects, and masking it would make real permission
            // problems invisible.  The recommended fix is to grant
            // s3:ListBucket so that AWS returns proper 404 for missing keys.
            if service_err.raw().status().as_u16() == 403 {
                return super::response::forbidden_response(
                    "access denied by object storage — check IAM permissions",
                );
            }
            if service_err.raw().status().as_u16() == 401 {
                return bad_gateway_response(
                    "object storage authentication failed — check credentials",
                );
            }
            super::stderr_write(&format!("s3 error: {err}"));
            bad_gateway_response("object storage returned an error")
        }
        _ => {
            super::stderr_write(&format!("s3 error: {err}"));
            bad_gateway_response("object storage request failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_s3_key_valid() {
        assert!(validate_s3_key("images/photo.jpg").is_ok());
        assert!(validate_s3_key("a").is_ok());
        assert!(validate_s3_key("path/to/deep/object.png").is_ok());
    }

    #[test]
    fn test_validate_s3_key_rejects_empty() {
        assert!(validate_s3_key("").is_err());
    }

    #[test]
    fn test_validate_s3_key_rejects_null() {
        assert!(validate_s3_key("foo\0bar").is_err());
    }

    #[test]
    fn test_validate_s3_key_rejects_newline() {
        assert!(validate_s3_key("foo\nbar").is_err());
        assert!(validate_s3_key("foo\rbar").is_err());
    }

    #[test]
    fn test_validate_s3_key_rejects_too_long() {
        let long_key = "a".repeat(1025);
        assert!(validate_s3_key(&long_key).is_err());

        let max_key = "a".repeat(1024);
        assert!(validate_s3_key(&max_key).is_ok());
    }

    #[test]
    fn test_validate_s3_key_allows_dot_segments() {
        // S3 keys are opaque identifiers — ".." has no special meaning in
        // object storage, so we must not reject them.
        assert!(validate_s3_key("../etc/passwd").is_ok());
        assert!(validate_s3_key("images/../secret").is_ok());
        assert!(validate_s3_key("..").is_ok());
        assert!(validate_s3_key("a..b/file.jpg").is_ok());
        assert!(validate_s3_key(".hidden/file.jpg").is_ok());
    }

    // L-3: Unicode / special character key tests
    #[test]
    fn test_validate_s3_key_allows_unicode() {
        assert!(validate_s3_key("images/\u{5199}\u{771f}.jpg").is_ok());
        assert!(validate_s3_key("données/fichier.png").is_ok());
    }

    #[test]
    fn test_validate_s3_key_allows_special_chars() {
        assert!(validate_s3_key("path/to/file name.jpg").is_ok());
        assert!(validate_s3_key("a+b=c.jpg").is_ok());
        assert!(validate_s3_key("foo\tbar").is_ok());
    }

    // L-6: map_s3_get_object_error tests
    fn s3_service_error(
        err: aws_sdk_s3::operation::get_object::GetObjectError,
        status: u16,
    ) -> aws_sdk_s3::error::SdkError<aws_sdk_s3::operation::get_object::GetObjectError> {
        use aws_smithy_runtime_api::http::{Response, StatusCode};
        use aws_smithy_types::body::SdkBody;
        let raw = Response::new(StatusCode::try_from(status).unwrap(), SdkBody::empty());
        aws_sdk_s3::error::SdkError::service_error(err, raw)
    }

    #[test]
    fn test_map_s3_error_no_such_key_returns_not_found() {
        let err = aws_sdk_s3::operation::get_object::GetObjectError::NoSuchKey(
            aws_sdk_s3::types::error::NoSuchKey::builder().build(),
        );
        let resp = map_s3_get_object_error(s3_service_error(err, 404));
        assert_eq!(resp.status, "404 Not Found");
    }

    #[test]
    fn test_map_s3_error_403_returns_forbidden() {
        let err = aws_sdk_s3::operation::get_object::GetObjectError::unhandled("access denied");
        let resp = map_s3_get_object_error(s3_service_error(err, 403));
        assert_eq!(resp.status, "403 Forbidden");
    }

    #[test]
    fn test_map_s3_error_401_returns_bad_gateway() {
        let err = aws_sdk_s3::operation::get_object::GetObjectError::unhandled("unauthorized");
        let resp = map_s3_get_object_error(s3_service_error(err, 401));
        assert_eq!(resp.status, "502 Bad Gateway");
    }

    #[test]
    fn test_map_s3_error_500_returns_bad_gateway() {
        let err =
            aws_sdk_s3::operation::get_object::GetObjectError::unhandled("internal server error");
        let resp = map_s3_get_object_error(s3_service_error(err, 500));
        assert_eq!(resp.status, "502 Bad Gateway");
    }

    #[test]
    fn test_map_s3_error_non_service_returns_bad_gateway() {
        let err = aws_sdk_s3::error::SdkError::<
            aws_sdk_s3::operation::get_object::GetObjectError,
        >::dispatch_failure(
            aws_smithy_runtime_api::client::result::ConnectorError::other(
                "connection refused".into(),
                None,
            ),
        );
        let resp = map_s3_get_object_error(err);
        assert_eq!(resp.status, "502 Bad Gateway");
    }
}
