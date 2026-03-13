use super::config::ServerConfig;
use super::http_parse::HttpRequest;
use super::negotiate::PublicSourceKind;
use super::response::{
    HttpResponse, auth_required_response, bad_request_response, internal_error_response,
    service_unavailable_response, signed_url_unauthorized_response,
};
use super::signing::{HmacSha256, SignedUrlSource};
use crate::{Rgba8, Rotation, TransformOptions};
use hmac::Mac;
use std::collections::BTreeMap;
use subtle::ConstantTimeEq;
use url::Url;

pub(super) fn authorize_request(
    request: &HttpRequest,
    config: &ServerConfig,
) -> Result<(), HttpResponse> {
    authorize_request_headers(&request.headers, config)
}

/// Authenticates a request using only the parsed header list. This is used by
/// `handle_stream` to reject unauthenticated requests *before* reading the
/// (potentially large) request body.
pub(super) fn authorize_request_headers(
    headers: &[(String, String)],
    config: &ServerConfig,
) -> Result<(), HttpResponse> {
    let expected = config.bearer_token.as_deref().ok_or_else(|| {
        service_unavailable_response("private API bearer token is not configured")
    })?;
    let provided = headers
        .iter()
        .find_map(|(name, value)| (name == "authorization").then_some(value.as_str()))
        .and_then(|value| {
            let (scheme, token) = value.split_once(|c: char| c.is_whitespace())?;
            scheme.eq_ignore_ascii_case("Bearer").then(|| token.trim())
        });

    match provided {
        Some(token) if token.as_bytes().ct_eq(expected.as_bytes()).into() => Ok(()),
        _ => Err(auth_required_response("authorization required")),
    }
}

pub(super) fn authorize_signed_request(
    request: &HttpRequest,
    query: &BTreeMap<String, String>,
    config: &ServerConfig,
) -> Result<(), HttpResponse> {
    if config.signing_keys.is_empty() {
        return Err(service_unavailable_response(
            "public signed URL keys are not configured",
        ));
    }
    let key_id = required_auth_query_param(query, "keyId")?;
    let expires = required_auth_query_param(query, "expires")?;
    let signature = required_auth_query_param(query, "signature")?;

    let secret = config
        .signing_keys
        .get(key_id)
        .ok_or_else(|| signed_url_unauthorized_response("signed URL is invalid or expired"))?;

    let expires = expires.parse::<u64>().map_err(|_| {
        bad_request_response("query parameter `expires` must be a positive integer")
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            internal_error_response(&format!("failed to read the current time: {error}"))
        })?
        .as_secs();
    if expires < now {
        return Err(signed_url_unauthorized_response(
            "signed URL is invalid or expired",
        ));
    }

    let authority = canonical_request_authority(request, config)?;
    let canonical_query = canonical_query_without_signature(query);
    let canonical = format!(
        "{}\n{}\n{}\n{}",
        request.method,
        authority,
        request.path(),
        canonical_query
    );

    let provided_signature = hex::decode(signature)
        .map_err(|_| signed_url_unauthorized_response("signed URL is invalid or expired"))?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|error| {
        internal_error_response(&format!(
            "failed to initialize signed URL verification: {error}"
        ))
    })?;
    mac.update(canonical.as_bytes());
    mac.verify_slice(&provided_signature)
        .map_err(|_| signed_url_unauthorized_response("signed URL is invalid or expired"))
}

pub(super) fn canonical_request_authority(
    request: &HttpRequest,
    config: &ServerConfig,
) -> Result<String, HttpResponse> {
    if let Some(public_base_url) = &config.public_base_url {
        let parsed = Url::parse(public_base_url).map_err(|error| {
            internal_error_response(&format!(
                "configured public base URL is invalid at runtime: {error}"
            ))
        })?;
        return url_authority(&parsed).map_err(|message| internal_error_response(&message));
    }

    request
        .header("host")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad_request_response("public GET requests require a Host header"))
}

pub(super) fn url_authority(url: &Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "configured public base URL must include a host".to_string())?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

pub(super) fn canonical_query_without_signature(query: &BTreeMap<String, String>) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in query {
        if name != "signature" {
            serializer.append_pair(name, value);
        }
    }
    serializer.finish()
}

