use crate::{
    sniff_artifact, transform_raster, Fit, MediaType, Position, RawArtifact, Rgba8, Rotation,
    TransformError, TransformOptions, TransformRequest,
};
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

/// The default bind address for the development HTTP server.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

/// The default storage root used by the server adapter.
pub const DEFAULT_STORAGE_ROOT: &str = ".";

const HEALTH_BODY: &str = "{\"status\":\"ok\"}\n";
const NOT_FOUND_BODY: &str = "{\"error\":\"not found\"}\n";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 100 * 1024 * 1024;

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
        }
    }

    /// Loads server configuration from environment variables.
    ///
    /// The adapter currently reads:
    ///
    /// - `TRUSS_STORAGE_ROOT`: filesystem root for `source.kind=path` inputs. Defaults to the
    ///   current directory and is canonicalized before use.
    /// - `TRUSS_BEARER_TOKEN`: private API Bearer token. When this value is missing, private
    ///   endpoints remain unavailable and return `503 Service Unavailable`.
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

        Ok(Self {
            storage_root,
            bearer_token,
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

fn handle_stream(mut stream: TcpStream, config: &ServerConfig) -> io::Result<()> {
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(response) => return write_response(&mut stream, response),
    };
    let response = route_request(request, config);

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
        _ => HttpResponse::json("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec()),
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
        TransformSourcePayload::Url { .. } => Err(not_implemented_response(
            "url sources are not implemented yet",
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
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("application/json"))
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

fn unsupported_media_type_response(message: &str) -> HttpResponse {
    error_response("415 Unsupported Media Type", message)
}

fn payload_too_large_response(message: &str) -> HttpResponse {
    error_response("413 Payload Too Large", message)
}

fn internal_error_response(message: &str) -> HttpResponse {
    error_response("500 Internal Server Error", message)
}

fn service_unavailable_response(message: &str) -> HttpResponse {
    error_response("503 Service Unavailable", message)
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
    use std::time::{SystemTime, UNIX_EPOCH};

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
    fn transform_endpoint_reports_unimplemented_url_sources() {
        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/images:transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: br#"{"source":{"kind":"url","url":"https://example.invalid/image.png"}}"#
                .to_vec(),
        };

        let response = route_request(
            request,
            &ServerConfig::new(temp_dir("url"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "501 Not Implemented");
        assert!(response_body(&response).contains("url sources are not implemented yet"));
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
