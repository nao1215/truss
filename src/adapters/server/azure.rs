use super::remote::MAX_SOURCE_BYTES;
use super::response::{
    HttpResponse, bad_gateway_response, bad_request_response, not_found_response,
    payload_too_large_response,
};

/// Shared Azure Blob Storage client state constructed once at startup and
/// threaded through [`super::ServerConfig`].
///
/// A multi-threaded Tokio runtime is stored alongside the client so that
/// worker threads can call `runtime.block_on(...)` concurrently without
/// creating a new runtime per request.
pub struct AzureContext {
    pub endpoint_url: String,
    pub default_bucket: String,
    runtime: tokio::runtime::Runtime,
}

impl std::fmt::Debug for AzureContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureContext")
            .field("default_bucket", &self.default_bucket)
            .field("endpoint_url", &self.endpoint_url)
            .finish()
    }
}

impl AzureContext {
    /// Returns `true` if the configured container is reachable.
    ///
    /// Issues a GetProperties for a blob that is extremely unlikely to exist.
    /// Most service-level responses (BlobNotFound, AuthorizationFailure) prove
    /// that Azure accepted the request and the container exists, so they count
    /// as "reachable".  `ContainerNotFound` is explicitly treated as
    /// *unreachable* because it indicates a misconfigured container name.
    pub fn check_reachable(&self) -> bool {
        use azure_storage_blob::models::StorageErrorCode;
        use std::time::Duration;

        let endpoint = self.endpoint_url.clone();
        let container = self.default_bucket.clone();
        self.runtime.block_on(async {
            let client = match azure_storage_blob::BlobClient::new(
                &endpoint,
                &container,
                "__truss_health_probe__",
                None,
                None,
            ) {
                Ok(c) => c,
                Err(_) => return false,
            };

            let result =
                tokio::time::timeout(Duration::from_secs(2), client.get_properties(None)).await;

            match result {
                Ok(Ok(_)) => true,
                Ok(Err(err)) => {
                    if let azure_core::error::ErrorKind::HttpResponse {
                        error_code: Some(code),
                        ..
                    } = err.kind()
                        && code == StorageErrorCode::ContainerNotFound.as_ref()
                    {
                        return false;
                    }
                    // Other errors (BlobNotFound, AuthorizationFailure, etc.)
                    // prove the container is reachable.
                    true
                }
                // Timeout or transport error — not reachable.
                Err(_) => false,
            }
        })
    }
}

#[cfg(test)]
impl AzureContext {
    pub(crate) fn for_test(default_bucket: &str, endpoint_url: &str) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        AzureContext {
            endpoint_url: endpoint_url.to_string(),
            default_bucket: default_bucket.to_string(),
            runtime,
        }
    }
}

/// Builds the Azure Blob Storage context from the environment.
///
/// Authentication uses anonymous access (credential=None) when
/// `TRUSS_AZURE_ENDPOINT` is set (typically for Azurite local development).
/// When the endpoint is not set, the default Azure public endpoint is derived
/// from `AZURE_STORAGE_ACCOUNT_NAME`.
///
/// Environment variables:
/// - `TRUSS_AZURE_ENDPOINT`: custom endpoint URL (for Azurite, etc.)
/// - `AZURE_STORAGE_ACCOUNT_NAME`: storage account name (used to derive
///   the default endpoint when `TRUSS_AZURE_ENDPOINT` is not set)
pub fn build_azure_context(
    default_bucket: String,
    allow_insecure: bool,
) -> Result<AzureContext, std::io::Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;

    let endpoint_url = match std::env::var("TRUSS_AZURE_ENDPOINT")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(url) => {
            super::remote::validate_backend_endpoint_url(
                &url,
                "TRUSS_AZURE_ENDPOINT",
                allow_insecure,
            )?;
            url
        }
        None => {
            let account_name = std::env::var("AZURE_STORAGE_ACCOUNT_NAME")
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "AZURE_STORAGE_ACCOUNT_NAME is required when TRUSS_AZURE_ENDPOINT is not set",
                    )
                })?;
            format!("https://{account_name}.blob.core.windows.net")
        }
    };

    Ok(AzureContext {
        endpoint_url,
        default_bucket,
        runtime,
    })
}

