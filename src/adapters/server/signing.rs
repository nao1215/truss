/// Signed URL generation and bind address resolution.
use hmac::{Hmac, Mac};
use sha2::Sha256;
use url::Url;

use super::auth::{
    canonical_query_without_signature, extend_transform_query, signed_source_query, url_authority,
};
use super::config::DEFAULT_BIND_ADDR;
use crate::TransformOptions;

pub(super) type HmacSha256 = Hmac<Sha256>;

/// Source selector used when generating a signed public transform URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignedUrlSource {
    /// Generates a signed `GET /images/by-path` URL.
    Path {
        /// The storage-relative source path.
        path: String,
        /// An optional source version token.
        version: Option<String>,
    },
    /// Generates a signed `GET /images/by-url` URL.
    Url {
        /// The remote source URL.
        url: String,
        /// An optional source version token.
        version: Option<String>,
    },
}

/// Builds a signed public transform URL for the server adapter.
///
/// The resulting URL targets either `GET /images/by-path` or `GET /images/by-url` depending on
/// `source`. `base_url` must be an absolute `http` or `https` URL that points at the externally
/// visible server origin. The helper applies the same canonical query and HMAC-SHA256 signature
/// scheme that the server adapter verifies at request time.
///
/// The helper serializes only explicitly requested transform options and omits fields that would
/// resolve to the documented defaults on the server side.
///
/// # Errors
///
/// Returns an error string when `base_url` is not an absolute `http` or `https` URL, when the
/// visible authority cannot be determined, or when the HMAC state cannot be initialized.
///
/// # Examples
///
/// ```
/// use truss::adapters::server::{sign_public_url, SignedUrlSource};
/// use truss::{MediaType, TransformOptions};
///
/// let url = sign_public_url(
///     "https://cdn.example.com",
///     SignedUrlSource::Path {
///         path: "/image.png".to_string(),
///         version: None,
///     },
///     &TransformOptions {
///         format: Some(MediaType::Jpeg),
///         ..TransformOptions::default()
///     },
///     "public-dev",
///     "secret-value",
///     4_102_444_800,
///     None,
///     None,
/// )
/// .unwrap();
///
/// assert!(url.starts_with("https://cdn.example.com/images/by-path?"));
/// assert!(url.contains("keyId=public-dev"));
/// assert!(url.contains("signature="));
/// ```
/// Optional watermark parameters for signed URL generation.
#[derive(Debug, Default)]
pub struct SignedWatermarkParams {
    pub url: String,
    pub position: Option<String>,
    pub opacity: Option<u8>,
    pub margin: Option<u32>,
}

#[allow(clippy::too_many_arguments)]
pub fn sign_public_url(
    base_url: &str,
    source: SignedUrlSource,
    options: &TransformOptions,
    key_id: &str,
    secret: &str,
    expires: u64,
    watermark: Option<&SignedWatermarkParams>,
    preset: Option<&str>,
) -> Result<String, String> {
    let base_url = Url::parse(base_url).map_err(|error| format!("base URL is invalid: {error}"))?;
    match base_url.scheme() {
        "http" | "https" => {}
        _ => return Err("base URL must use the http or https scheme".to_string()),
    }

    let route_path = match source {
        SignedUrlSource::Path { .. } => "/images/by-path",
        SignedUrlSource::Url { .. } => "/images/by-url",
    };
    let mut endpoint = base_url
        .join(route_path)
        .map_err(|error| format!("failed to resolve the public endpoint URL: {error}"))?;
    let authority = url_authority(&endpoint)?;
    let mut query = signed_source_query(source);
    if let Some(name) = preset {
        query.insert("preset".to_string(), name.to_string());
    }
    extend_transform_query(&mut query, options);
    if let Some(wm) = watermark {
        query.insert("watermarkUrl".to_string(), wm.url.clone());
        if let Some(ref pos) = wm.position {
            query.insert("watermarkPosition".to_string(), pos.clone());
        }
        if let Some(opacity) = wm.opacity {
            query.insert("watermarkOpacity".to_string(), opacity.to_string());
        }
        if let Some(margin) = wm.margin {
            query.insert("watermarkMargin".to_string(), margin.to_string());
        }
    }
    query.insert("keyId".to_string(), key_id.to_string());
    query.insert("expires".to_string(), expires.to_string());

    let canonical = format!(
        "GET\n{}\n{}\n{}",
        authority,
        endpoint.path(),
        canonical_query_without_signature(&query)
    );
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| format!("failed to initialize signed URL HMAC: {error}"))?;
    mac.update(canonical.as_bytes());
    query.insert(
        "signature".to_string(),
        hex::encode(mac.finalize().into_bytes()),
    );

    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in query {
        serializer.append_pair(&name, &value);
    }
    endpoint.set_query(Some(&serializer.finish()));
    Ok(endpoint.into())
}

