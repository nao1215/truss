use crate::{
    sniff_artifact, transform_raster, Fit, MediaType, Position, RawArtifact, Rgba8, Rotation,
    TransformError, TransformOptions, TransformRequest,
};
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use url::Url;

/// The default bind address for the development HTTP server.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

/// The default storage root used by the server adapter.
pub const DEFAULT_STORAGE_ROOT: &str = ".";

const HEALTH_BODY: &str = "{\"status\":\"ok\"}\n";
const NOT_FOUND_BODY: &str = "{\"error\":\"not found\"}\n";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_REMOTE_REDIRECTS: usize = 5;

static HTTP_REQUESTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_HEALTH_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_HEALTH_LIVE_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_HEALTH_READY_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_TRANSFORM_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_UPLOAD_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_METRICS_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_UNKNOWN_TOTAL: AtomicU64 = AtomicU64::new(0);

static HTTP_RESPONSES_200_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_400_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_401_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_403_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_404_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_413_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_415_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_500_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_501_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_502_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_503_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_508_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_OTHER_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Runtime configuration for the HTTP server adapter.
///
/// The HTTP adapter keeps environment-specific concerns, such as the storage root and
/// authentication secret, outside the Core transformation API. Tests and embedding runtimes
/// can construct this value directly, while the CLI entry point typically uses
/// [`ServerConfig::from_env`] to load the same fields from process environment variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    /// The storage root used for `source.kind=path` lookups.
    pub storage_root: PathBuf,
    /// The expected Bearer token for private endpoints.
    pub bearer_token: Option<String>,
    /// The externally visible base URL for future public endpoint generation.
    ///
    /// The current runtime stores this value for configuration completeness, but it does not yet
    /// emit public URLs because the signed public GET endpoints are still unimplemented.
    pub public_base_url: Option<String>,
    /// Whether server-side URL sources may bypass private-network and port restrictions.
    ///
    /// This flag is intended for local development and automated tests where fixture servers
    /// commonly run on loopback addresses and non-standard ports. Production-like configurations
    /// should keep this disabled.
    pub allow_insecure_url_sources: bool,
}

impl ServerConfig {
    /// Creates a server configuration from explicit values.
    ///
    /// This constructor does not canonicalize the storage root. It is primarily intended for
    /// tests and embedding scenarios where the caller already controls the filesystem layout.
    ///
    /// # Examples
    ///
    /// ```
    /// use truss::adapters::server::ServerConfig;
    ///
    /// let config = ServerConfig::new(std::env::temp_dir(), Some("secret".to_string()));
    ///
    /// assert_eq!(config.bearer_token.as_deref(), Some("secret"));
    /// ```
    pub fn new(storage_root: PathBuf, bearer_token: Option<String>) -> Self {
        Self {
            storage_root,
            bearer_token,
            public_base_url: None,
            allow_insecure_url_sources: false,
        }
    }

    /// Returns a copy of the configuration with insecure URL source allowances toggled.
    ///
    /// Enabling this flag allows URL sources that target loopback or private-network addresses
    /// and permits non-standard ports. This is useful for local integration tests but weakens
    /// the default SSRF protections of the server adapter.
    ///
    /// # Examples
    ///
    /// ```
    /// use truss::adapters::server::ServerConfig;
    ///
    /// let config = ServerConfig::new(std::env::temp_dir(), Some("secret".to_string()))
    ///     .with_insecure_url_sources(true);
    ///
    /// assert!(config.allow_insecure_url_sources);
    /// ```
    pub fn with_insecure_url_sources(mut self, allow_insecure_url_sources: bool) -> Self {
        self.allow_insecure_url_sources = allow_insecure_url_sources;
        self
    }

    /// Loads server configuration from environment variables.
    ///
    /// The adapter currently reads:
    ///
    /// - `TRUSS_STORAGE_ROOT`: filesystem root for `source.kind=path` inputs. Defaults to the
    ///   current directory and is canonicalized before use.
    /// - `TRUSS_BEARER_TOKEN`: private API Bearer token. When this value is missing, private
    ///   endpoints remain unavailable and return `503 Service Unavailable`.
    /// - `TRUSS_PUBLIC_BASE_URL`: externally visible base URL reserved for future public endpoint
    ///   generation. When set, it must parse as an absolute `http` or `https` URL.
    /// - `TRUSS_ALLOW_INSECURE_URL_SOURCES`: when set to `1`, `true`, `yes`, or `on`, URL
    ///   sources may target loopback or private-network addresses and non-standard ports.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] when the configured storage root does not exist or cannot be
    /// canonicalized.
    pub fn from_env() -> io::Result<Self> {
        let storage_root =
            env::var("TRUSS_STORAGE_ROOT").unwrap_or_else(|_| DEFAULT_STORAGE_ROOT.to_string());
        let storage_root = PathBuf::from(storage_root).canonicalize()?;
        let bearer_token = env::var("TRUSS_BEARER_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        let public_base_url = env::var("TRUSS_PUBLIC_BASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .map(validate_public_base_url)
            .transpose()?;

        Ok(Self {
            storage_root,
            bearer_token,
            public_base_url,
            allow_insecure_url_sources: env_flag("TRUSS_ALLOW_INSECURE_URL_SOURCES"),
        })
    }
}

/// Returns the bind address for the HTTP server adapter.
///
/// The adapter reads `TRUSS_BIND_ADDR` when it is present. Otherwise it falls back to
/// [`DEFAULT_BIND_ADDR`].
pub fn bind_addr() -> String {
    env::var("TRUSS_BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
}

/// Serves requests until the listener stops producing connections.
///
/// This helper loads [`ServerConfig`] from the process environment and then delegates to
/// [`serve_with_config`]. Health endpoints remain available even when the private API is not
/// configured, but authenticated transform requests will return `503 Service Unavailable`
/// unless `TRUSS_BEARER_TOKEN` is set.
///
/// # Errors
///
/// Returns an [`io::Error`] when the storage root cannot be resolved, when accepting the next
/// connection fails, or when a response cannot be written to the socket.
pub fn serve(listener: TcpListener) -> io::Result<()> {
    let config = ServerConfig::from_env()?;
    serve_with_config(listener, &config)
}

/// Serves requests with an explicit server configuration.
///
/// This is the adapter entry point for tests and embedding scenarios that want deterministic
/// configuration instead of environment-variable lookup.
///
/// # Errors
///
/// Returns an [`io::Error`] when accepting the next connection fails or when a response cannot
/// be written to the socket.
pub fn serve_with_config(listener: TcpListener, config: &ServerConfig) -> io::Result<()> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_stream(stream, config) {
                    eprintln!("failed to handle connection: {err}");
                }
            }
            Err(err) => return Err(err),
        }
    }

    Ok(())
}