pub(super) fn signed_source_query(source: SignedUrlSource) -> BTreeMap<String, String> {
    let mut query = BTreeMap::new();
    match source {
        SignedUrlSource::Path { path, version } => {
            query.insert("path".to_string(), path);
            if let Some(version) = version {
                query.insert("version".to_string(), version);
            }
        }
        SignedUrlSource::Url { url, version } => {
            query.insert("url".to_string(), url);
            if let Some(version) = version {
                query.insert("version".to_string(), version);
            }
        }
    }
    query
}

pub(super) fn extend_transform_query(
    query: &mut BTreeMap<String, String>,
    options: &TransformOptions,
) {
    if let Some(width) = options.width {
        query.insert("width".to_string(), width.to_string());
    }
    if let Some(height) = options.height {
        query.insert("height".to_string(), height.to_string());
    }
    if let Some(fit) = options.fit {
        query.insert("fit".to_string(), fit.as_name().to_string());
    }
    if let Some(position) = options.position {
        query.insert("position".to_string(), position.as_name().to_string());
    }
    if let Some(format) = options.format {
        query.insert("format".to_string(), format.as_name().to_string());
    }
    if let Some(quality) = options.quality {
        query.insert("quality".to_string(), quality.to_string());
    }
    if let Some(background) = options.background {
        query.insert("background".to_string(), encode_background(background));
    }
    if options.rotate != Rotation::Deg0 {
        query.insert(
            "rotate".to_string(),
            options.rotate.as_degrees().to_string(),
        );
    }
    if !options.auto_orient {
        query.insert("autoOrient".to_string(), "false".to_string());
    }
    if !options.strip_metadata {
        query.insert("stripMetadata".to_string(), "false".to_string());
    }
    if options.preserve_exif {
        query.insert("preserveExif".to_string(), "true".to_string());
    }
    if let Some(crop) = options.crop {
        query.insert("crop".to_string(), crop.to_string());
    }
    if let Some(blur) = options.blur {
        query.insert("blur".to_string(), format!("{blur}"));
    }
    if let Some(sharpen) = options.sharpen {
        query.insert("sharpen".to_string(), format!("{sharpen}"));
    }
}

pub(super) fn encode_background(color: Rgba8) -> String {
    if color.a == u8::MAX {
        format!("{:02X}{:02X}{:02X}", color.r, color.g, color.b)
    } else {
        format!(
            "{:02X}{:02X}{:02X}{:02X}",
            color.r, color.g, color.b, color.a
        )
    }
}

pub(super) fn validate_public_query_names(
    query: &BTreeMap<String, String>,
    source_kind: PublicSourceKind,
) -> Result<(), HttpResponse> {
    for name in query.keys() {
        let allowed = matches!(
            name.as_str(),
            "keyId"
                | "expires"
                | "signature"
                | "version"
                | "width"
                | "height"
                | "fit"
                | "position"
                | "format"
                | "quality"
                | "background"
                | "rotate"
                | "autoOrient"
                | "stripMetadata"
                | "preserveExif"
                | "crop"
                | "blur"
                | "sharpen"
                | "watermarkUrl"
                | "watermarkPosition"
                | "watermarkOpacity"
                | "watermarkMargin"
                | "preset"
        ) || matches!(
            (source_kind, name.as_str()),
            (PublicSourceKind::Path, "path") | (PublicSourceKind::Url, "url")
        );

        if !allowed {
            return Err(bad_request_response(&format!(
                "query parameter `{name}` is not supported for this endpoint"
            )));
        }
    }

    Ok(())
}

pub(super) fn parse_query_params(
    request: &HttpRequest,
) -> Result<BTreeMap<String, String>, HttpResponse> {
    let Some(query) = request.query() else {
        return Ok(BTreeMap::new());
    };

    let mut params = BTreeMap::new();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let name = name.into_owned();
        let value = value.into_owned();
        if params.contains_key(&name) {
            return Err(bad_request_response(&format!(
                "query parameter `{name}` must not be repeated"
            )));
        }
        params.insert(name, value);
    }

    Ok(params)
}

pub(super) fn required_query_param<'a>(
    query: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, HttpResponse> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_request_response(&format!("query parameter `{name}` is required")))
}

pub(super) fn required_auth_query_param<'a>(
    query: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, HttpResponse> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| signed_url_unauthorized_response("signed URL is invalid or expired"))
}

