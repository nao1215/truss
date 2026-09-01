/// Signed URL generation and bind address resolution.
use hmac::{Hmac, KeyInit, Mac};
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
    sign_public_url_with_method(
        "GET", base_url, source, options, key_id, secret, expires, watermark, preset,
    )
}

/// Names the reason a set of signing inputs can never produce a URL the server accepts.
///
/// A key id and a secret are refused by `ServerConfig::from_env`, which will not start a
/// server whose `TRUSS_SIGNING_KEYS` holds an empty one, and an empty source is refused by
/// the route that reads it, so a URL carrying any of them is answered 400 or 401 for as
/// long as it exists. A signed URL is usually written somewhere other than where it is
/// fetched, so the signer refuses them rather than the request.
///
/// The signer and `truss sign` both read this, the way both read
/// [`TransformOptions::validate_without_input`] for the rules about the transform.
pub(crate) fn signing_input_error(
    key_id: &str,
    secret: &str,
    source: &SignedUrlSource,
) -> Option<&'static str> {
    if key_id.is_empty() {
        return Some("key id must not be empty");
    }
    if secret.is_empty() {
        return Some("secret must not be empty");
    }
    match source {
        SignedUrlSource::Path { path, .. } if path.is_empty() => Some("path must not be empty"),
        SignedUrlSource::Url { url, .. } if url.is_empty() => Some("url must not be empty"),
        _ => None,
    }
}

/// Like [`sign_public_url`] but allows the caller to specify the HTTP method
/// included in the canonical string (e.g. `"GET"` or `"HEAD"`).
#[allow(clippy::too_many_arguments)]
pub fn sign_public_url_with_method(
    method: &str,
    base_url: &str,
    source: SignedUrlSource,
    options: &TransformOptions,
    key_id: &str,
    secret: &str,
    expires: u64,
    watermark: Option<&SignedWatermarkParams>,
    preset: Option<&str>,
) -> Result<String, String> {
    let mut base_url =
        Url::parse(base_url).map_err(|error| format!("base URL is invalid: {error}"))?;
    match base_url.scheme() {
        "http" | "https" => {}
        _ => return Err("base URL must use the http or https scheme".to_string()),
    }
    if let Some(reason) = signing_input_error(key_id, secret, &source) {
        return Err(reason.to_string());
    }

    let route_path = match source {
        SignedUrlSource::Path { .. } => "/images/by-path",
        SignedUrlSource::Url { .. } => "/images/by-url",
    };
    // The base URL may carry a path, which is a deployment served under a prefix by a
    // proxy that strips it before truss sees the request. Resolving an absolute route path
    // against it would drop the prefix, so the base path is given a trailing slash and the
    // route is joined onto it as a relative reference.
    if !base_url.path().ends_with('/') {
        let with_slash = format!("{}/", base_url.path());
        base_url.set_path(&with_slash);
    }
    let mut endpoint = base_url
        .join(route_path.trim_start_matches('/'))
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

    // REQUEST_PATH in `docs/signed-url-spec.md` is the literal endpoint path, which is what
    // truss receives after a proxy has stripped whatever prefix the base URL carried. It is
    // therefore the route rather than the path of the URL being emitted.
    let canonical = format!(
        "{}\n{}\n{}\n{}",
        method.to_ascii_uppercase(),
        authority,
        route_path,
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

    /// The three inputs the server can never accept, whatever the request carries.
    ///
    /// An empty key id and an empty secret are refused by the configuration parser before
    /// a server binds a port, and an empty path is refused by the by-path route, so a URL
    /// carrying one of them is a URL that will be answered 400 or 401 for as long as it
    /// exists.
    #[test]
    fn sign_public_url_refuses_inputs_no_server_can_accept() {
        let sign = |key_id: &str, secret: &str, source: SignedUrlSource| {
            sign_public_url(
                "https://images.example.com",
                source,
                &TransformOptions::default(),
                key_id,
                secret,
                1_900_000_000,
                None,
                None,
            )
        };
        let path = |path: &str| SignedUrlSource::Path {
            path: path.to_string(),
            version: None,
        };

        assert_eq!(
            sign("", "secret-value", path("/image.png")),
            Err("key id must not be empty".to_string())
        );
        assert_eq!(
            sign("public-demo", "", path("/image.png")),
            Err("secret must not be empty".to_string())
        );
        assert_eq!(
            sign("public-demo", "secret-value", path("")),
            Err("path must not be empty".to_string())
        );
        assert_eq!(
            sign(
                "public-demo",
                "secret-value",
                SignedUrlSource::Url {
                    url: String::new(),
                    version: None,
                },
            ),
            Err("url must not be empty".to_string())
        );
        assert!(sign("public-demo", "secret-value", path("/image.png")).is_ok());
    }

    /// A base URL with a path prefix points at a deployment behind a proxy that serves
    /// truss under it, and the prefix has to survive into the emitted URL.
    ///
    /// The signature must not move: the canonical string carries the literal endpoint path
    /// the server sees after the proxy has stripped the prefix, which is what
    /// `docs/signed-url-spec.md` calls REQUEST_PATH.
    #[test]
    fn sign_public_url_keeps_a_path_in_the_base_url() {
        let sign = |base_url: &str| {
            sign_public_url(
                base_url,
                SignedUrlSource::Path {
                    path: "image.png".to_string(),
                    version: None,
                },
                &TransformOptions::default(),
                "public-demo",
                "secret-value",
                1_900_000_000,
                None,
                None,
            )
            .expect("sign")
        };

        let plain = sign("https://images.example.com");
        let signature = |url: &str| {
            url.split("signature=")
                .nth(1)
                .expect("a signature")
                .split('&')
                .next()
                .expect("the signature value")
                .to_string()
        };

        for base in [
            "https://images.example.com/img",
            "https://images.example.com/img/",
        ] {
            let prefixed = sign(base);
            assert!(
                prefixed.starts_with("https://images.example.com/img/images/by-path?"),
                "the prefix has to reach the emitted URL, got: {prefixed}"
            );
            assert_eq!(
                signature(&prefixed),
                signature(&plain),
                "the canonical string carries the endpoint path, not the base URL's"
            );
        }

        assert!(
            sign("https://images.example.com/")
                .starts_with("https://images.example.com/images/by-path?")
        );
    }

    #[test]
    fn sign_public_url_matches_fixed_compatibility_vector() {
        let url = sign_public_url(
            "https://images.example.com",
            SignedUrlSource::Path {
                path: "image.png".to_string(),
                version: None,
            },
            &TransformOptions {
                width: Some(800),
                format: Some(crate::MediaType::Webp),
                ..TransformOptions::default()
            },
            "public-demo",
            "secret-value",
            1_900_000_000,
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            url,
            "https://images.example.com/images/by-path?expires=1900000000&format=webp&keyId=public-demo&path=image.png&signature=8c3234125e0e20efeaae1e2afaa88a81d387c82cef0080780fddd31c5689199e&width=800"
        );
    }
}