/// Serves exactly one request using configuration loaded from the environment.
///
/// This helper is primarily useful in tests that want to drive the server over a real TCP
/// socket but do not need a long-running loop.
///
/// # Errors
///
/// Returns an [`io::Error`] when the storage root cannot be resolved, when accepting the next
/// connection fails, or when a response cannot be written to the socket.
pub fn serve_once(listener: TcpListener) -> io::Result<()> {
    let config = ServerConfig::from_env()?;
    serve_once_with_config(listener, &config)
}

/// Serves exactly one request with an explicit server configuration.
///
/// # Errors
///
/// Returns an [`io::Error`] when accepting the next connection fails or when a response cannot
/// be written to the socket.
pub fn serve_once_with_config(listener: TcpListener, config: &ServerConfig) -> io::Result<()> {
    let (stream, _) = listener.accept()?;
    handle_stream(stream, config)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(header_name, value)| (header_name == name).then_some(value.as_str()))
    }

    fn path(&self) -> &str {
        self.target
            .split('?')
            .next()
            .unwrap_or(self.target.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpResponse {
    status: &'static str,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some("application/json".to_string()),
            headers: Vec::new(),
            body,
        }
    }

    fn json_with_headers(
        status: &'static str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            content_type: Some("application/json".to_string()),
            headers,
            body,
        }
    }

    fn binary(status: &'static str, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some(content_type.to_string()),
            headers: Vec::new(),
            body,
        }
    }

    fn text(status: &'static str, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some(content_type.to_string()),
            headers: Vec::new(),
            body,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransformImageRequestPayload {
    source: TransformSourcePayload,
    #[serde(default)]
    options: TransformOptionsPayload,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum TransformSourcePayload {
    Path {
        path: String,
        #[allow(dead_code)]
        version: Option<String>,
    },
    Url {
        #[allow(dead_code)]
        url: String,
        #[allow(dead_code)]
        version: Option<String>,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct TransformOptionsPayload {
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<String>,
    position: Option<String>,
    format: Option<String>,
    quality: Option<u8>,
    background: Option<String>,
    rotate: Option<u16>,
    auto_orient: Option<bool>,
    strip_metadata: Option<bool>,
    preserve_exif: Option<bool>,
}

impl TransformOptionsPayload {
    fn into_options(self) -> Result<TransformOptions, HttpResponse> {
        let defaults = TransformOptions::default();

        Ok(TransformOptions {
            width: self.width,
            height: self.height,
            fit: parse_optional_named(self.fit.as_deref(), "fit", Fit::from_str)?,
            position: parse_optional_named(
                self.position.as_deref(),
                "position",
                Position::from_str,
            )?,
            format: parse_optional_named(self.format.as_deref(), "format", MediaType::from_str)?,
            quality: self.quality,
            background: parse_optional_named(
                self.background.as_deref(),
                "background",
                Rgba8::from_hex,
            )?,
            rotate: match self.rotate {
                Some(value) => parse_named(&value.to_string(), "rotate", Rotation::from_str)?,
                None => defaults.rotate,
            },
            auto_orient: self.auto_orient.unwrap_or(defaults.auto_orient),
            strip_metadata: self.strip_metadata.unwrap_or(defaults.strip_metadata),
            preserve_exif: self.preserve_exif.unwrap_or(defaults.preserve_exif),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MultipartPart {
    name: String,
    content_type: Option<String>,
    body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteMetric {
    Health,
    HealthLive,
    HealthReady,
    Transform,
    Upload,
    Metrics,
    Unknown,
}

impl RouteMetric {
    const fn as_label(self) -> &'static str {
        match self {
            Self::Health => "/health",
            Self::HealthLive => "/health/live",
            Self::HealthReady => "/health/ready",
            Self::Transform => "/images:transform",
            Self::Upload => "/images",
            Self::Metrics => "/metrics",
            Self::Unknown => "<unknown>",
        }
    }
}

fn handle_stream(mut stream: TcpStream, config: &ServerConfig) -> io::Result<()> {
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(response) => return write_response(&mut stream, response),
    };
    let route = classify_route(&request);
    let response = route_request(request, config);
    record_http_metrics(route, response.status);

    write_response(&mut stream, response)
}

fn route_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    let method = request.method.clone();
    let path = request.path().to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/health") | ("GET", "/health/live") | ("GET", "/health/ready") => {
            HttpResponse::json("200 OK", HEALTH_BODY.as_bytes().to_vec())
        }
        ("POST", "/images:transform") => handle_transform_request(request, config),
        ("POST", "/images") => handle_upload_request(request, config),
        ("GET", "/metrics") => handle_metrics_request(request, config),
        _ => HttpResponse::json("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec()),
    }
}

fn classify_route(request: &HttpRequest) -> RouteMetric {
    match (request.method.as_str(), request.path()) {
        ("GET", "/health") => RouteMetric::Health,
        ("GET", "/health/live") => RouteMetric::HealthLive,
        ("GET", "/health/ready") => RouteMetric::HealthReady,
        ("POST", "/images:transform") => RouteMetric::Transform,
        ("POST", "/images") => RouteMetric::Upload,
        ("GET", "/metrics") => RouteMetric::Metrics,
        _ => RouteMetric::Unknown,
    }
}

fn handle_transform_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    if !request_has_json_content_type(&request) {
        return unsupported_media_type_response("content-type must be application/json");
    }

    let payload: TransformImageRequestPayload = match serde_json::from_slice(&request.body) {
        Ok(payload) => payload,
        Err(error) => {
            return bad_request_response(&format!("request body must be valid JSON: {error}"));
        }
    };
    let options = match payload.options.into_options() {
        Ok(options) => options,
        Err(response) => return response,
    };
    let source_bytes = match resolve_source_bytes(payload.source, config) {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    transform_source_bytes(source_bytes, options)
}

fn handle_upload_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    let boundary = match parse_multipart_boundary(&request) {
        Ok(boundary) => boundary,
        Err(response) => return response,
    };
    let (file_bytes, options) = match parse_upload_request(&request.body, &boundary) {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    transform_source_bytes(file_bytes, options)
}

fn handle_metrics_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    HttpResponse::text(
        "200 OK",
        "text/plain; version=0.0.4; charset=utf-8",
        render_metrics_text().into_bytes(),
    )
}

fn transform_source_bytes(source_bytes: Vec<u8>, options: TransformOptions) -> HttpResponse {
    let artifact = match sniff_artifact(RawArtifact::new(source_bytes, None)) {
        Ok(artifact) => artifact,
        Err(error) => return transform_error_response(error),
    };
    let output = match transform_raster(TransformRequest::new(artifact, options)) {
        Ok(output) => output,
        Err(error) => return transform_error_response(error),
    };

    HttpResponse::binary("200 OK", output.media_type.as_mime(), output.bytes)
}

fn parse_upload_request(
    body: &[u8],
    boundary: &str,
) -> Result<(Vec<u8>, TransformOptions), HttpResponse> {
    let parts = parse_multipart_form_data(body, boundary)?;
    let mut file_bytes = None;
    let mut options = None;

    for part in parts {
        match part.name.as_str() {
            "file" => {
                if file_bytes.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `file` fields",
                    ));
                }
                if part.body.is_empty() {
                    return Err(bad_request_response(
                        "multipart upload `file` field must not be empty",
                    ));
                }
                file_bytes = Some(part.body);
            }
            "options" => {
                if options.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `options` fields",
                    ));
                }
                if let Some(content_type) = part.content_type.as_deref() {
                    if !content_type_matches(content_type, "application/json") {
                        return Err(bad_request_response(
                            "multipart upload `options` field must use application/json when a content type is provided",
                        ));
                    }
                }
                let payload = if part.body.is_empty() {
                    TransformOptionsPayload::default()
                } else {
                    serde_json::from_slice::<TransformOptionsPayload>(&part.body).map_err(
                        |error| {
                            bad_request_response(&format!(
                                "multipart upload `options` field must contain valid JSON: {error}"
                            ))
                        },
                    )?
                };
                options = Some(payload.into_options()?);
            }
            field_name => {
                return Err(bad_request_response(&format!(
                    "multipart upload contains an unsupported field `{field_name}`"
                )));
            }
        }
    }

    let file_bytes = file_bytes
        .ok_or_else(|| bad_request_response("multipart upload requires a `file` field"))?;

    Ok((file_bytes, options.unwrap_or_default()))
}