/// Fetches a blob from Azure Blob Storage and returns its body bytes.
///
/// Uses the shared multi-threaded Tokio runtime stored in [`AzureContext`] so
/// that multiple worker threads can issue concurrent Azure requests without
/// creating a runtime per call.
pub(super) fn read_azure_source_bytes(
    container: &str,
    key: &str,
    ctx: &AzureContext,
) -> Result<Vec<u8>, HttpResponse> {
    validate_azure_key(key)?;

    ctx.runtime.block_on(async {
        let client =
            azure_storage_blob::BlobClient::new(&ctx.endpoint_url, container, key, None, None)
                .map_err(|e| {
                    bad_gateway_response(&format!("failed to create Azure blob client: {e}"))
                })?;

        let resp = client.download(None).await.map_err(map_azure_error)?;

        use azure_storage_blob::models::BlobClientDownloadResultHeaders;
        let content_length = resp.content_length().ok().flatten();
        if let Some(len) = content_length
            && len > MAX_SOURCE_BYTES
        {
            return Err(payload_too_large_response(
                "Azure blob exceeds the source size limit",
            ));
        }

        let (_, _, body) = resp.deconstruct();

        use futures::StreamExt;
        let capacity = if let Some(len) = content_length {
            (len as usize).min(MAX_SOURCE_BYTES as usize + 1)
        } else {
            0
        };
        let mut buf = Vec::with_capacity(capacity);
        futures::pin_mut!(body);
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| {
                bad_gateway_response(&format!("failed to read Azure blob body: {e}"))
            })?;
            buf.extend_from_slice(&chunk);
            if buf.len() as u64 > MAX_SOURCE_BYTES {
                return Err(payload_too_large_response(
                    "Azure blob exceeds the source size limit",
                ));
            }
        }
        Ok(buf)
    })
}

/// Validates that an Azure blob name does not contain dangerous characters.
fn validate_azure_key(key: &str) -> Result<(), HttpResponse> {
    if key.is_empty() {
        return Err(bad_request_response("Azure blob name must not be empty"));
    }
    if key.contains('\0') || key.contains('\n') || key.contains('\r') {
        return Err(bad_request_response(
            "Azure blob name contains invalid characters (null, newline, or carriage return)",
        ));
    }
    if key.len() > 1024 {
        return Err(bad_request_response(
            "Azure blob name exceeds the maximum allowed length of 1024 bytes",
        ));
    }
    Ok(())
}

/// Maps an Azure SDK error to an appropriate HTTP response.
///
/// - **404**: The blob was not found in the container.
/// - **403**: Access was denied.  Azure returns 403 when the storage account
///   key is incorrect or the SAS token lacks the required permissions.
/// - **Other**: Treated as a backend failure and mapped to 502 Bad Gateway.
fn map_azure_error(err: azure_core::Error) -> HttpResponse {
    if let Some(status) = err.http_status() {
        if status == azure_core::http::StatusCode::NotFound {
            return not_found_response("source image was not found in object storage");
        }
        if status == azure_core::http::StatusCode::Forbidden {
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
    fn test_validate_azure_key_valid() {
        assert!(validate_azure_key("images/photo.jpg").is_ok());
        assert!(validate_azure_key("a").is_ok());
        assert!(validate_azure_key("path/to/deep/object.png").is_ok());
    }

    #[test]
    fn test_validate_azure_key_rejects_empty() {
        assert!(validate_azure_key("").is_err());
    }

    #[test]
    fn test_validate_azure_key_rejects_null() {
        assert!(validate_azure_key("foo\0bar").is_err());
    }

    #[test]
    fn test_validate_azure_key_rejects_newline() {
        assert!(validate_azure_key("foo\nbar").is_err());
        assert!(validate_azure_key("foo\rbar").is_err());
    }

    #[test]
    fn test_validate_azure_key_rejects_too_long() {
        let long_key = "a".repeat(1025);
        assert!(validate_azure_key(&long_key).is_err());

        let max_key = "a".repeat(1024);
        assert!(validate_azure_key(&max_key).is_ok());
    }

    #[test]
    fn test_validate_azure_key_allows_dot_segments() {
        assert!(validate_azure_key("../etc/passwd").is_ok());
        assert!(validate_azure_key("images/../secret").is_ok());
        assert!(validate_azure_key("..").is_ok());
        assert!(validate_azure_key("a..b/file.jpg").is_ok());
        assert!(validate_azure_key(".hidden/file.jpg").is_ok());
    }

    /// Helper: build an `azure_core::Error` with the given HTTP status code.
    fn http_error(status: azure_core::http::StatusCode) -> azure_core::Error {
        azure_core::error::ErrorKind::HttpResponse {
            status,
            error_code: None,
            raw_response: None,
        }
        .into_error()
    }

    #[test]
    fn test_map_azure_error_404_returns_not_found() {
        let resp = map_azure_error(http_error(azure_core::http::StatusCode::NotFound));
        assert_eq!(resp.status, "404 Not Found");
    }

    #[test]
    fn test_map_azure_error_403_returns_forbidden() {
        let resp = map_azure_error(http_error(azure_core::http::StatusCode::Forbidden));
        assert_eq!(resp.status, "403 Forbidden");
    }

    #[test]
    fn test_map_azure_error_500_returns_bad_gateway() {
        let resp = map_azure_error(http_error(
            azure_core::http::StatusCode::InternalServerError,
        ));
        assert_eq!(resp.status, "502 Bad Gateway");
    }

    #[test]
    fn test_map_azure_error_non_http_returns_bad_gateway() {
        let err = azure_core::error::ErrorKind::Other.into_error();
        let resp = map_azure_error(err);
        assert_eq!(resp.status, "502 Bad Gateway");
    }
}