/// Returns the bind address for the HTTP server adapter.
///
/// The adapter reads `TRUSS_BIND_ADDR` when it is present. Otherwise it falls back to
/// [`DEFAULT_BIND_ADDR`].
pub fn bind_addr() -> String {
    std::env::var("TRUSS_BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OptimizeMode, TargetQuality, TransformOptions};

    #[test]
    fn sign_public_url_rejects_invalid_base_url() {
        let result = sign_public_url(
            "not-a-url",
            SignedUrlSource::Path {
                path: "/img.png".to_string(),
                version: None,
            },
            &TransformOptions::default(),
            "key",
            "secret",
            0,
            None,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("base URL is invalid"));
    }

    #[test]
    fn sign_public_url_rejects_non_http_scheme() {
        let result = sign_public_url(
            "ftp://example.com",
            SignedUrlSource::Path {
                path: "/img.png".to_string(),
                version: None,
            },
            &TransformOptions::default(),
            "key",
            "secret",
            0,
            None,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("http or https"));
    }

    #[test]
    fn sign_public_url_path_source_generates_by_path_url() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/photo.jpg".to_string(),
                version: None,
            },
            &TransformOptions::default(),
            "mykey",
            "mysecret",
            9999,
            None,
            None,
        )
        .unwrap();
        assert!(url.starts_with("https://cdn.example.com/images/by-path?"));
        assert!(url.contains("keyId=mykey"));
        assert!(url.contains("signature="));
        assert!(url.contains("expires=9999"));
    }

    #[test]
    fn sign_public_url_url_source_generates_by_url() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Url {
                url: "https://remote.example.com/img.png".to_string(),
                version: None,
            },
            &TransformOptions::default(),
            "key",
            "secret",
            0,
            None,
            None,
        )
        .unwrap();
        assert!(url.starts_with("https://cdn.example.com/images/by-url?"));
    }

    #[test]
    fn sign_public_url_includes_preset() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/img.png".to_string(),
                version: None,
            },
            &TransformOptions::default(),
            "key",
            "secret",
            0,
            None,
            Some("thumbnail"),
        )
        .unwrap();
        assert!(url.contains("preset=thumbnail"));
    }

    #[test]
    fn sign_public_url_includes_watermark_params() {
        let wm = SignedWatermarkParams {
            url: "https://example.com/logo.png".to_string(),
            position: Some("southeast".to_string()),
            opacity: Some(80),
            margin: Some(10),
        };
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/img.png".to_string(),
                version: None,
            },
            &TransformOptions::default(),
            "key",
            "secret",
            0,
            Some(&wm),
            None,
        )
        .unwrap();
        assert!(url.contains("watermarkUrl="));
        assert!(url.contains("watermarkPosition=southeast"));
        assert!(url.contains("watermarkOpacity=80"));
        assert!(url.contains("watermarkMargin=10"));
    }

    #[test]
    fn sign_public_url_includes_optimize_params() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/img.png".to_string(),
                version: None,
            },
            &TransformOptions {
                format: Some(crate::MediaType::Jpeg),
                optimize: OptimizeMode::Lossy,
                target_quality: Some("ssim:0.98".parse::<TargetQuality>().unwrap()),
                ..TransformOptions::default()
            },
            "key",
            "secret",
            0,
            None,
            None,
        )
        .unwrap();

        assert!(url.contains("optimize=lossy"));
        assert!(url.contains("targetQuality=ssim%3A0.98"));
    }
}