fn record_http_metrics(route: RouteMetric, status: &str) {
    HTTP_REQUESTS_TOTAL.fetch_add(1, Ordering::Relaxed);
    route_counter(route).fetch_add(1, Ordering::Relaxed);
    status_counter(status).fetch_add(1, Ordering::Relaxed);
}

fn render_metrics_text() -> String {
    let mut body = String::new();
    body.push_str(
        "# HELP truss_process_up Whether the server adapter considers the process alive.\n",
    );
    body.push_str("# TYPE truss_process_up gauge\n");
    body.push_str("truss_process_up 1\n");

    body.push_str("# HELP truss_http_requests_total Total parsed HTTP requests handled by the server adapter.\n");
    body.push_str("# TYPE truss_http_requests_total counter\n");
    body.push_str(&format!(
        "truss_http_requests_total {}\n",
        HTTP_REQUESTS_TOTAL.load(Ordering::Relaxed)
    ));

    body.push_str(
        "# HELP truss_http_requests_by_route_total Total parsed HTTP requests handled by route.\n",
    );
    body.push_str("# TYPE truss_http_requests_by_route_total counter\n");
    for route in [
        RouteMetric::Health,
        RouteMetric::HealthLive,
        RouteMetric::HealthReady,
        RouteMetric::Transform,
        RouteMetric::Upload,
        RouteMetric::Metrics,
        RouteMetric::Unknown,
    ] {
        body.push_str(&format!(
            "truss_http_requests_by_route_total{{route=\"{}\"}} {}\n",
            route.as_label(),
            route_counter(route).load(Ordering::Relaxed)
        ));
    }

    body.push_str(
        "# HELP truss_http_responses_total Total HTTP responses emitted by status code.\n",
    );
    body.push_str("# TYPE truss_http_responses_total counter\n");
    for status in [
        "200", "400", "401", "403", "404", "413", "415", "500", "501", "502", "503", "508", "other",
    ] {
        body.push_str(&format!(
            "truss_http_responses_total{{status=\"{status}\"}} {}\n",
            status_counter_value(status)
        ));
    }

    body
}

fn route_counter(route: RouteMetric) -> &'static AtomicU64 {
    match route {
        RouteMetric::Health => &HTTP_REQUESTS_HEALTH_TOTAL,
        RouteMetric::HealthLive => &HTTP_REQUESTS_HEALTH_LIVE_TOTAL,
        RouteMetric::HealthReady => &HTTP_REQUESTS_HEALTH_READY_TOTAL,
        RouteMetric::Transform => &HTTP_REQUESTS_TRANSFORM_TOTAL,
        RouteMetric::Upload => &HTTP_REQUESTS_UPLOAD_TOTAL,
        RouteMetric::Metrics => &HTTP_REQUESTS_METRICS_TOTAL,
        RouteMetric::Unknown => &HTTP_REQUESTS_UNKNOWN_TOTAL,
    }
}

fn status_counter(status: &str) -> &'static AtomicU64 {
    match status_code(status) {
        Some("200") => &HTTP_RESPONSES_200_TOTAL,
        Some("400") => &HTTP_RESPONSES_400_TOTAL,
        Some("401") => &HTTP_RESPONSES_401_TOTAL,
        Some("403") => &HTTP_RESPONSES_403_TOTAL,
        Some("404") => &HTTP_RESPONSES_404_TOTAL,
        Some("413") => &HTTP_RESPONSES_413_TOTAL,
        Some("415") => &HTTP_RESPONSES_415_TOTAL,
        Some("500") => &HTTP_RESPONSES_500_TOTAL,
        Some("501") => &HTTP_RESPONSES_501_TOTAL,
        Some("502") => &HTTP_RESPONSES_502_TOTAL,
        Some("503") => &HTTP_RESPONSES_503_TOTAL,
        Some("508") => &HTTP_RESPONSES_508_TOTAL,
        _ => &HTTP_RESPONSES_OTHER_TOTAL,
    }
}

