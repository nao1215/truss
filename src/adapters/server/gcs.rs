use super::remote::MAX_SOURCE_BYTES;
use super::response::{
    HttpResponse, bad_gateway_response, bad_request_response, not_found_response,
    payload_too_large_response,
};

/// Shared GCS client state constructed once at startup and threaded through
/// [`super::ServerConfig`].  The client is cheaply cloneable (`Arc` internally)
/// and safe to share across worker threads.
///
/// A multi-threaded Tokio runtime is stored alongside the client so that
/// worker threads can call `runtime.block_on(...)` concurrently without
/// creating a new runtime per request.
pub struct GcsContext {
    pub client: google_cloud_storage::client::Storage,
    pub default_bucket: String,
    /// The endpoint URL used to construct the client, or `None` when the
    /// default GCS endpoint is used. Stored for cache-key isolation.
    pub endpoint_url: Option<String>,
    runtime: tokio::runtime::Runtime,
}

impl std::fmt::Debug for GcsContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcsContext")
            .field("default_bucket", &self.default_bucket)
            .field("endpoint_url", &self.endpoint_url)
            .field("client", &"..")
            .finish()
    }
}

impl GcsContext {
    /// Returns `true` if the configured bucket is reachable.
    ///
    /// Issues a read_object for a key that is extremely unlikely to exist.
    /// Most service-level responses (not-found, access-denied) prove that GCS
    /// accepted the request and the bucket exists, so they count as
    /// "reachable".
    pub fn check_reachable(&self) -> bool {
        use std::time::Duration;

        let client = self.client.clone();
        let bucket = format!("projects/_/buckets/{}", self.default_bucket);
        self.runtime.block_on(async {
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                client.read_object(&bucket, "__truss_health_probe__").send(),
            )
            .await;

            match result {
                Ok(Ok(_)) => true,
                Ok(Err(err)) => {
                    // A 404 for the bucket itself means the bucket name is
                    // wrong — treat as unreachable, matching S3's NoSuchBucket
                    // behavior.
                    if err.http_status_code() == Some(404) {
                        // Check if this is a bucket-level 404 (not an object-level 404).
                        // A 404 mentioning "bucket" indicates the bucket does not exist.
                        // We intentionally avoid broader checks like "not found" because
                        // object-level 404s (the expected case for a healthy bucket) would
                        // also match and cause a false negative.
                        let msg = err.to_string().to_ascii_lowercase();
                        if msg.contains("bucket") {
                            return false;
                        }
                    }
                    // Other service errors (NoSuchKey, AccessDenied, etc.)
                    // prove that GCS accepted the request, so the bucket is
                    // reachable.
                    true
                }
                // Timeout or transport error — not reachable.
                Err(_) => false,
            }
        })
    }
}

#[cfg(test)]
impl GcsContext {
    pub(crate) fn for_test(default_bucket: &str, endpoint_url: Option<&str>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = runtime.block_on(async {
            let mut builder = google_cloud_storage::client::Storage::builder();
            if let Some(endpoint) = endpoint_url {
                builder = builder.with_endpoint(endpoint);
            }
            builder.build().await.unwrap()
        });
        GcsContext {
            client,
            default_bucket: default_bucket.to_string(),
            endpoint_url: endpoint_url.map(|s| s.to_string()),
            runtime,
        }
    }
}