pub(super) fn parse_optional_integer_query(
    query: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<u32>, HttpResponse> {
    match query.get(name) {
        Some(value) => value.parse::<u32>().map(Some).map_err(|_| {
            bad_request_response(&format!("query parameter `{name}` must be an integer"))
        }),
        None => Ok(None),
    }
}

pub(super) fn parse_optional_u8_query(
    query: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<u8>, HttpResponse> {
    match query.get(name) {
        Some(value) => value.parse::<u8>().map(Some).map_err(|_| {
            bad_request_response(&format!("query parameter `{name}` must be an integer"))
        }),
        None => Ok(None),
    }
}

pub(super) fn parse_optional_float_query(
    query: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<f32>, HttpResponse> {
    match query.get(name) {
        Some(value) => value.parse::<f32>().map(Some).map_err(|_| {
            bad_request_response(&format!("query parameter `{name}` must be a number"))
        }),
        None => Ok(None),
    }
}

pub(super) fn parse_optional_bool_query(
    query: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<bool>, HttpResponse> {
    match query.get(name).map(String::as_str) {
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(bad_request_response(&format!(
            "query parameter `{name}` must be `true` or `false`"
        ))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_config(bearer_token: Option<&str>) -> ServerConfig {
        ServerConfig::new(PathBuf::from("/tmp"), bearer_token.map(str::to_string))
    }

    fn test_request(method: &str, target: &str, headers: Vec<(&str, &str)>) -> HttpRequest {
        HttpRequest {
            method: method.to_string(),
            target: target.to_string(),
            version: "HTTP/1.1".to_string(),
            headers: headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: Vec::new(),
        }
    }

    fn query(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ── authorize_request_headers ──

    #[test]
    fn test_authorize_valid_bearer_token() {
        let config = test_config(Some("my-secret"));
        let headers = vec![("authorization", "Bearer my-secret")];
        let result = authorize_request_headers(
            &headers
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>(),
            &config,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_authorize_bearer_case_insensitive_scheme() {
        let config = test_config(Some("tok"));
        let headers = vec![("authorization".to_string(), "bEaReR tok".to_string())];
        assert!(authorize_request_headers(&headers, &config).is_ok());
    }

    #[test]
    fn test_authorize_wrong_token_rejected() {
        let config = test_config(Some("correct"));
        let headers = vec![("authorization".to_string(), "Bearer wrong".to_string())];
        let err = authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_authorize_missing_header_rejected() {
        let config = test_config(Some("secret"));
        let headers: Vec<(String, String)> = vec![];
        let err = authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_authorize_no_bearer_token_configured() {
        let config = test_config(None);
        let headers = vec![("authorization".to_string(), "Bearer x".to_string())];
        let err = authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "503 Service Unavailable");
    }

    #[test]
    fn test_authorize_basic_scheme_rejected() {
        let config = test_config(Some("secret"));
        let headers = vec![("authorization".to_string(), "Basic secret".to_string())];
        let err = authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_authorize_bearer_with_extra_whitespace() {
        let config = test_config(Some("tok"));
        let headers = vec![("authorization".to_string(), "Bearer   tok  ".to_string())];
        assert!(authorize_request_headers(&headers, &config).is_ok());
    }

    #[test]
    fn test_authorize_empty_token_value_rejected() {
        let config = test_config(Some("secret"));
        let headers = vec![("authorization".to_string(), "Bearer ".to_string())];
        let err = authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    // ── authorize_signed_request ──

    #[test]
    fn test_signed_request_no_keys_configured() {
        let config = test_config(None); // signing_keys is empty
        let request = test_request("GET", "/images/by-path", vec![]);
        let q = query(&[
            ("keyId", "k"),
            ("expires", "9999999999"),
            ("signature", "aa"),
        ]);
        let err = authorize_signed_request(&request, &q, &config).unwrap_err();
        assert_eq!(err.status, "503 Service Unavailable");
    }

    #[test]
    fn test_signed_request_missing_key_id() {
        let config = test_config(None).with_signed_url_credentials("k1", "secret");
        let request = test_request("GET", "/images/by-path", vec![("host", "example.com")]);
        let q = query(&[("expires", "9999999999"), ("signature", "aa")]);
        let err = authorize_signed_request(&request, &q, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_signed_request_unknown_key_id() {
        let config = test_config(None).with_signed_url_credentials("k1", "secret");
        let request = test_request("GET", "/images/by-path", vec![("host", "example.com")]);
        let q = query(&[
            ("keyId", "unknown"),
            ("expires", "9999999999"),
            ("signature", "aa"),
        ]);
        let err = authorize_signed_request(&request, &q, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_signed_request_expired() {
        let config = test_config(None).with_signed_url_credentials("k1", "secret");
        let request = test_request("GET", "/images/by-path", vec![("host", "example.com")]);
        let q = query(&[
            ("keyId", "k1"),
            ("expires", "1"), // epoch second 1 is definitely expired
            ("signature", "aa"),
        ]);
        let err = authorize_signed_request(&request, &q, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_signed_request_invalid_expires() {
        let config = test_config(None).with_signed_url_credentials("k1", "secret");
        let request = test_request("GET", "/images/by-path", vec![("host", "example.com")]);
        let q = query(&[
            ("keyId", "k1"),
            ("expires", "not-a-number"),
            ("signature", "aa"),
        ]);
        let err = authorize_signed_request(&request, &q, &config).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_signed_request_valid_signature() {
        let secret = "test-secret";
        let mut config = test_config(None).with_signed_url_credentials("k1", secret);
        config.public_base_url = Some("https://example.com".to_string());

        let expires = "9999999999";
        let path = "/images/by-path";
        let _target =
            format!("{path}?keyId=k1&expires={expires}&path=photo.jpg&signature=placeholder");

        // Build the canonical form to compute the correct HMAC.
        let mut params = BTreeMap::new();
        params.insert("keyId".to_string(), "k1".to_string());
        params.insert("expires".to_string(), expires.to_string());
        params.insert("path".to_string(), "photo.jpg".to_string());

        let canonical_q = canonical_query_without_signature(&params);
        let canonical = format!("GET\nexample.com\n{path}\n{canonical_q}");

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(canonical.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        params.insert("signature".to_string(), sig.clone());

        let request = test_request(
            "GET",
            &format!("{path}?keyId=k1&expires={expires}&path=photo.jpg&signature={sig}"),
            vec![("host", "example.com")],
        );

        assert!(authorize_signed_request(&request, &params, &config).is_ok());
    }

    #[test]
    fn test_signed_request_wrong_signature() {
        let mut config = test_config(None).with_signed_url_credentials("k1", "secret");
        config.public_base_url = Some("https://example.com".to_string());

        let request = test_request(
            "GET",
            "/images/by-path?keyId=k1&expires=9999999999&signature=deadbeef",
            vec![("host", "example.com")],
        );
        let q = query(&[
            ("keyId", "k1"),
            ("expires", "9999999999"),
            ("signature", "deadbeef"),
        ]);
        let err = authorize_signed_request(&request, &q, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_signed_request_non_hex_signature() {
        let mut config = test_config(None).with_signed_url_credentials("k1", "secret");
        config.public_base_url = Some("https://example.com".to_string());

        let request = test_request("GET", "/images/by-path", vec![("host", "example.com")]);
        let q = query(&[
            ("keyId", "k1"),
            ("expires", "9999999999"),
            ("signature", "not-hex!!"),
        ]);
        let err = authorize_signed_request(&request, &q, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    // ── canonical_request_authority ──

    #[test]
    fn test_authority_from_public_base_url() {
        let mut config = test_config(None);
        config.public_base_url = Some("https://cdn.example.com:8443/ignored".to_string());
        let request = test_request("GET", "/path", vec![("host", "internal.local")]);
        let authority = canonical_request_authority(&request, &config).unwrap();
        assert_eq!(authority, "cdn.example.com:8443");
    }

    #[test]
    fn test_authority_from_public_base_url_no_port() {
        let mut config = test_config(None);
        config.public_base_url = Some("https://cdn.example.com".to_string());
        let request = test_request("GET", "/path", vec![("host", "internal.local")]);
        let authority = canonical_request_authority(&request, &config).unwrap();
        assert_eq!(authority, "cdn.example.com");
    }

    #[test]
    fn test_authority_falls_back_to_host_header() {
        let config = test_config(None); // no public_base_url
        let request = test_request("GET", "/path", vec![("host", "myhost.com")]);
        let authority = canonical_request_authority(&request, &config).unwrap();
        assert_eq!(authority, "myhost.com");
    }

    #[test]
    fn test_authority_trims_host_header() {
        let config = test_config(None);
        let request = test_request("GET", "/path", vec![("host", "  myhost.com  ")]);
        let authority = canonical_request_authority(&request, &config).unwrap();
        assert_eq!(authority, "myhost.com");
    }

    #[test]
    fn test_authority_missing_host_header() {
        let config = test_config(None);
        let request = test_request("GET", "/path", vec![]);
        let err = canonical_request_authority(&request, &config).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_authority_empty_host_header() {
        let config = test_config(None);
        let request = test_request("GET", "/path", vec![("host", "")]);
        let err = canonical_request_authority(&request, &config).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_authority_whitespace_only_host_header() {
        let config = test_config(None);
        let request = test_request("GET", "/path", vec![("host", "   ")]);
        let err = canonical_request_authority(&request, &config).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ── url_authority ──

    #[test]
    fn test_url_authority_with_port() {
        let url = Url::parse("https://example.com:9090/path").unwrap();
        assert_eq!(url_authority(&url).unwrap(), "example.com:9090");
    }

    #[test]
    fn test_url_authority_without_port() {
        let url = Url::parse("https://example.com/path").unwrap();
        assert_eq!(url_authority(&url).unwrap(), "example.com");
    }

    #[test]
    fn test_url_authority_no_host() {
        let url = Url::parse("file:///tmp/image.png").unwrap();
        assert!(url_authority(&url).is_err());
    }

    // ── canonical_query_without_signature ──

    #[test]
    fn test_canonical_query_excludes_signature() {
        let q = query(&[
            ("expires", "123"),
            ("keyId", "k1"),
            ("signature", "abc"),
            ("width", "100"),
        ]);
        let result = canonical_query_without_signature(&q);
        assert!(!result.contains("signature"));
        assert!(result.contains("expires=123"));
        assert!(result.contains("keyId=k1"));
        assert!(result.contains("width=100"));
    }

    #[test]
    fn test_canonical_query_empty_map() {
        let q: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(canonical_query_without_signature(&q), "");
    }

    #[test]
    fn test_canonical_query_deterministic_order() {
        let q = query(&[("z", "1"), ("a", "2"), ("m", "3")]);
        let result = canonical_query_without_signature(&q);
        // BTreeMap sorts keys, so order should be a, m, z
        assert_eq!(result, "a=2&m=3&z=1");
    }

    #[test]
    fn test_canonical_query_url_encodes_values() {
        let q = query(&[("path", "photos/my image.jpg")]);
        let result = canonical_query_without_signature(&q);
        assert!(result.contains("photos"));
        assert!(!result.contains(' ')); // space must be encoded
    }

    // ── parse_query_params ──

    #[test]
    fn test_parse_query_params_basic() {
        let request = test_request("GET", "/path?width=100&height=200", vec![]);
        let params = parse_query_params(&request).unwrap();
        assert_eq!(params.get("width").unwrap(), "100");
        assert_eq!(params.get("height").unwrap(), "200");
    }

    #[test]
    fn test_parse_query_params_no_query() {
        let request = test_request("GET", "/path", vec![]);
        let params = parse_query_params(&request).unwrap();
        assert!(params.is_empty());
    }

    #[test]
    fn test_parse_query_params_duplicate_rejected() {
        let request = test_request("GET", "/path?width=100&width=200", vec![]);
        let err = parse_query_params(&request).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        let body = String::from_utf8_lossy(&err.body);
        assert!(body.contains("width"));
    }

    #[test]
    fn test_parse_query_params_url_decoded() {
        let request = test_request("GET", "/path?path=my%20photo.jpg", vec![]);
        let params = parse_query_params(&request).unwrap();
        assert_eq!(params.get("path").unwrap(), "my photo.jpg");
    }

    // ── validate_public_query_names ──

    #[test]
    fn test_validate_query_names_all_allowed_path() {
        let q = query(&[
            ("path", "img.jpg"),
            ("width", "100"),
            ("height", "200"),
            ("format", "webp"),
            ("quality", "80"),
            ("keyId", "k1"),
            ("expires", "123"),
            ("signature", "abc"),
        ]);
        assert!(validate_public_query_names(&q, PublicSourceKind::Path).is_ok());
    }

    #[test]
    fn test_validate_query_names_url_source_allows_url() {
        let q = query(&[("url", "https://example.com/img.jpg")]);
        assert!(validate_public_query_names(&q, PublicSourceKind::Url).is_ok());
    }

    #[test]
    fn test_validate_query_names_path_source_rejects_url_param() {
        let q = query(&[("url", "https://example.com/img.jpg")]);
        let err = validate_public_query_names(&q, PublicSourceKind::Path).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_validate_query_names_url_source_rejects_path_param() {
        let q = query(&[("path", "img.jpg")]);
        let err = validate_public_query_names(&q, PublicSourceKind::Url).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_validate_query_names_unknown_param_rejected() {
        let q = query(&[("path", "img.jpg"), ("bogus", "value")]);
        let err = validate_public_query_names(&q, PublicSourceKind::Path).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        let body = String::from_utf8_lossy(&err.body);
        assert!(body.contains("bogus"));
    }

    #[test]
    fn test_validate_query_names_transform_params() {
        let q = query(&[
            ("url", "x"),
            ("fit", "cover"),
            ("position", "center"),
            ("background", "FF0000"),
            ("rotate", "90"),
            ("autoOrient", "true"),
            ("stripMetadata", "false"),
            ("preserveExif", "true"),
            ("crop", "10,10,100,100"),
            ("blur", "2.5"),
            ("sharpen", "1.0"),
            ("version", "v2"),
            ("preset", "thumb"),
        ]);
        assert!(validate_public_query_names(&q, PublicSourceKind::Url).is_ok());
    }

    // ── encode_background ──

    #[test]
    fn test_encode_background_opaque() {
        let color = Rgba8 {
            r: 255,
            g: 0,
            b: 128,
            a: 255,
        };
        assert_eq!(encode_background(color), "FF0080");
    }

    #[test]
    fn test_encode_background_transparent() {
        let color = Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        assert_eq!(encode_background(color), "00000000");
    }

    #[test]
    fn test_encode_background_semi_transparent() {
        let color = Rgba8 {
            r: 255,
            g: 255,
            b: 255,
            a: 128,
        };
        assert_eq!(encode_background(color), "FFFFFF80");
    }

    #[test]
    fn test_encode_background_black_opaque() {
        let color = Rgba8 {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        assert_eq!(encode_background(color), "000000");
    }

    // ── parse_optional_integer_query ──

    #[test]
    fn test_parse_integer_valid() {
        let q = query(&[("width", "1024")]);
        assert_eq!(
            parse_optional_integer_query(&q, "width").unwrap(),
            Some(1024)
        );
    }

    #[test]
    fn test_parse_integer_absent() {
        let q: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(parse_optional_integer_query(&q, "width").unwrap(), None);
    }

    #[test]
    fn test_parse_integer_invalid() {
        let q = query(&[("width", "abc")]);
        let err = parse_optional_integer_query(&q, "width").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_integer_negative() {
        let q = query(&[("width", "-5")]);
        let err = parse_optional_integer_query(&q, "width").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_integer_overflow() {
        let q = query(&[("width", "99999999999999")]);
        let err = parse_optional_integer_query(&q, "width").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ── parse_optional_u8_query ──

    #[test]
    fn test_parse_u8_valid() {
        let q = query(&[("quality", "80")]);
        assert_eq!(parse_optional_u8_query(&q, "quality").unwrap(), Some(80));
    }

    #[test]
    fn test_parse_u8_absent() {
        let q: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(parse_optional_u8_query(&q, "quality").unwrap(), None);
    }

    #[test]
    fn test_parse_u8_overflow() {
        let q = query(&[("quality", "256")]);
        let err = parse_optional_u8_query(&q, "quality").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_u8_boundary_max() {
        let q = query(&[("quality", "255")]);
        assert_eq!(parse_optional_u8_query(&q, "quality").unwrap(), Some(255));
    }

    // ── parse_optional_float_query ──

    #[test]
    fn test_parse_float_valid() {
        let q = query(&[("blur", "2.5")]);
        assert_eq!(parse_optional_float_query(&q, "blur").unwrap(), Some(2.5));
    }

    #[test]
    fn test_parse_float_integer() {
        let q = query(&[("blur", "3")]);
        assert_eq!(parse_optional_float_query(&q, "blur").unwrap(), Some(3.0));
    }

    #[test]
    fn test_parse_float_absent() {
        let q: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(parse_optional_float_query(&q, "blur").unwrap(), None);
    }

    #[test]
    fn test_parse_float_invalid() {
        let q = query(&[("blur", "xyz")]);
        let err = parse_optional_float_query(&q, "blur").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ── parse_optional_bool_query ──

    #[test]
    fn test_parse_bool_true() {
        let q = query(&[("autoOrient", "true")]);
        assert_eq!(
            parse_optional_bool_query(&q, "autoOrient").unwrap(),
            Some(true)
        );
    }

    #[test]
    fn test_parse_bool_false() {
        let q = query(&[("autoOrient", "false")]);
        assert_eq!(
            parse_optional_bool_query(&q, "autoOrient").unwrap(),
            Some(false)
        );
    }

    #[test]
    fn test_parse_bool_absent() {
        let q: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(parse_optional_bool_query(&q, "autoOrient").unwrap(), None);
    }

    #[test]
    fn test_parse_bool_invalid() {
        let q = query(&[("autoOrient", "yes")]);
        let err = parse_optional_bool_query(&q, "autoOrient").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_bool_one_rejected() {
        // "1" is not accepted, only "true"/"false"
        let q = query(&[("autoOrient", "1")]);
        let err = parse_optional_bool_query(&q, "autoOrient").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ── required_query_param / required_auth_query_param ──

    #[test]
    fn test_required_query_param_present() {
        let q = query(&[("path", "img.jpg")]);
        assert_eq!(required_query_param(&q, "path").unwrap(), "img.jpg");
    }

    #[test]
    fn test_required_query_param_missing() {
        let q: BTreeMap<String, String> = BTreeMap::new();
        let err = required_query_param(&q, "path").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_required_query_param_empty_value() {
        let q = query(&[("path", "")]);
        let err = required_query_param(&q, "path").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_required_auth_query_param_missing_returns_401() {
        let q: BTreeMap<String, String> = BTreeMap::new();
        let err = required_auth_query_param(&q, "keyId").unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn test_required_auth_query_param_empty_returns_401() {
        let q = query(&[("keyId", "")]);
        let err = required_auth_query_param(&q, "keyId").unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    // ── signed_source_query ──

    #[test]
    fn test_signed_source_query_path_with_version() {
        let q = signed_source_query(SignedUrlSource::Path {
            path: "photos/a.jpg".to_string(),
            version: Some("v2".to_string()),
        });
        assert_eq!(q.get("path").unwrap(), "photos/a.jpg");
        assert_eq!(q.get("version").unwrap(), "v2");
        assert!(!q.contains_key("url"));
    }

    #[test]
    fn test_signed_source_query_url_no_version() {
        let q = signed_source_query(SignedUrlSource::Url {
            url: "https://example.com/img.png".to_string(),
            version: None,
        });
        assert_eq!(q.get("url").unwrap(), "https://example.com/img.png");
        assert!(!q.contains_key("version"));
        assert!(!q.contains_key("path"));
    }

    // ── authorize_signed_request: key rotation ──

    #[test]
    fn test_signed_request_key_rotation_accepts_both_keys() {
        let secret_old = "old-secret";
        let secret_new = "new-secret";
        let mut keys = HashMap::new();
        keys.insert("old".to_string(), secret_old.to_string());
        keys.insert("new".to_string(), secret_new.to_string());
        let mut config = test_config(None).with_signing_keys(keys);
        config.public_base_url = Some("https://example.com".to_string());

        // Sign with the OLD key
        let expires = "9999999999";
        let path = "/images/by-path";
        let mut params = BTreeMap::new();
        params.insert("keyId".to_string(), "old".to_string());
        params.insert("expires".to_string(), expires.to_string());
        params.insert("path".to_string(), "img.jpg".to_string());

        let canonical_q = canonical_query_without_signature(&params);
        let canonical = format!("GET\nexample.com\n{path}\n{canonical_q}");
        let mut mac = HmacSha256::new_from_slice(secret_old.as_bytes()).unwrap();
        mac.update(canonical.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());
        params.insert("signature".to_string(), sig.clone());

        let request = test_request("GET", path, vec![("host", "example.com")]);
        assert!(authorize_signed_request(&request, &params, &config).is_ok());
    }
}