fn status_counter_value(status: &str) -> u64 {
    match status {
        "200" => HTTP_RESPONSES_200_TOTAL.load(Ordering::Relaxed),
        "400" => HTTP_RESPONSES_400_TOTAL.load(Ordering::Relaxed),
        "401" => HTTP_RESPONSES_401_TOTAL.load(Ordering::Relaxed),
        "403" => HTTP_RESPONSES_403_TOTAL.load(Ordering::Relaxed),
        "404" => HTTP_RESPONSES_404_TOTAL.load(Ordering::Relaxed),
        "413" => HTTP_RESPONSES_413_TOTAL.load(Ordering::Relaxed),
        "415" => HTTP_RESPONSES_415_TOTAL.load(Ordering::Relaxed),
        "500" => HTTP_RESPONSES_500_TOTAL.load(Ordering::Relaxed),
        "501" => HTTP_RESPONSES_501_TOTAL.load(Ordering::Relaxed),
        "502" => HTTP_RESPONSES_502_TOTAL.load(Ordering::Relaxed),
        "503" => HTTP_RESPONSES_503_TOTAL.load(Ordering::Relaxed),
        "508" => HTTP_RESPONSES_508_TOTAL.load(Ordering::Relaxed),
        _ => HTTP_RESPONSES_OTHER_TOTAL.load(Ordering::Relaxed),
    }
}

fn status_code(status: &str) -> Option<&str> {
    status.split_whitespace().next()
}

fn authorize_request(request: &HttpRequest, config: &ServerConfig) -> Result<(), HttpResponse> {
    let expected = config.bearer_token.as_deref().ok_or_else(|| {
        service_unavailable_response("private API bearer token is not configured")
    })?;
    let provided = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    match provided {
        Some(token) if token == expected => Ok(()),
        _ => Err(auth_required_response("authorization required")),
    }
}

fn parse_multipart_boundary(request: &HttpRequest) -> Result<String, HttpResponse> {
    let Some(content_type) = request.header("content-type") else {
        return Err(unsupported_media_type_response(
            "content-type must be multipart/form-data",
        ));
    };

    let mut segments = content_type.split(';');
    let Some(media_type) = segments.next() else {
        return Err(unsupported_media_type_response(
            "content-type must be multipart/form-data",
        ));
    };
    if !content_type_matches(media_type, "multipart/form-data") {
        return Err(unsupported_media_type_response(
            "content-type must be multipart/form-data",
        ));
    }

    for segment in segments {
        let Some((name, value)) = segment.split_once('=') else {
            return Err(bad_request_response(
                "multipart content-type parameters must use name=value syntax",
            ));
        };
        if name.trim().eq_ignore_ascii_case("boundary") {
            let boundary = value.trim().trim_matches('"');
            if boundary.is_empty() {
                return Err(bad_request_response(
                    "multipart content-type boundary must not be empty",
                ));
            }
            return Ok(boundary.to_string());
        }
    }

    Err(bad_request_response(
        "multipart content-type requires a boundary parameter",
    ))
}

fn parse_multipart_form_data(
    body: &[u8],
    boundary: &str,
) -> Result<Vec<MultipartPart>, HttpResponse> {
    let opening = format!("--{boundary}").into_bytes();
    let delimiter = format!("\r\n--{boundary}").into_bytes();

    if !body.starts_with(&opening) {
        return Err(bad_request_response(
            "multipart body does not start with the declared boundary",
        ));
    }

    let mut cursor = 0;
    let mut parts = Vec::new();

    loop {
        if !body[cursor..].starts_with(&opening) {
            return Err(bad_request_response(
                "multipart boundary sequence is malformed",
            ));
        }
        cursor += opening.len();

        if body[cursor..].starts_with(b"--") {
            cursor += 2;
            if !body[cursor..].is_empty() && body[cursor..] != b"\r\n"[..] {
                return Err(bad_request_response(
                    "multipart closing boundary has unexpected trailing data",
                ));
            }
            break;
        }

        if !body[cursor..].starts_with(b"\r\n") {
            return Err(bad_request_response(
                "multipart boundary must be followed by CRLF",
            ));
        }
        cursor += 2;

        let header_end = find_subslice(&body[cursor..], b"\r\n\r\n")
            .ok_or_else(|| bad_request_response("multipart part is missing a header terminator"))?;
        let header_bytes = &body[cursor..(cursor + header_end)];
        let headers = parse_part_headers(header_bytes)?;
        cursor += header_end + 4;

        let body_end = find_subslice(&body[cursor..], &delimiter).ok_or_else(|| {
            bad_request_response("multipart part is missing the next boundary delimiter")
        })?;
        let part_body = body[cursor..(cursor + body_end)].to_vec();
        let part_name = parse_multipart_part_name(&headers)?;
        let content_type = header_value(&headers, "content-type").map(str::to_string);
        parts.push(MultipartPart {
            name: part_name,
            content_type,
            body: part_body,
        });

        cursor += body_end + 2;
    }

    Ok(parts)
}

fn parse_part_headers(header_bytes: &[u8]) -> Result<Vec<(String, String)>, HttpResponse> {
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| bad_request_response("multipart part headers must be valid UTF-8"))?;
    parse_headers(header_text.split("\r\n"))
}

fn parse_multipart_part_name(headers: &[(String, String)]) -> Result<String, HttpResponse> {
    let Some(disposition) = header_value(headers, "content-disposition") else {
        return Err(bad_request_response(
            "multipart part is missing a Content-Disposition header",
        ));
    };

    let mut segments = disposition.split(';');
    let Some(kind) = segments.next() else {
        return Err(bad_request_response(
            "multipart Content-Disposition header is malformed",
        ));
    };
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return Err(bad_request_response(
            "multipart Content-Disposition header must use form-data",
        ));
    }

    for segment in segments {
        let Some((name, value)) = segment.split_once('=') else {
            return Err(bad_request_response(
                "multipart Content-Disposition parameters must use name=value syntax",
            ));
        };
        if name.trim().eq_ignore_ascii_case("name") {
            let value = value.trim().trim_matches('"');
            if value.is_empty() {
                return Err(bad_request_response(
                    "multipart part name must not be empty",
                ));
            }
            return Ok(value.to_string());
        }
    }

    Err(bad_request_response(
        "multipart Content-Disposition header must include a name parameter",
    ))
}

