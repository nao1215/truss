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
    pub default_container: String,
    runtime: tokio::runtime::Runtime,
}

impl std::fmt::Debug for AzureContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AzureContext")
            .field("default_container", &self.default_container)
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
        let container = self.default_container.clone();
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
    pub(crate) fn for_test(default_container: &str, endpoint_url: &str) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        AzureContext {
            endpoint_url: endpoint_url.to_string(),
            default_container: default_container.to_string(),
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
    default_container: String,
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
            if !(3..=24).contains(&account_name.len())
                || !account_name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "AZURE_STORAGE_ACCOUNT_NAME must be 3-24 characters, lowercase letters and digits only",
                ));
            }
            let url = format!("https://{account_name}.blob.core.windows.net");
            super::remote::validate_backend_endpoint_url(
                &url,
                "AZURE_STORAGE_ACCOUNT_NAME (derived endpoint)",
                allow_insecure,
            )?;
            url
        }
    };

    Ok(AzureContext {
        endpoint_url,
        default_container,
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
    timeout_secs: u64,
) -> Result<Vec<u8>, HttpResponse> {
    validate_azure_key(key)?;

    ctx.runtime.block_on(async {
        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let client =
                azure_storage_blob::BlobClient::new(&ctx.endpoint_url, container, key, None, None)
                    .map_err(|e| {
                        eprintln!("azure error: failed to create blob client: {e}");
                        bad_gateway_response("failed to create Azure blob client")
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
                    eprintln!("azure error: failed to read blob body: {e}");
                    bad_gateway_response("failed to read Azure blob body")
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
        .await;
        match result {
            Ok(inner) => inner,
            Err(_) => {
                eprintln!("azure error: download timed out after {timeout_secs}s");
                Err(bad_gateway_response("object storage download timed out"))
            }
        }
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
        if status == azure_core::http::StatusCode::Unauthorized {
            return super::response::forbidden_response(
                "object storage authentication failed — check credentials",
            );
        }
    }
    eprintln!("azure error: {err}");
    bad_gateway_response("object storage returned an error")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

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

    #[test]
    fn test_map_azure_error_401_returns_forbidden() {
        let resp = map_azure_error(http_error(azure_core::http::StatusCode::Unauthorized));
        assert_eq!(resp.status, "403 Forbidden");
    }

    // L-3: Unicode / special character key tests
    #[test]
    fn test_validate_azure_key_allows_unicode() {
        assert!(validate_azure_key("images/\u{5199}\u{771f}.jpg").is_ok());
        assert!(validate_azure_key("données/fichier.png").is_ok());
    }

    #[test]
    fn test_validate_azure_key_allows_special_chars() {
        assert!(validate_azure_key("path/to/file name.jpg").is_ok());
        assert!(validate_azure_key("a+b=c.jpg").is_ok());
        assert!(validate_azure_key("foo\tbar").is_ok());
    }

    // L-2: build_azure_context negative tests
    #[test]
    #[serial]
    fn test_build_azure_context_missing_env() {
        unsafe {
            std::env::remove_var("TRUSS_AZURE_ENDPOINT");
            std::env::remove_var("AZURE_STORAGE_ACCOUNT_NAME");
        }
        let result = build_azure_context("test-container".to_string(), true);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("AZURE_STORAGE_ACCOUNT_NAME"),
            "error should mention AZURE_STORAGE_ACCOUNT_NAME: {msg}"
        );
    }

    #[test]
    #[serial]
    fn test_build_azure_context_invalid_account_name_chars() {
        unsafe {
            std::env::remove_var("TRUSS_AZURE_ENDPOINT");
            std::env::set_var("AZURE_STORAGE_ACCOUNT_NAME", "My-Account!");
        }
        let result = build_azure_context("test-container".to_string(), true);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("lowercase letters and digits only"),
            "error should mention format constraint: {msg}"
        );
        unsafe { std::env::remove_var("AZURE_STORAGE_ACCOUNT_NAME") };
    }

    #[test]
    #[serial]
    fn test_build_azure_context_account_name_too_short() {
        unsafe {
            std::env::remove_var("TRUSS_AZURE_ENDPOINT");
            std::env::set_var("AZURE_STORAGE_ACCOUNT_NAME", "ab");
        }
        let result = build_azure_context("test-container".to_string(), true);
        assert!(result.is_err());
        unsafe { std::env::remove_var("AZURE_STORAGE_ACCOUNT_NAME") };
    }

    #[test]
    #[serial]
    fn test_build_azure_context_account_name_too_long() {
        unsafe {
            std::env::remove_var("TRUSS_AZURE_ENDPOINT");
            std::env::set_var("AZURE_STORAGE_ACCOUNT_NAME", "a".repeat(25));
        }
        let result = build_azure_context("test-container".to_string(), true);
        assert!(result.is_err());
        unsafe { std::env::remove_var("AZURE_STORAGE_ACCOUNT_NAME") };
    }
}