/// Builds the GCS client from the environment.
///
/// Authentication follows the standard Google Cloud SDK conventions:
/// - `GOOGLE_APPLICATION_CREDENTIALS` (path to service account JSON)
/// - `GOOGLE_APPLICATION_CREDENTIALS_JSON` (inline JSON)
/// - GCE metadata server (when running on Google Cloud)
///
/// When `TRUSS_GCS_ENDPOINT` is set, the client uses that URL instead of
/// the default GCS endpoint. This is required for emulators like
/// `fake-gcs-server`.
pub fn build_gcs_context(default_bucket: String) -> Result<GcsContext, std::io::Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;

    let endpoint_url = std::env::var("TRUSS_GCS_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty());

    if let Some(ref url) = endpoint_url {
        validate_endpoint_url(url)?;
    }

    let client = runtime
        .block_on(async {
            let mut builder = google_cloud_storage::client::Storage::builder();
            if let Some(ref endpoint) = endpoint_url {
                builder = builder.with_endpoint(endpoint);
            }
            builder.build().await
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    Ok(GcsContext {
        client,
        default_bucket,
        endpoint_url,
        runtime,
    })
}

/// Fetches an object from GCS and returns its body bytes.
///
/// Uses the shared multi-threaded Tokio runtime stored in [`GcsContext`] so
/// that multiple worker threads can issue concurrent GCS requests without
/// creating a runtime per call.
pub(super) fn read_gcs_source_bytes(
    bucket: &str,
    key: &str,
    gcs: &GcsContext,
) -> Result<Vec<u8>, HttpResponse> {
    validate_gcs_key(key)?;

    let gcs_bucket = format!("projects/_/buckets/{bucket}");
    gcs.runtime.block_on(async {
        let mut resp = gcs
            .client
            .read_object(&gcs_bucket, key)
            .send()
            .await
            .map_err(map_gcs_error)?;

        let object = resp.object();
        if object.size > 0 && object.size as u64 > MAX_SOURCE_BYTES {
            return Err(payload_too_large_response(
                "GCS object exceeds the source size limit",
            ));
        }

        let capacity = if object.size > 0 {
            (object.size as usize).min(MAX_SOURCE_BYTES as usize + 1)
        } else {
            0
        };
        let mut buf = Vec::with_capacity(capacity);
        while let Some(chunk) = resp.next().await {
            let chunk = chunk.map_err(|e| {
                bad_gateway_response(&format!("failed to read GCS object body: {e}"))
            })?;
            buf.extend_from_slice(&chunk);
            if buf.len() as u64 > MAX_SOURCE_BYTES {
                return Err(payload_too_large_response(
                    "GCS object exceeds the source size limit",
                ));
            }
        }
        Ok(buf)
    })
}

/// Validates the custom endpoint URL to prevent SSRF against cloud metadata
/// services and other internal endpoints.
fn validate_endpoint_url(url: &str) -> Result<(), std::io::Error> {
    let parsed: url::Url = url.parse().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("TRUSS_GCS_ENDPOINT is not a valid URL: {e}"),
        )
    })?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("TRUSS_GCS_ENDPOINT must use http or https scheme, got `{other}`"),
            ));
        }
    }

    if let Some(host) = parsed.host_str() {
        // Block the cloud metadata endpoint (169.254.169.254) used by AWS/GCP/Azure.
        if host == "169.254.169.254" || host == "metadata.google.internal" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "TRUSS_GCS_ENDPOINT must not point to a cloud metadata service",
            ));
        }
    }

    Ok(())
}

/// Validates that a GCS object name does not contain dangerous characters.
fn validate_gcs_key(key: &str) -> Result<(), HttpResponse> {
    if key.is_empty() {
        return Err(bad_request_response("GCS object name must not be empty"));
    }
    if key.contains('\0') || key.contains('\n') || key.contains('\r') {
        return Err(bad_request_response(
            "GCS object name contains invalid characters (null, newline, or carriage return)",
        ));
    }
    if key.len() > 1024 {
        return Err(bad_request_response(
            "GCS object name exceeds the maximum allowed length of 1024 bytes",
        ));
    }
    Ok(())
}

fn map_gcs_error(err: google_cloud_storage::Error) -> HttpResponse {
    if let Some(status) = err.http_status_code() {
        if status == 404 {
            return not_found_response("source image was not found in object storage");
        }
        if status == 403 {
            return super::response::forbidden_response(
                "access denied by object storage — check IAM permissions",
            );
        }
    }
    bad_gateway_response("object storage returned an error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_gcs_key_valid() {
        assert!(validate_gcs_key("images/photo.jpg").is_ok());
        assert!(validate_gcs_key("a").is_ok());
        assert!(validate_gcs_key("path/to/deep/object.png").is_ok());
    }

    #[test]
    fn test_validate_gcs_key_rejects_empty() {
        assert!(validate_gcs_key("").is_err());
    }

    #[test]
    fn test_validate_gcs_key_rejects_null() {
        assert!(validate_gcs_key("foo\0bar").is_err());
    }

    #[test]
    fn test_validate_gcs_key_rejects_newline() {
        assert!(validate_gcs_key("foo\nbar").is_err());
        assert!(validate_gcs_key("foo\rbar").is_err());
    }

    #[test]
    fn test_validate_gcs_key_rejects_too_long() {
        let long_key = "a".repeat(1025);
        assert!(validate_gcs_key(&long_key).is_err());

        let max_key = "a".repeat(1024);
        assert!(validate_gcs_key(&max_key).is_ok());
    }

    #[test]
    fn test_validate_gcs_key_allows_dot_segments() {
        assert!(validate_gcs_key("../etc/passwd").is_ok());
        assert!(validate_gcs_key("images/../secret").is_ok());
        assert!(validate_gcs_key("..").is_ok());
        assert!(validate_gcs_key("a..b/file.jpg").is_ok());
        assert!(validate_gcs_key(".hidden/file.jpg").is_ok());
    }

    #[test]
    fn test_validate_endpoint_url_accepts_http() {
        assert!(validate_endpoint_url("http://localhost:4443").is_ok());
    }

    #[test]
    fn test_validate_endpoint_url_accepts_https() {
        assert!(validate_endpoint_url("https://storage.googleapis.com").is_ok());
    }

    #[test]
    fn test_validate_endpoint_url_rejects_non_http_scheme() {
        assert!(validate_endpoint_url("ftp://example.com").is_err());
        assert!(validate_endpoint_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn test_validate_endpoint_url_rejects_metadata_service() {
        assert!(validate_endpoint_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(validate_endpoint_url("http://metadata.google.internal/computeMetadata").is_err());
    }

    #[test]
    fn test_validate_endpoint_url_rejects_invalid_url() {
        assert!(validate_endpoint_url("not a url").is_err());
    }
}