fn resolve_source_bytes(
    source: TransformSourcePayload,
    config: &ServerConfig,
) -> Result<Vec<u8>, HttpResponse> {
    match source {
        TransformSourcePayload::Path { path, .. } => {
            let path = resolve_storage_path(&config.storage_root, &path)?;
            let metadata = fs::metadata(&path).map_err(map_source_io_error)?;
            if metadata.len() > MAX_SOURCE_BYTES {
                return Err(payload_too_large_response("source file is too large"));
            }

            fs::read(&path).map_err(map_source_io_error)
        }
        TransformSourcePayload::Url { url, .. } => read_remote_source_bytes(&url, config),
    }
}

fn read_remote_source_bytes(url: &str, config: &ServerConfig) -> Result<Vec<u8>, HttpResponse> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .build();
    let mut current_url = url.to_string();

    for redirect_index in 0..=MAX_REMOTE_REDIRECTS {
        let parsed = parse_remote_url(&current_url)?;
        validate_remote_url(&parsed, config)?;

        match agent.get(parsed.as_str()).call() {
            Ok(response) if is_redirect_status(response.status()) => {
                current_url = next_redirect_url(&parsed, &response, redirect_index)?;
            }
            Ok(response) => return read_remote_response_body(parsed.as_str(), response),
            Err(ureq::Error::Status(status, response)) if is_redirect_status(status) => {
                current_url = next_redirect_url(&parsed, &response, redirect_index)?;
            }
            Err(ureq::Error::Status(status, _)) => {
                return Err(bad_gateway_response(&format!(
                    "failed to fetch remote URL: upstream HTTP {status}"
                )));
            }
            Err(ureq::Error::Transport(error)) => {
                return Err(bad_gateway_response(&format!(
                    "failed to fetch remote URL: {error}"
                )));
            }
        }
    }

    Err(too_many_redirects_response(
        "remote URL exceeded the redirect limit",
    ))
}

fn next_redirect_url(
    current_url: &Url,
    response: &ureq::Response,
    redirect_index: usize,
) -> Result<String, HttpResponse> {
    if redirect_index == MAX_REMOTE_REDIRECTS {
        return Err(too_many_redirects_response(
            "remote URL exceeded the redirect limit",
        ));
    }

    let Some(location) = response.header("Location") else {
        return Err(bad_gateway_response(
            "remote redirect response is missing a Location header",
        ));
    };
    let next_url = current_url.join(location).map_err(|error| {
        bad_gateway_response(&format!(
            "remote redirect location could not be resolved: {error}"
        ))
    })?;

    Ok(next_url.to_string())
}

fn parse_remote_url(value: &str) -> Result<Url, HttpResponse> {
    Url::parse(value)
        .map_err(|error| bad_request_response(&format!("remote URL is invalid: {error}")))
}

fn validate_remote_url(url: &Url, config: &ServerConfig) -> Result<(), HttpResponse> {
    match url.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(bad_request_response(
                "remote URL must use the http or https scheme",
            ));
        }
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad_request_response(
            "remote URL must not embed user information",
        ));
    }

    let Some(host) = url.host_str() else {
        return Err(bad_request_response("remote URL must include a host"));
    };
    let Some(port) = url.port_or_known_default() else {
        return Err(bad_request_response(
            "remote URL must resolve to a known port",
        ));
    };

    if !config.allow_insecure_url_sources && port != 80 && port != 443 {
        return Err(forbidden_response(
            "remote URL port is not allowed by the current server policy",
        ));
    }

    let addrs = url.socket_addrs(|| None).map_err(|error| {
        bad_gateway_response(&format!("failed to resolve remote host `{host}`: {error}"))
    })?;
    if addrs.is_empty() {
        return Err(bad_gateway_response(&format!(
            "failed to resolve remote host `{host}`"
        )));
    }

    if !config.allow_insecure_url_sources
        && addrs
            .iter()
            .map(|addr| addr.ip())
            .any(is_disallowed_remote_ip)
    {
        return Err(forbidden_response(
            "remote URL resolves to a disallowed IP range",
        ));
    }

    Ok(())
}

fn read_remote_response_body(url: &str, response: ureq::Response) -> Result<Vec<u8>, HttpResponse> {
    validate_remote_content_encoding(&response)?;

    if let Some(content_length) = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
    {
        if content_length > MAX_SOURCE_BYTES {
            return Err(payload_too_large_response(&format!(
                "remote response exceeds {MAX_SOURCE_BYTES} bytes"
            )));
        }
    }

    let mut reader = response.into_reader().take(MAX_SOURCE_BYTES + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|error| {
        bad_gateway_response(&format!("failed to read remote URL `{url}`: {error}"))
    })?;

    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(payload_too_large_response(&format!(
            "remote response exceeds {MAX_SOURCE_BYTES} bytes"
        )));
    }

    Ok(bytes)
}

fn validate_remote_content_encoding(response: &ureq::Response) -> Result<(), HttpResponse> {
    let Some(content_encoding) = response.header("Content-Encoding") else {
        return Ok(());
    };

    for encoding in content_encoding
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !matches!(encoding, "gzip" | "br" | "identity") {
            return Err(bad_gateway_response(&format!(
                "remote response uses unsupported content-encoding `{encoding}`"
            )));
        }
    }

    Ok(())
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn is_disallowed_remote_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_disallowed_ipv4(ip),
        IpAddr::V6(ip) => is_disallowed_ipv6(ip),
    }
}

fn is_disallowed_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        || (octets[0] & 0b1111_0000) == 240
}

fn is_disallowed_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

fn validate_public_base_url(value: String) -> io::Result<String> {
    let parsed = Url::parse(&value).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("TRUSS_PUBLIC_BASE_URL must be a valid URL: {error}"),
        )
    })?;

    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TRUSS_PUBLIC_BASE_URL must use http or https",
        )),
    }
}

