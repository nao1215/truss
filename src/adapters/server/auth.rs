use super::http_parse::HttpRequest;
use super::negotiate::PublicSourceKind;
use super::response::{
    HttpResponse, auth_required_response, bad_request_response, internal_error_response,
    service_unavailable_response, signed_url_unauthorized_response,
};
use super::{HmacSha256, ServerConfig, SignedUrlSource};
use crate::{Rotation, Rgba8, TransformOptions};
use hmac::Mac;
use std::collections::BTreeMap;
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
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    match provided {
        Some(token) if token == expected => Ok(()),
        _ => Err(auth_required_response("authorization required")),
    }
}

pub(super) fn authorize_signed_request(
    request: &HttpRequest,
    query: &BTreeMap<String, String>,
    config: &ServerConfig,
) -> Result<(), HttpResponse> {
    let expected_key_id = config
        .signed_url_key_id
        .as_deref()
        .ok_or_else(|| service_unavailable_response("public signed URL key is not configured"))?;
    let secret = config.signed_url_secret.as_deref().ok_or_else(|| {
        service_unavailable_response("public signed URL secret is not configured")
    })?;
    let key_id = required_auth_query_param(query, "keyId")?;
    let expires = required_auth_query_param(query, "expires")?;
    let signature = required_auth_query_param(query, "signature")?;

    if key_id != expected_key_id {
        return Err(signed_url_unauthorized_response(
            "signed URL is invalid or expired",
        ));
    }

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
        if params.insert(name.clone(), value).is_some() {
            return Err(bad_request_response(&format!(
                "query parameter `{name}` must not be repeated"
            )));
        }
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
