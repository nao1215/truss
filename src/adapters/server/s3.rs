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
    runtime: tokio::runtime::Runtime,
}

impl std::fmt::Debug for S3Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Context")
            .field("default_bucket", &self.default_bucket)
            .field("client", &"..")
            .finish()
    }
}

/// The storage backend that determines how `Path`-based public GET requests are
/// resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// Source images live on the local filesystem under `storage_root`.
    Filesystem,
    /// Source images live in an S3-compatible bucket.
    S3,
}

impl StorageBackend {
    /// Parses the `TRUSS_STORAGE_BACKEND` environment variable value.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "filesystem" | "fs" | "local" => Ok(Self::Filesystem),
            "s3" => Ok(Self::S3),
            _ => Err(format!(
                "unknown storage backend `{value}` (expected `filesystem` or `s3`)"
            )),
        }
    }
}

/// Builds the S3 client from the default AWS SDK environment (`AWS_REGION`,
/// `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optionally
/// `AWS_ENDPOINT_URL` for S3-compatible services like MinIO).
pub(super) fn build_s3_context(default_bucket: String) -> Result<S3Context, std::io::Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let sdk_config = runtime.block_on(aws_config::load_defaults(
        aws_config::BehaviorVersion::latest(),
    ));
    let client = aws_sdk_s3::Client::new(&sdk_config);
    Ok(S3Context {
        client,
        default_bucket,
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
) -> Result<Vec<u8>, HttpResponse> {
    validate_s3_key(key)?;

    let result = s3
        .runtime
        .block_on(async { s3.client.get_object().bucket(bucket).key(key).send().await });

    match result {
        Ok(output) => {
            if let Some(len) = output.content_length()
                && len as u64 > MAX_SOURCE_BYTES
            {
                return Err(payload_too_large_response(
                    "S3 object exceeds the source size limit",
                ));
            }

            let body = s3
                .runtime
                .block_on(async { output.body.collect().await })
                .map_err(|e| {
                    bad_gateway_response(&format!("failed to read S3 object body: {e}"))
                })?;
            let bytes = body.into_bytes().to_vec();
            if bytes.len() as u64 > MAX_SOURCE_BYTES {
                return Err(payload_too_large_response(
                    "S3 object exceeds the source size limit",
                ));
            }
            Ok(bytes)
        }
        Err(err) => Err(map_s3_get_object_error(err)),
    }
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
    for segment in key.split('/') {
        if segment == ".." {
            return Err(bad_request_response(
                "S3 key must not contain path traversal segments (..)",
            ));
        }
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
            bad_gateway_response("object storage returned an error")
        }
        _ => bad_gateway_response("object storage request failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_backend_parse() {
        assert_eq!(
            StorageBackend::parse("filesystem").unwrap(),
            StorageBackend::Filesystem
        );
        assert_eq!(
            StorageBackend::parse("fs").unwrap(),
            StorageBackend::Filesystem
        );
        assert_eq!(
            StorageBackend::parse("local").unwrap(),
            StorageBackend::Filesystem
        );
        assert_eq!(StorageBackend::parse("s3").unwrap(), StorageBackend::S3);
        assert_eq!(StorageBackend::parse("S3").unwrap(), StorageBackend::S3);
        assert!(StorageBackend::parse("gcs").is_err());
        assert!(StorageBackend::parse("").is_err());
    }

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
    fn test_validate_s3_key_rejects_traversal() {
        assert!(validate_s3_key("../etc/passwd").is_err());
        assert!(validate_s3_key("images/../secret").is_err());
        assert!(validate_s3_key("..").is_err());
        // Segments that merely contain dots are fine
        assert!(validate_s3_key("a..b/file.jpg").is_ok());
        assert!(validate_s3_key(".hidden/file.jpg").is_ok());
    }
}