fn resolve_storage_path(storage_root: &Path, source_path: &str) -> Result<PathBuf, HttpResponse> {
    let trimmed = source_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(bad_request_response("source path must not be empty"));
    }

    let mut relative_path = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(segment) => relative_path.push(segment),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(bad_request_response(
                    "source path must not contain root, current-directory, or parent-directory segments",
                ));
            }
        }
    }

    if relative_path.as_os_str().is_empty() {
        return Err(bad_request_response("source path must not be empty"));
    }

    let canonical_root = storage_root.canonicalize().map_err(|error| {
        internal_error_response(&format!("failed to resolve storage root: {error}"))
    })?;
    let candidate = storage_root.join(relative_path);
    let canonical_candidate = candidate.canonicalize().map_err(map_source_io_error)?;

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(bad_request_response("source path escapes the storage root"));
    }

    Ok(canonical_candidate)
}

fn read_request<R>(stream: &mut R) -> Result<HttpRequest, HttpResponse>
where
    R: Read,
{
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).map_err(|error| {
            internal_error_response(&format!("failed to read request: {error}"))
        })?;
        if read == 0 {
            return Err(bad_request_response(
                "request ended before the HTTP headers were complete",
            ));
        }

        buffer.extend_from_slice(&chunk[..read]);

        if buffer.len() > MAX_HEADER_BYTES + MAX_REQUEST_BODY_BYTES {
            return Err(payload_too_large_response("request is too large"));
        }

        if let Some(index) = find_header_terminator(&buffer) {
            break index;
        }

        if buffer.len() > MAX_HEADER_BYTES {
            return Err(payload_too_large_response("request headers are too large"));
        }
    };

    let header_text = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| bad_request_response("request headers must be valid UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let (method, target, version) = parse_request_line(request_line)?;
    let headers = parse_headers(lines)?;
    let content_length = parse_content_length(&headers)?;
    if content_length > MAX_REQUEST_BODY_BYTES {
        return Err(payload_too_large_response("request body is too large"));
    }

    let mut body = buffer[(header_end + 4)..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).map_err(|error| {
            internal_error_response(&format!("failed to read request: {error}"))
        })?;
        if read == 0 {
            return Err(bad_request_response("request body was truncated"));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        target,
        version,
        headers,
        body,
    })
}

fn parse_request_line(request_line: &str) -> Result<(String, String, String), HttpResponse> {
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| bad_request_response("request line is missing an HTTP method"))?;
    let target = parts
        .next()
        .ok_or_else(|| bad_request_response("request line is missing a target path"))?;
    let version = parts
        .next()
        .ok_or_else(|| bad_request_response("request line is missing an HTTP version"))?;

    if parts.next().is_some() {
        return Err(bad_request_response("request line has too many fields"));
    }

    Ok((method.to_string(), target.to_string(), version.to_string()))
}

fn parse_headers<'a, I>(lines: I) -> Result<Vec<(String, String)>, HttpResponse>
where
    I: Iterator<Item = &'a str>,
{
    let mut headers = Vec::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }

        let Some((name, value)) = line.split_once(':') else {
            return Err(bad_request_response(
                "request headers must use `name: value` syntax",
            ));
        };

        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    Ok(headers)
}

fn parse_content_length(headers: &[(String, String)]) -> Result<usize, HttpResponse> {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name == "content-length")
        .map(|(_, value)| value.as_str());
    let Some(value) = values.next() else {
        return Ok(0);
    };

    if values.next().is_some() {
        return Err(bad_request_response(
            "duplicate content-length headers are not supported",
        ));
    }

    value
        .parse::<usize>()
        .map_err(|_| bad_request_response("content-length must be a non-negative integer"))
}

fn request_has_json_content_type(request: &HttpRequest) -> bool {
    request
        .header("content-type")
        .is_some_and(|value| content_type_matches(value, "application/json"))
}

fn transform_error_response(error: TransformError) -> HttpResponse {
    match error {
        TransformError::InvalidInput(reason)
        | TransformError::InvalidOptions(reason)
        | TransformError::DecodeFailed(reason) => bad_request_response(&reason),
        TransformError::UnsupportedInputMediaType(reason) => {
            unsupported_media_type_response(&reason)
        }
        TransformError::UnsupportedOutputMediaType(media_type) => unsupported_media_type_response(
            &format!("output format `{}` is not supported", media_type.as_name()),
        ),
        TransformError::EncodeFailed(reason) => {
            internal_error_response(&format!("failed to encode transformed artifact: {reason}"))
        }
        TransformError::CapabilityMissing(reason) => not_implemented_response(&reason),
    }
}

fn map_source_io_error(error: io::Error) -> HttpResponse {
    match error.kind() {
        io::ErrorKind::NotFound => not_found_response("source artifact was not found"),
        _ => internal_error_response(&format!("failed to access source artifact: {error}")),
    }
}

fn parse_optional_named<T, F>(
    value: Option<&str>,
    field_name: &str,
    parser: F,
) -> Result<Option<T>, HttpResponse>
where
    F: Fn(&str) -> Result<T, String>,
{
    match value {
        Some(value) => parse_named(value, field_name, parser).map(Some),
        None => Ok(None),
    }
}

fn parse_named<T, F>(value: &str, field_name: &str, parser: F) -> Result<T, HttpResponse>
where
    F: Fn(&str) -> Result<T, String>,
{
    parser(value).map_err(|reason| bad_request_response(&format!("{field_name}: {reason}")))
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find_map(|(header_name, value)| (header_name == name).then_some(value.as_str()))
}

fn content_type_matches(value: &str, expected: &str) -> bool {
    value
        .split(';')
        .next()
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_header_terminator(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn write_response(stream: &mut TcpStream, response: HttpResponse) -> io::Result<()> {
    let mut header = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.body.len()
    );

    if let Some(content_type) = response.content_type {
        header.push_str(&format!("Content-Type: {content_type}\r\n"));
    }

    for (name, value) in response.headers {
        header.push_str(&format!("{name}: {value}\r\n"));
    }

    header.push_str("\r\n");

    stream.write_all(header.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn bad_request_response(message: &str) -> HttpResponse {
    error_response("400 Bad Request", message)
}

fn auth_required_response(message: &str) -> HttpResponse {
    HttpResponse::json_with_headers(
        "401 Unauthorized",
        vec![("WWW-Authenticate".to_string(), "Bearer".to_string())],
        json_error_body(message),
    )
}

fn not_found_response(message: &str) -> HttpResponse {
    error_response("404 Not Found", message)
}

fn forbidden_response(message: &str) -> HttpResponse {
    error_response("403 Forbidden", message)
}

fn unsupported_media_type_response(message: &str) -> HttpResponse {
    error_response("415 Unsupported Media Type", message)
}

fn payload_too_large_response(message: &str) -> HttpResponse {
    error_response("413 Payload Too Large", message)
}

fn internal_error_response(message: &str) -> HttpResponse {
    error_response("500 Internal Server Error", message)
}

fn bad_gateway_response(message: &str) -> HttpResponse {
    error_response("502 Bad Gateway", message)
}

fn service_unavailable_response(message: &str) -> HttpResponse {
    error_response("503 Service Unavailable", message)
}

fn too_many_redirects_response(message: &str) -> HttpResponse {
    error_response("508 Loop Detected", message)
}

fn not_implemented_response(message: &str) -> HttpResponse {
    error_response("501 Not Implemented", message)
}

fn error_response(status: &'static str, message: &str) -> HttpResponse {
    HttpResponse::json(status, json_error_body(message))
}

fn json_error_body(message: &str) -> Vec<u8> {
    let mut body =
        serde_json::to_vec(&json!({ "error": message })).expect("serialize JSON error body");
    body.push(b'\n');
    body
}

#[cfg(test)]
mod tests {
    use super::{
        auth_required_response, bad_request_response, bind_addr, read_request,
        resolve_storage_path, route_request, serve_once_with_config, HttpRequest, ServerConfig,
        DEFAULT_BIND_ADDR,
    };
    use crate::{sniff_artifact, MediaType, RawArtifact};
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn png_bytes() -> Vec<u8> {
        let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 4, 3, ColorType::Rgba8.into())
            .expect("encode png");
        bytes
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("truss-server-{name}-{unique}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_png(path: &Path) {
        fs::write(path, png_bytes()).expect("write png fixture");
    }

    fn spawn_http_server(
        responses: Vec<(String, Vec<(String, String)>, Vec<u8>)>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        listener
            .set_nonblocking(true)
            .expect("configure fixture server");
        let addr = listener.local_addr().expect("fixture server addr");
        let url = format!("http://{addr}/image");

        let handle = thread::spawn(move || {
            for (status, headers, body) in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(2);
                let mut accepted = None;
                while std::time::Instant::now() < deadline {
                    match listener.accept() {
                        Ok(stream) => {
                            accepted = Some(stream);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept fixture request: {error}"),
                    }
                }

                let Some((mut stream, _)) = accepted else {
                    break;
                };
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let mut header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    header.push_str(&format!("{name}: {value}\r\n"));
                }
                header.push_str("\r\n");
                stream
                    .write_all(header.as_bytes())
                    .expect("write fixture headers");
                stream.write_all(&body).expect("write fixture body");
                stream.flush().expect("flush fixture response");
            }
        });

        (url, handle)
    }

    fn transform_request(path: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            target: "/images:transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: format!(
                "{{\"source\":{{\"kind\":\"path\",\"path\":\"{path}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
            )
            .into_bytes(),
        }
    }

    fn transform_url_request(url: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            target: "/images:transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: format!(
                "{{\"source\":{{\"kind\":\"url\",\"url\":\"{url}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
            )
            .into_bytes(),
        }
    }

    fn upload_request(file_bytes: &[u8], options_json: Option<&str>) -> HttpRequest {
        let boundary = "truss-test-boundary";
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(b"\r\n");

        if let Some(options_json) = options_json {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{options_json}\r\n"
                )
                .as_bytes(),
            );
        }

        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                (
                    "content-type".to_string(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ],
            body,
        }
    }

    fn metrics_request(with_auth: bool) -> HttpRequest {
        let mut headers = Vec::new();
        if with_auth {
            headers.push(("authorization".to_string(), "Bearer secret".to_string()));
        }

        HttpRequest {
            method: "GET".to_string(),
            target: "/metrics".to_string(),
            version: "HTTP/1.1".to_string(),
            headers,
            body: Vec::new(),
        }
    }

    fn response_body(response: &super::HttpResponse) -> String {
        String::from_utf8(response.body.clone()).expect("utf8 response body")
    }

    #[test]
    fn uses_default_bind_addr_when_env_is_missing() {
        std::env::remove_var("TRUSS_BIND_ADDR");
        assert_eq!(bind_addr(), DEFAULT_BIND_ADDR);
    }

    #[test]
    fn health_endpoints_return_ok() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(temp_dir("health"), None));

        assert_eq!(response.status, "200 OK");
        assert_eq!(response_body(&response), "{\"status\":\"ok\"}\n");
    }

    #[test]
    fn unknown_path_returns_not_found() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/unknown".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(temp_dir("not-found"), None));

        assert_eq!(response.status, "404 Not Found");
        assert_eq!(response_body(&response), "{\"error\":\"not found\"}\n");
    }

    #[test]
    fn transform_endpoint_requires_authentication() {
        let storage_root = temp_dir("auth");
        write_png(&storage_root.join("image.png"));
        let mut request = transform_request("/image.png");
        request.headers.retain(|(name, _)| name != "authorization");

        let response = route_request(
            request,
            &ServerConfig::new(storage_root, Some("secret".to_string())),
        );

        assert_eq!(response.status, "401 Unauthorized");
        assert!(response_body(&response).contains("authorization required"));
    }

    #[test]
    fn transform_endpoint_returns_service_unavailable_without_configured_token() {
        let storage_root = temp_dir("token");
        write_png(&storage_root.join("image.png"));

        let response = route_request(
            transform_request("/image.png"),
            &ServerConfig::new(storage_root, None),
        );

        assert_eq!(response.status, "503 Service Unavailable");
        assert!(response_body(&response).contains("bearer token is not configured"));
    }

    #[test]
    fn transform_endpoint_transforms_a_path_source() {
        let storage_root = temp_dir("transform");
        write_png(&storage_root.join("image.png"));

        let response = route_request(
            transform_request("/image.png"),
            &ServerConfig::new(storage_root, Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type.as_deref(), Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
        assert_eq!(artifact.metadata.width, Some(4));
        assert_eq!(artifact.metadata.height, Some(3));
    }

    #[test]
    fn transform_endpoint_rejects_private_url_sources_by_default() {
        let response = route_request(
            transform_url_request("http://127.0.0.1:8080/image.png"),
            &ServerConfig::new(temp_dir("url-blocked"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "403 Forbidden");
        assert!(response_body(&response).contains("port is not allowed"));
    }

    #[test]
    fn transform_endpoint_transforms_a_url_source_when_insecure_allowance_is_enabled() {
        let (url, handle) = spawn_http_server(vec![(
            "200 OK".to_string(),
            vec![("Content-Type".to_string(), "image/png".to_string())],
            png_bytes(),
        )]);

        let response = route_request(
            transform_url_request(&url),
            &ServerConfig::new(temp_dir("url"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type.as_deref(), Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn transform_endpoint_follows_remote_redirects() {
        let (redirect_url, handle) = spawn_http_server(vec![
            (
                "302 Found".to_string(),
                vec![("Location".to_string(), "/final-image".to_string())],
                Vec::new(),
            ),
            (
                "200 OK".to_string(),
                vec![("Content-Type".to_string(), "image/png".to_string())],
                png_bytes(),
            ),
        ]);

        let response = route_request(
            transform_url_request(&redirect_url),
            &ServerConfig::new(temp_dir("redirect"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "200 OK");
        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn upload_endpoint_transforms_uploaded_file() {
        let response = route_request(
            upload_request(&png_bytes(), Some(r#"{"format":"jpeg"}"#)),
            &ServerConfig::new(temp_dir("upload"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type.as_deref(), Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn upload_endpoint_requires_a_file_field() {
        let boundary = "truss-test-boundary";
        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                (
                    "content-type".to_string(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ],
            body: format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"jpeg\"}}\r\n--{boundary}--\r\n"
            )
            .into_bytes(),
        };

        let response = route_request(
            request,
            &ServerConfig::new(temp_dir("upload-missing-file"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("requires a `file` field"));
    }

    #[test]
    fn upload_endpoint_rejects_non_multipart_content_type() {
        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: br#"{"file":"not-really-json"}"#.to_vec(),
        };

        let response = route_request(
            request,
            &ServerConfig::new(temp_dir("upload-content-type"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "415 Unsupported Media Type");
        assert!(response_body(&response).contains("multipart/form-data"));
    }

    #[test]
    fn parse_upload_request_extracts_file_and_options() {
        let request = upload_request(&png_bytes(), Some(r#"{"width":8,"format":"jpeg"}"#));
        let boundary = super::parse_multipart_boundary(&request).expect("parse boundary");
        let (file_bytes, options) =
            super::parse_upload_request(&request.body, &boundary).expect("parse upload body");

        assert_eq!(file_bytes, png_bytes());
        assert_eq!(options.width, Some(8));
        assert_eq!(options.format, Some(MediaType::Jpeg));
    }

    #[test]
    fn metrics_endpoint_requires_authentication() {
        let response = route_request(
            metrics_request(false),
            &ServerConfig::new(temp_dir("metrics-auth"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "401 Unauthorized");
        assert!(response_body(&response).contains("authorization required"));
    }

    #[test]
    fn metrics_endpoint_returns_prometheus_text() {
        super::record_http_metrics(super::RouteMetric::Health, "200 OK");
        let response = route_request(
            metrics_request(true),
            &ServerConfig::new(temp_dir("metrics"), Some("secret".to_string())),
        );
        let body = response_body(&response);

        assert_eq!(response.status, "200 OK");
        assert_eq!(
            response.content_type.as_deref(),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        assert!(body.contains("truss_http_requests_total"));
        assert!(body.contains("truss_http_requests_by_route_total{route=\"/health\"}"));
        assert!(body.contains("truss_http_responses_total{status=\"200\"}"));
    }

    #[test]
    fn transform_endpoint_rejects_unsupported_remote_content_encoding() {
        let (url, handle) = spawn_http_server(vec![(
            "200 OK".to_string(),
            vec![
                ("Content-Type".to_string(), "image/png".to_string()),
                ("Content-Encoding".to_string(), "compress".to_string()),
            ],
            png_bytes(),
        )]);

        let response = route_request(
            transform_url_request(&url),
            &ServerConfig::new(temp_dir("encoding"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "502 Bad Gateway");
        assert!(response_body(&response).contains("unsupported content-encoding"));
    }

    #[test]
    fn resolve_storage_path_rejects_parent_segments() {
        let storage_root = temp_dir("resolve");
        let response = resolve_storage_path(&storage_root, "../escape.png")
            .expect_err("parent segments should be rejected");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("must not contain root"));
    }

    #[test]
    fn read_request_parses_headers_and_body() {
        let request_bytes = b"POST /images:transform HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(request_bytes);
        let request = read_request(&mut cursor).expect("parse request");

        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/images:transform");
        assert_eq!(request.version, "HTTP/1.1");
        assert_eq!(request.header("host"), Some("localhost"));
        assert_eq!(request.body, b"{}");
    }

    #[test]
    fn read_request_rejects_duplicate_content_length() {
        let request_bytes =
            b"POST /images:transform HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(request_bytes);
        let response = read_request(&mut cursor).expect_err("duplicate headers should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("duplicate content-length"));
    }

    #[test]
    fn serve_once_handles_a_tcp_request() {
        let storage_root = temp_dir("serve-once");
        let config = ServerConfig::new(storage_root, None);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("read local addr");

        let server = thread::spawn(move || serve_once_with_config(listener, &config));

        let mut stream = TcpStream::connect(addr).expect("connect to test server");
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .expect("write request");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");

        server
            .join()
            .expect("join test server thread")
            .expect("serve one request");

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.ends_with("{\"status\":\"ok\"}\n"));
    }

    #[test]
    fn helper_error_responses_include_json_bodies() {
        let response = auth_required_response("authorization required");
        let bad_request = bad_request_response("bad input");

        assert!(response_body(&response).contains("authorization required"));
        assert!(response_body(&bad_request).contains("bad input"));
    }
}
