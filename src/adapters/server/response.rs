use crate::TransformError;
use serde_json::json;
use std::io::{self, Write};
use std::net::TcpStream;

pub(super) const NOT_FOUND_BODY: &str =
    "{\"type\":\"about:blank\",\"title\":\"Not Found\",\"status\":404,\"detail\":\"not found\"}\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HttpResponse {
    pub(super) status: &'static str,
    pub(super) content_type: Option<&'static str>,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl HttpResponse {
    pub(super) fn json(status: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some("application/json"),
            headers: Vec::new(),
            body,
        }
    }

    /// Clears the response body when `is_head` is true, ensuring HEAD
    /// requests never carry a body per RFC 9110.
    pub(super) fn strip_body_if_head(&mut self, is_head: bool) {
        if is_head {
            self.body = Vec::new();
        }
    }

    /// Records the request id on the response: as the `X-Request-Id` header and, for a
    /// problem body, as its `requestId` member too, so a body that is logged or forwarded on
    /// its own still matches the server's access log line. The member is RFC 9457's
    /// extension mechanism; `instance` is not used because a client-supplied id need not be
    /// a URI.
    pub(super) fn attach_request_id(&mut self, request_id: &str) {
        self.headers
            .push(("X-Request-Id".to_string(), request_id.to_string()));
        if self.content_type != Some("application/problem+json") {
            return;
        }
        if let Ok(serde_json::Value::Object(mut problem)) =
            serde_json::from_slice::<serde_json::Value>(&self.body)
        {
            problem.insert(
                "requestId".to_string(),
                serde_json::Value::String(request_id.to_string()),
            );
            let mut body = serde_json::to_vec(&problem).expect("serialize problem body");
            body.push(b'\n');
            self.body = body;
        }
    }

    pub(super) fn problem(status: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some("application/problem+json"),
            headers: Vec::new(),
            body,
        }
    }

    pub(super) fn problem_with_headers(
        status: &'static str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            content_type: Some("application/problem+json"),
            headers,
            body,
        }
    }

    pub(super) fn binary_with_headers(
        status: &'static str,
        content_type: &'static str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            content_type: Some(content_type),
            headers,
            body,
        }
    }

    pub(super) fn text(status: &'static str, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some(content_type),
            headers: Vec::new(),
            body,
        }
    }

    pub(super) fn empty(status: &'static str, headers: Vec<(String, String)>) -> Self {
        Self {
            status,
            content_type: None,
            headers,
            body: Vec::new(),
        }
    }
}

/// Minimum body size (in bytes) below which gzip compression is skipped.
/// Very small bodies may actually grow when compressed due to gzip framing overhead.
const MIN_COMPRESS_BYTES: usize = 128;

/// Content types eligible for gzip compression.  Image types are excluded
/// because they are already compressed (JPEG, PNG, WebP, AVIF, etc.).
///
/// **Security note (BREACH):** If a future endpoint returns compressed
/// responses that mix attacker-controlled input with secret tokens, it may
/// be vulnerable to BREACH-style compression side-channel attacks. The
/// current endpoints (health, metrics, image transforms) do not include
/// secrets in the response body, so the risk is low today.
fn is_compressible_content_type(ct: &str) -> bool {
    let media_type = ct.split(';').next().unwrap_or("").trim();
    matches!(
        media_type,
        "application/json"
            | "application/problem+json"
            | "text/plain"
            | "application/openmetrics-text"
    )
}

pub(super) fn write_response(
    stream: &mut TcpStream,
    response: HttpResponse,
    close: bool,
) -> io::Result<()> {
    write_response_compressed(stream, response, close, false, 1)
}

pub(super) fn write_response_compressed(
    stream: &mut TcpStream,
    response: HttpResponse,
    close: bool,
    accepts_gzip: bool,
    compression_level: u32,
) -> io::Result<()> {
    use std::fmt::Write as FmtWrite;

    let should_compress = accepts_gzip
        && response.body.len() >= MIN_COMPRESS_BYTES
        && response
            .content_type
            .is_some_and(is_compressible_content_type);

    let (body, is_compressed) = if should_compress {
        match gzip_compress(&response.body, compression_level) {
            Ok(compressed) if compressed.len() < response.body.len() => (compressed, true),
            _ => (response.body, false),
        }
    } else {
        (response.body, false)
    };

    let connection_value = if close { "close" } else { "keep-alive" };
    let mut header = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: {connection_value}\r\n",
        response.status,
        body.len()
    );

    if let Some(content_type) = response.content_type {
        let _ = write!(header, "Content-Type: {content_type}\r\n");
    }

    if is_compressed {
        header.push_str("Content-Encoding: gzip\r\n");
    }

    // Collect Vary directives from response headers and compression, then emit
    // a single combined Vary header to avoid duplicate Vary lines.
    let mut vary_parts: Vec<&str> = Vec::new();
    if accepts_gzip
        && response
            .content_type
            .is_some_and(is_compressible_content_type)
    {
        vary_parts.push("Accept-Encoding");
    }
    for (name, value) in &response.headers {
        if name.eq_ignore_ascii_case("Vary") {
            for part in value.split(',') {
                let trimmed = part.trim();
                if !trimmed.is_empty()
                    && !vary_parts.iter().any(|v| v.eq_ignore_ascii_case(trimmed))
                {
                    vary_parts.push(trimmed);
                }
            }
        }
    }
    if !vary_parts.is_empty() {
        let _ = write!(header, "Vary: {}\r\n", vary_parts.join(", "));
    }

    for (name, value) in response.headers {
        if !name.eq_ignore_ascii_case("Vary") {
            let _ = write!(header, "{name}: {value}\r\n");
        }
    }

    header.push_str("\r\n");

    stream.write_all(header.as_bytes())?;
    stream.write_all(&body)?;
    stream.flush()
}

fn gzip_compress(data: &[u8], level: u32) -> io::Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
    encoder.write_all(data)?;
    encoder.finish()
}

/// The response header that carries a transform warning, one header per warning.
///
/// It is the same text the CLI prints after `warning:` and the Wasm package returns in
/// `warnings`. The HTTP `Warning` field is deprecated by RFC 9111, and RFC 6648 retired the
/// `X-` prefix for new fields, which is how the name came to be this one.
pub(super) const WARNING_HEADER: &str = "Truss-Warning";

/// A warning's text as a header value: one line of visible ASCII.
///
/// A header cannot carry a line break, and the cache entry keeps the text on its first
/// line with a tab between warnings, so a tab, a carriage return, and a newline become a
/// space, and anything outside visible ASCII becomes `?`. The warnings truss raises are
/// ASCII already; this is what keeps a future one from breaking the framing.
pub(super) fn warning_header_value(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\t' | '\r' | '\n' => ' ',
            ' '..='~' => c,
            _ => '?',
        })
        .collect()
}

/// Appends one [`WARNING_HEADER`] per warning.
pub(super) fn push_warning_headers(headers: &mut Vec<(String, String)>, warnings: &[String]) {
    for warning in warnings {
        headers.push((WARNING_HEADER.to_string(), warning_header_value(warning)));
    }
}

/// Where the problem types are described. Each [`ProblemType`] is an anchor on this page.
pub(super) const PROBLEM_TYPES_URL: &str =
    "https://github.com/nao1215/truss/blob/main/docs/problems.md";

/// A class of failure, the way RFC 9457 names one: `type` is a URI identifying the class,
/// `title` is its fixed short name, and `detail` describes the one occurrence.
///
/// The transform classes are the `TransformError` variants under the names the Wasm package
/// reports as `kind`, so a client sees one classification whichever adapter it talks to; the
/// rest are the server's own. A variant added here needs its row in `docs/problems.md`, which
/// is what the `type` URI resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProblemType {
    /// The request itself could not be understood: a malformed body, query, or header.
    InvalidRequest,
    InvalidOptions,
    InvalidInput,
    DecodeFailed,
    /// The request's own media type, as opposed to the image's: a body that is not JSON
    /// where JSON is required, or a multipart part of the wrong kind.
    UnsupportedMediaType,
    UnsupportedInputMediaType,
    UnsupportedOutputMediaType,
    EncodeFailed,
    CapabilityMissing,
    LimitExceeded,
    Unauthorized,
    Forbidden,
    NotFound,
    NotAcceptable,
    RequestTimeout,
    PayloadTooLarge,
    UnprocessableEntity,
    TooManyRequests,
    InternalError,
    NotImplemented,
    BadGateway,
    ServiceUnavailable,
    LoopDetected,
}

impl ProblemType {
    /// The anchor on the problem types page, which is also the class's name in the docs.
    pub(super) const fn slug(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid-request",
            Self::InvalidOptions => "invalid-options",
            Self::InvalidInput => "invalid-input",
            Self::DecodeFailed => "decode-failed",
            Self::UnsupportedMediaType => "unsupported-media-type",
            Self::UnsupportedInputMediaType => "unsupported-input-media-type",
            Self::UnsupportedOutputMediaType => "unsupported-output-media-type",
            Self::EncodeFailed => "encode-failed",
            Self::CapabilityMissing => "capability-missing",
            Self::LimitExceeded => "limit-exceeded",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not-found",
            Self::NotAcceptable => "not-acceptable",
            Self::RequestTimeout => "request-timeout",
            Self::PayloadTooLarge => "payload-too-large",
            Self::UnprocessableEntity => "unprocessable-entity",
            Self::TooManyRequests => "too-many-requests",
            Self::InternalError => "internal-error",
            Self::NotImplemented => "not-implemented",
            Self::BadGateway => "bad-gateway",
            Self::ServiceUnavailable => "service-unavailable",
            Self::LoopDetected => "loop-detected",
        }
    }

    /// The fixed `title` of the class. RFC 9457 asks that it not change from one occurrence
    /// to the next, so nothing about the request goes in here; that is what `detail` is for.
    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::InvalidRequest => "Invalid request",
            Self::InvalidOptions => "Invalid transform options",
            Self::InvalidInput => "Invalid input",
            Self::DecodeFailed => "Input could not be decoded",
            Self::UnsupportedMediaType => "Unsupported Media Type",
            Self::UnsupportedInputMediaType => "Unsupported input media type",
            Self::UnsupportedOutputMediaType => "Unsupported output media type",
            Self::EncodeFailed => "Output could not be encoded",
            Self::CapabilityMissing => "Capability not available",
            Self::LimitExceeded => "Limit exceeded",
            Self::Unauthorized => "Unauthorized",
            Self::Forbidden => "Forbidden",
            Self::NotFound => "Not Found",
            Self::NotAcceptable => "Not Acceptable",
            Self::RequestTimeout => "Request Timeout",
            Self::PayloadTooLarge => "Payload Too Large",
            Self::UnprocessableEntity => "Unprocessable Entity",
            Self::TooManyRequests => "Too Many Requests",
            Self::InternalError => "Internal Server Error",
            Self::NotImplemented => "Not Implemented",
            Self::BadGateway => "Bad Gateway",
            Self::ServiceUnavailable => "Service Unavailable",
            Self::LoopDetected => "Loop Detected",
        }
    }

    /// The `type` URI: the problem types page, at this class's anchor.
    pub(super) fn uri(self) -> String {
        format!("{PROBLEM_TYPES_URL}#{}", self.slug())
    }
}

pub(super) fn bad_request_response(message: &str) -> HttpResponse {
    problem_response("400 Bad Request", 400, ProblemType::InvalidRequest, message)
}

pub(super) fn auth_required_response(message: &str) -> HttpResponse {
    HttpResponse::problem_with_headers(
        "401 Unauthorized",
        vec![("WWW-Authenticate".to_string(), "Bearer".to_string())],
        problem_detail_body(ProblemType::Unauthorized, 401, message),
    )
}

pub(super) fn signed_url_unauthorized_response(message: &str) -> HttpResponse {
    problem_response("401 Unauthorized", 401, ProblemType::Unauthorized, message)
}

pub(super) fn not_found_response(message: &str) -> HttpResponse {
    problem_response("404 Not Found", 404, ProblemType::NotFound, message)
}

pub(super) fn forbidden_response(message: &str) -> HttpResponse {
    problem_response("403 Forbidden", 403, ProblemType::Forbidden, message)
}

pub(super) fn unsupported_media_type_response(message: &str) -> HttpResponse {
    problem_response(
        "415 Unsupported Media Type",
        415,
        ProblemType::UnsupportedMediaType,
        message,
    )
}

pub(super) fn not_acceptable_response(message: &str) -> HttpResponse {
    problem_response(
        "406 Not Acceptable",
        406,
        ProblemType::NotAcceptable,
        message,
    )
}

pub(super) fn unprocessable_entity_response(message: &str) -> HttpResponse {
    problem_response(
        "422 Unprocessable Entity",
        422,
        ProblemType::UnprocessableEntity,
        message,
    )
}

pub(super) fn payload_too_large_response(message: &str) -> HttpResponse {
    problem_response(
        "413 Payload Too Large",
        413,
        ProblemType::PayloadTooLarge,
        message,
    )
}

pub(super) fn request_timeout_response(message: &str) -> HttpResponse {
    problem_response(
        "408 Request Timeout",
        408,
        ProblemType::RequestTimeout,
        message,
    )
}

pub(super) fn internal_error_response(message: &str) -> HttpResponse {
    problem_response(
        "500 Internal Server Error",
        500,
        ProblemType::InternalError,
        message,
    )
}

pub(super) fn bad_gateway_response(message: &str) -> HttpResponse {
    problem_response("502 Bad Gateway", 502, ProblemType::BadGateway, message)
}

pub(super) fn service_unavailable_response(message: &str) -> HttpResponse {
    problem_response(
        "503 Service Unavailable",
        503,
        ProblemType::ServiceUnavailable,
        message,
    )
}

pub(super) fn too_many_requests_response(message: &str) -> HttpResponse {
    let mut resp = problem_response(
        "429 Too Many Requests",
        429,
        ProblemType::TooManyRequests,
        message,
    );
    // RFC 6585 §4: include Retry-After so well-behaved clients back off.
    resp.headers
        .push(("Retry-After".to_string(), "1".to_string()));
    resp
}

pub(super) fn too_many_redirects_response(message: &str) -> HttpResponse {
    problem_response("508 Loop Detected", 508, ProblemType::LoopDetected, message)
}

pub(super) fn not_implemented_response(message: &str) -> HttpResponse {
    problem_response(
        "501 Not Implemented",
        501,
        ProblemType::NotImplemented,
        message,
    )
}

/// Builds an RFC 9457 Problem Details error response.
///
/// The body is `application/problem+json` with `type`, `title`, `status`, and `detail`, and
/// gains `requestId` when the response is sent, in [`HttpResponse::attach_request_id`]. The
/// one problem body that keeps `about:blank` as its `type` is [`NOT_FOUND_BODY`], for a route
/// that does not exist, where the status really is all there is to say.
pub(super) fn problem_response(
    status: &'static str,
    status_code: u16,
    kind: ProblemType,
    detail: &str,
) -> HttpResponse {
    HttpResponse::problem(status, problem_detail_body(kind, status_code, detail))
}

/// Serializes an RFC 9457 Problem Details JSON body.
pub(super) fn problem_detail_body(kind: ProblemType, status: u16, detail: &str) -> Vec<u8> {
    let mut body = serde_json::to_vec(&json!({
        "type": kind.uri(),
        "title": kind.title(),
        "status": status,
        "detail": detail,
    }))
    .expect("serialize problem detail body");
    body.push(b'\n');
    body
}

/// Maps a transform failure onto the status and problem type that name it.
///
/// The classes are the ones `@nao1215/truss-wasm` reports as `kind`, so the two adapters
/// classify a failure the same way; the statuses are unchanged from what each class always got.
pub(super) fn transform_error_response(error: TransformError) -> HttpResponse {
    match error {
        TransformError::InvalidOptions(reason) => {
            problem_response("400 Bad Request", 400, ProblemType::InvalidOptions, &reason)
        }
        TransformError::InvalidInput(reason) => {
            problem_response("400 Bad Request", 400, ProblemType::InvalidInput, &reason)
        }
        TransformError::DecodeFailed(reason) => {
            problem_response("400 Bad Request", 400, ProblemType::DecodeFailed, &reason)
        }
        TransformError::UnsupportedInputMediaType(reason) => problem_response(
            "415 Unsupported Media Type",
            415,
            ProblemType::UnsupportedInputMediaType,
            &reason,
        ),
        ref error @ TransformError::UnsupportedOutputMediaType(_) => problem_response(
            "415 Unsupported Media Type",
            415,
            ProblemType::UnsupportedOutputMediaType,
            &error.to_string(),
        ),
        TransformError::EncodeFailed(reason) => problem_response(
            "500 Internal Server Error",
            500,
            ProblemType::EncodeFailed,
            &format!("failed to encode transformed artifact: {reason}"),
        ),
        TransformError::CapabilityMissing(reason) => problem_response(
            "501 Not Implemented",
            501,
            ProblemType::CapabilityMissing,
            &reason,
        ),
        TransformError::LimitExceeded(reason) => problem_response(
            "413 Payload Too Large",
            413,
            ProblemType::LimitExceeded,
            &reason,
        ),
    }
}

pub(super) fn map_source_io_error(error: io::Error) -> HttpResponse {
    match error.kind() {
        io::ErrorKind::NotFound => not_found_response("source artifact was not found"),
        _ => internal_error_response(&format!("failed to access source artifact: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MediaType, TransformError};
    use serde_json::Value;

    /// Parse the body of an HttpResponse as JSON.
    fn parse_body(response: &HttpResponse) -> Value {
        serde_json::from_slice(&response.body).expect("body should be valid JSON")
    }

    // ---------------------------------------------------------------
    // problem_detail_body
    // ---------------------------------------------------------------

    #[test]
    fn test_problem_detail_body_contains_required_fields() {
        let body = problem_detail_body(ProblemType::NotFound, 404, "resource missing");
        let v: Value = serde_json::from_slice(&body).expect("valid JSON");

        assert_eq!(v["type"], format!("{PROBLEM_TYPES_URL}#not-found"));
        assert_eq!(v["title"], "Not Found");
        assert_eq!(v["status"], 404);
        assert_eq!(v["detail"], "resource missing");
    }

    #[test]
    fn test_problem_detail_body_ends_with_newline() {
        let body = problem_detail_body(ProblemType::InternalError, 500, "boom");
        assert_eq!(*body.last().unwrap(), b'\n');
    }

    #[test]
    fn test_problem_detail_body_special_characters_in_detail() {
        let body = problem_detail_body(
            ProblemType::InvalidRequest,
            400,
            "invalid <script>alert(1)</script>",
        );
        let v: Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(v["detail"], "invalid <script>alert(1)</script>");
    }

    // ---------------------------------------------------------------
    // bad_request_response
    // ---------------------------------------------------------------

    #[test]
    fn test_bad_request_response_status_and_content_type() {
        let resp = bad_request_response("missing parameter");
        assert_eq!(resp.status, "400 Bad Request");
        assert_eq!(resp.content_type, Some("application/problem+json"));

        let v = parse_body(&resp);
        assert_eq!(v["status"], 400);
        assert_eq!(v["title"], "Invalid request");
        assert_eq!(v["detail"], "missing parameter");
    }

    // ---------------------------------------------------------------
    // not_found_response
    // ---------------------------------------------------------------

    #[test]
    fn test_not_found_response_status_and_body() {
        let resp = not_found_response("image not found");
        assert_eq!(resp.status, "404 Not Found");
        assert_eq!(resp.content_type, Some("application/problem+json"));

        let v = parse_body(&resp);
        assert_eq!(v["status"], 404);
        assert_eq!(v["title"], "Not Found");
        assert_eq!(v["detail"], "image not found");
    }

    // ---------------------------------------------------------------
    // internal_error_response
    // ---------------------------------------------------------------

    #[test]
    fn test_internal_error_response_status_and_body() {
        let resp = internal_error_response("disk full");
        assert_eq!(resp.status, "500 Internal Server Error");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 500);
        assert_eq!(v["title"], "Internal Server Error");
        assert_eq!(v["detail"], "disk full");
    }

    // ---------------------------------------------------------------
    // forbidden_response
    // ---------------------------------------------------------------

    #[test]
    fn test_forbidden_response_status_and_body() {
        let resp = forbidden_response("access denied");
        assert_eq!(resp.status, "403 Forbidden");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 403);
        assert_eq!(v["title"], "Forbidden");
        assert_eq!(v["detail"], "access denied");
    }

    // ---------------------------------------------------------------
    // unsupported_media_type_response
    // ---------------------------------------------------------------

    #[test]
    fn test_unsupported_media_type_response() {
        let resp = unsupported_media_type_response("image/gif is not supported");
        assert_eq!(resp.status, "415 Unsupported Media Type");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 415);
        assert_eq!(v["title"], "Unsupported Media Type");
    }

    // ---------------------------------------------------------------
    // not_acceptable_response
    // ---------------------------------------------------------------

    #[test]
    fn test_not_acceptable_response() {
        let resp = not_acceptable_response("no acceptable format");
        assert_eq!(resp.status, "406 Not Acceptable");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 406);
        assert_eq!(v["title"], "Not Acceptable");
    }

    // ---------------------------------------------------------------
    // payload_too_large_response
    // ---------------------------------------------------------------

    #[test]
    fn test_payload_too_large_response() {
        let resp = payload_too_large_response("exceeds 10MB limit");
        assert_eq!(resp.status, "413 Payload Too Large");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 413);
        assert_eq!(v["title"], "Payload Too Large");
        assert_eq!(v["detail"], "exceeds 10MB limit");
    }

    // ---------------------------------------------------------------
    // bad_gateway_response
    // ---------------------------------------------------------------

    #[test]
    fn test_bad_gateway_response() {
        let resp = bad_gateway_response("upstream error");
        assert_eq!(resp.status, "502 Bad Gateway");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 502);
        assert_eq!(v["title"], "Bad Gateway");
    }

    // ---------------------------------------------------------------
    // service_unavailable_response
    // ---------------------------------------------------------------

    #[test]
    fn test_service_unavailable_response() {
        let resp = service_unavailable_response("overloaded");
        assert_eq!(resp.status, "503 Service Unavailable");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 503);
        assert_eq!(v["title"], "Service Unavailable");
    }

    // ---------------------------------------------------------------
    // too_many_requests_response
    // ---------------------------------------------------------------

    #[test]
    fn test_too_many_requests_response_includes_retry_after() {
        let resp = too_many_requests_response("rate limit exceeded");
        assert_eq!(resp.status, "429 Too Many Requests");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 429);
        assert_eq!(v["title"], "Too Many Requests");

        let retry_after = resp.headers.iter().find(|(name, _)| name == "Retry-After");
        assert_eq!(retry_after.map(|(_, v)| v.as_str()), Some("1"));
    }

    // ---------------------------------------------------------------
    // too_many_redirects_response
    // ---------------------------------------------------------------

    #[test]
    fn test_too_many_redirects_response() {
        let resp = too_many_redirects_response("redirect loop");
        assert_eq!(resp.status, "508 Loop Detected");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 508);
        assert_eq!(v["title"], "Loop Detected");
    }

    // ---------------------------------------------------------------
    // not_implemented_response
    // ---------------------------------------------------------------

    #[test]
    fn test_not_implemented_response() {
        let resp = not_implemented_response("feature unavailable");
        assert_eq!(resp.status, "501 Not Implemented");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 501);
        assert_eq!(v["title"], "Not Implemented");
        assert_eq!(v["detail"], "feature unavailable");
    }

    // ---------------------------------------------------------------
    // signed_url_unauthorized_response
    // ---------------------------------------------------------------

    #[test]
    fn test_signed_url_unauthorized_response_no_www_authenticate() {
        let resp = signed_url_unauthorized_response("bad signature");
        assert_eq!(resp.status, "401 Unauthorized");
        assert_eq!(resp.content_type, Some("application/problem+json"));
        // Unlike auth_required_response, this should NOT have WWW-Authenticate.
        assert!(resp.headers.is_empty());

        let v = parse_body(&resp);
        assert_eq!(v["status"], 401);
        assert_eq!(v["detail"], "bad signature");
    }

    // ---------------------------------------------------------------
    // auth_required_response - WWW-Authenticate header
    // ---------------------------------------------------------------

    #[test]
    fn test_auth_required_response_includes_www_authenticate_header() {
        let resp = auth_required_response("token required");
        assert_eq!(resp.status, "401 Unauthorized");
        assert_eq!(resp.content_type, Some("application/problem+json"));

        let www_auth = resp
            .headers
            .iter()
            .find(|(name, _)| *name == "WWW-Authenticate");
        assert!(www_auth.is_some(), "must include WWW-Authenticate header");
        assert_eq!(www_auth.unwrap().1, "Bearer");

        let v = parse_body(&resp);
        assert_eq!(v["status"], 401);
        assert_eq!(v["title"], "Unauthorized");
        assert_eq!(v["detail"], "token required");
    }

    // ---------------------------------------------------------------
    // transform_error_response
    // ---------------------------------------------------------------

    #[test]
    fn test_transform_error_response_invalid_input() {
        let resp = transform_error_response(TransformError::InvalidInput("bad input".into()));
        assert_eq!(resp.status, "400 Bad Request");
        let v = parse_body(&resp);
        assert_eq!(v["detail"], "bad input");
    }

    #[test]
    fn test_transform_error_response_invalid_options() {
        let resp = transform_error_response(TransformError::InvalidOptions("bad opts".into()));
        assert_eq!(resp.status, "400 Bad Request");
        let v = parse_body(&resp);
        assert_eq!(v["detail"], "bad opts");
    }

    #[test]
    fn test_transform_error_response_decode_failed() {
        let resp = transform_error_response(TransformError::DecodeFailed("corrupt".into()));
        assert_eq!(resp.status, "400 Bad Request");
        let v = parse_body(&resp);
        assert_eq!(v["detail"], "corrupt");
    }

    #[test]
    fn test_transform_error_response_unsupported_input_media_type() {
        let resp = transform_error_response(TransformError::UnsupportedInputMediaType(
            "image/gif".into(),
        ));
        assert_eq!(resp.status, "415 Unsupported Media Type");
        let v = parse_body(&resp);
        assert_eq!(v["detail"], "image/gif");
    }

    #[test]
    fn test_transform_error_response_unsupported_output_media_type() {
        // Only gif and svg reach this arm, and each is refused for its own reason, so the
        // detail names the rule instead of restating the format the caller already sent.
        let resp =
            transform_error_response(TransformError::UnsupportedOutputMediaType(MediaType::Gif));
        assert_eq!(resp.status, "415 Unsupported Media Type");
        let v = parse_body(&resp);
        assert_eq!(
            v["detail"],
            "gif is an input-only format; choose an output format such as png, jpeg, webp, or avif"
        );

        let resp =
            transform_error_response(TransformError::UnsupportedOutputMediaType(MediaType::Svg));
        assert_eq!(resp.status, "415 Unsupported Media Type");
        let v = parse_body(&resp);
        assert_eq!(
            v["detail"],
            "svg output requires an svg input; choose a raster output format such as png, jpeg, webp, or avif"
        );
    }

    #[test]
    fn test_transform_error_response_encode_failed() {
        let resp = transform_error_response(TransformError::EncodeFailed("out of memory".into()));
        assert_eq!(resp.status, "500 Internal Server Error");
        let v = parse_body(&resp);
        assert_eq!(
            v["detail"],
            "failed to encode transformed artifact: out of memory"
        );
    }

    #[test]
    fn test_transform_error_response_capability_missing() {
        let resp = transform_error_response(TransformError::CapabilityMissing(
            "AVIF not compiled".into(),
        ));
        assert_eq!(resp.status, "501 Not Implemented");
        let v = parse_body(&resp);
        assert_eq!(v["detail"], "AVIF not compiled");
    }

    #[test]
    fn test_transform_error_response_limit_exceeded() {
        let resp = transform_error_response(TransformError::LimitExceeded("too large".into()));
        assert_eq!(resp.status, "413 Payload Too Large");
        let v = parse_body(&resp);
        assert_eq!(v["detail"], "too large");
    }

    // ---------------------------------------------------------------
    // map_source_io_error
    // ---------------------------------------------------------------

    #[test]
    fn test_map_source_io_error_not_found() {
        let err = io::Error::new(io::ErrorKind::NotFound, "no such file");
        let resp = map_source_io_error(err);
        assert_eq!(resp.status, "404 Not Found");
        let v = parse_body(&resp);
        assert_eq!(v["detail"], "source artifact was not found");
    }

    #[test]
    fn test_map_source_io_error_permission_denied() {
        let err = io::Error::new(io::ErrorKind::PermissionDenied, "forbidden");
        let resp = map_source_io_error(err);
        assert_eq!(resp.status, "500 Internal Server Error");
        let v = parse_body(&resp);
        let detail = v["detail"].as_str().unwrap();
        assert!(
            detail.starts_with("failed to access source artifact:"),
            "detail should describe the IO error, got: {detail}"
        );
    }

    #[test]
    fn test_map_source_io_error_other() {
        let err = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
        let resp = map_source_io_error(err);
        assert_eq!(resp.status, "500 Internal Server Error");
    }

    // ---------------------------------------------------------------
    // NOT_FOUND_BODY constant
    // ---------------------------------------------------------------

    #[test]
    fn test_not_found_body_is_valid_rfc7807_json() {
        let v: Value = serde_json::from_str(NOT_FOUND_BODY).expect("NOT_FOUND_BODY is valid JSON");
        assert_eq!(v["type"], "about:blank");
        assert_eq!(v["title"], "Not Found");
        assert_eq!(v["status"], 404);
        assert_eq!(v["detail"], "not found");
    }

    // ---------------------------------------------------------------
    // HttpResponse constructors
    // ---------------------------------------------------------------

    #[test]
    fn test_http_response_json_constructor() {
        let resp = HttpResponse::json("200 OK", b"{}".to_vec());
        assert_eq!(resp.status, "200 OK");
        assert_eq!(resp.content_type, Some("application/json"));
        assert!(resp.headers.is_empty());
        assert_eq!(resp.body, b"{}");
    }

    #[test]
    fn test_http_response_problem_constructor() {
        let resp = HttpResponse::problem("400 Bad Request", b"err".to_vec());
        assert_eq!(resp.content_type, Some("application/problem+json"));
        assert!(resp.headers.is_empty());
    }

    #[test]
    fn test_http_response_empty_constructor() {
        let headers = vec![("X-Custom".to_string(), "val".to_string())];
        let resp = HttpResponse::empty("204 No Content", headers);
        assert_eq!(resp.status, "204 No Content");
        assert!(resp.content_type.is_none());
        assert!(resp.body.is_empty());
        assert_eq!(resp.headers.len(), 1);
    }

    #[test]
    fn test_http_response_text_constructor() {
        let resp = HttpResponse::text("200 OK", "text/plain", b"hello".to_vec());
        assert_eq!(resp.content_type, Some("text/plain"));
        assert_eq!(resp.body, b"hello");
    }

    #[test]
    fn test_http_response_binary_with_headers_constructor() {
        let headers = vec![("Cache-Control".to_string(), "no-cache".to_string())];
        let resp =
            HttpResponse::binary_with_headers("200 OK", "image/png", headers, vec![0x89, 0x50]);
        assert_eq!(resp.content_type, Some("image/png"));
        assert_eq!(resp.headers.len(), 1);
        assert_eq!(resp.body, vec![0x89, 0x50]);
    }

    // ---------------------------------------------------------------
    // RFC 9457: every helper names its class in `type` and `title`
    // ---------------------------------------------------------------

    #[test]
    fn every_problem_response_names_its_class() {
        let responses = vec![
            (
                bad_request_response("x"),
                "invalid-request",
                "Invalid request",
                400,
            ),
            (
                auth_required_response("x"),
                "unauthorized",
                "Unauthorized",
                401,
            ),
            (
                signed_url_unauthorized_response("x"),
                "unauthorized",
                "Unauthorized",
                401,
            ),
            (forbidden_response("x"), "forbidden", "Forbidden", 403),
            (not_found_response("x"), "not-found", "Not Found", 404),
            (
                not_acceptable_response("x"),
                "not-acceptable",
                "Not Acceptable",
                406,
            ),
            (
                request_timeout_response("x"),
                "request-timeout",
                "Request Timeout",
                408,
            ),
            (
                payload_too_large_response("x"),
                "payload-too-large",
                "Payload Too Large",
                413,
            ),
            (
                unsupported_media_type_response("x"),
                "unsupported-media-type",
                "Unsupported Media Type",
                415,
            ),
            (
                unprocessable_entity_response("x"),
                "unprocessable-entity",
                "Unprocessable Entity",
                422,
            ),
            (
                too_many_requests_response("x"),
                "too-many-requests",
                "Too Many Requests",
                429,
            ),
            (
                internal_error_response("x"),
                "internal-error",
                "Internal Server Error",
                500,
            ),
            (
                not_implemented_response("x"),
                "not-implemented",
                "Not Implemented",
                501,
            ),
            (bad_gateway_response("x"), "bad-gateway", "Bad Gateway", 502),
            (
                service_unavailable_response("x"),
                "service-unavailable",
                "Service Unavailable",
                503,
            ),
            (
                too_many_redirects_response("x"),
                "loop-detected",
                "Loop Detected",
                508,
            ),
        ];

        for (resp, slug, title, status) in &responses {
            assert_eq!(
                resp.content_type,
                Some("application/problem+json"),
                "{slug}"
            );
            let v = parse_body(resp);
            assert_eq!(v["type"], format!("{PROBLEM_TYPES_URL}#{slug}"), "{slug}");
            assert_eq!(v["title"], *title, "{slug}");
            assert_eq!(v["status"], *status, "{slug}");
            assert_eq!(v["detail"], "x", "{slug}");
        }
    }

    /// The transform classes are the ones the Wasm package reports as `kind`, one per
    /// variant, so the two adapters classify a failure the same way.
    #[test]
    fn transform_errors_map_onto_their_own_problem_types() {
        let cases = vec![
            (
                TransformError::InvalidOptions("o".into()),
                "invalid-options",
                "400 Bad Request",
            ),
            (
                TransformError::InvalidInput("i".into()),
                "invalid-input",
                "400 Bad Request",
            ),
            (
                TransformError::DecodeFailed("d".into()),
                "decode-failed",
                "400 Bad Request",
            ),
            (
                TransformError::UnsupportedInputMediaType("u".into()),
                "unsupported-input-media-type",
                "415 Unsupported Media Type",
            ),
            (
                TransformError::UnsupportedOutputMediaType(MediaType::Gif),
                "unsupported-output-media-type",
                "415 Unsupported Media Type",
            ),
            (
                TransformError::EncodeFailed("e".into()),
                "encode-failed",
                "500 Internal Server Error",
            ),
            (
                TransformError::CapabilityMissing("c".into()),
                "capability-missing",
                "501 Not Implemented",
            ),
            (
                TransformError::LimitExceeded("l".into()),
                "limit-exceeded",
                "413 Payload Too Large",
            ),
        ];

        for (error, slug, status) in cases {
            let resp = transform_error_response(error);
            assert_eq!(resp.status, status, "{slug}");
            let v = parse_body(&resp);
            assert_eq!(v["type"], format!("{PROBLEM_TYPES_URL}#{slug}"), "{slug}");
            assert_ne!(v["title"], "", "{slug}");
        }
    }

    /// A route that does not exist is the one body where the status is all there is.
    #[test]
    fn only_the_unknown_route_keeps_about_blank() {
        let v: Value = serde_json::from_str(NOT_FOUND_BODY).expect("valid JSON");
        assert_eq!(v["type"], "about:blank");
    }

    #[test]
    fn warning_header_value_is_one_line_of_visible_ascii() {
        assert_eq!(warning_header_value("plain text"), "plain text");
        assert_eq!(warning_header_value("a\tb\r\nc"), "a b  c");
        assert_eq!(warning_header_value("caf\u{e9} \u{7f}"), "caf? ?");

        let mut headers = Vec::new();
        push_warning_headers(&mut headers, &["one".to_string(), "two".to_string()]);
        assert_eq!(
            headers,
            vec![
                (WARNING_HEADER.to_string(), "one".to_string()),
                (WARNING_HEADER.to_string(), "two".to_string()),
            ]
        );
    }

    #[test]
    fn attach_request_id_sets_the_header_and_the_problem_member() {
        let mut problem = bad_request_response("x");
        problem.attach_request_id("req-1");
        assert!(
            problem
                .headers
                .contains(&("X-Request-Id".to_string(), "req-1".to_string()))
        );
        let v = parse_body(&problem);
        assert_eq!(v["requestId"], "req-1");
        assert_eq!(v["type"], format!("{PROBLEM_TYPES_URL}#invalid-request"));
        assert!(problem.body.ends_with(b"\n"));

        let mut image = HttpResponse::json("200 OK", b"{}".to_vec());
        image.attach_request_id("req-2");
        assert!(
            image
                .headers
                .contains(&("X-Request-Id".to_string(), "req-2".to_string()))
        );
        assert_eq!(
            image.body,
            b"{}".to_vec(),
            "only a problem body gains the member"
        );
    }

    // ---------------------------------------------------------------
    // compression helpers
    // ---------------------------------------------------------------

    #[test]
    fn test_is_compressible_json() {
        assert!(is_compressible_content_type("application/json"));
        assert!(is_compressible_content_type("application/problem+json"));
    }

    #[test]
    fn test_is_not_compressible_image() {
        assert!(!is_compressible_content_type("image/png"));
        assert!(!is_compressible_content_type("image/jpeg"));
        assert!(!is_compressible_content_type("image/webp"));
    }

    #[test]
    fn test_is_compressible_text() {
        assert!(is_compressible_content_type("text/plain"));
        assert!(is_compressible_content_type(
            "application/openmetrics-text; version=1.0.0; charset=utf-8"
        ));
    }

    #[test]
    fn test_gzip_compress_roundtrip() {
        use flate2::read::GzDecoder;
        use std::io::Read;

        let original = b"hello world, this is test data that should compress well. \
                        repeating repeating repeating repeating repeating repeating.";
        let compressed = gzip_compress(original, 1).unwrap();
        assert!(compressed.len() < original.len());

        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_not_compressible_unknown_type() {
        assert!(!is_compressible_content_type("application/octet-stream"));
        assert!(!is_compressible_content_type("video/mp4"));
    }

    // ---------------------------------------------------------------
    // compression threshold boundary (m12)
    // ---------------------------------------------------------------

    #[test]
    fn test_gzip_compress_below_threshold_skipped() {
        // Body of exactly MIN_COMPRESS_BYTES - 1 should NOT be compressed.
        let body = vec![b'x'; MIN_COMPRESS_BYTES - 1];
        let response = HttpResponse::json("200 OK", body.clone());
        let mut _stream_buf: Vec<u8> = Vec::new();
        // We can't call write_response_compressed directly with a Cursor
        // because it expects a TcpStream, so we test the decision logic.
        let should_compress = response.body.len() >= MIN_COMPRESS_BYTES
            && response
                .content_type
                .is_some_and(is_compressible_content_type);
        assert!(
            !should_compress,
            "body below threshold should not be compressed"
        );
    }

    #[test]
    fn test_gzip_compress_at_threshold_eligible() {
        // Body of exactly MIN_COMPRESS_BYTES should be eligible for compression.
        let body = vec![b'x'; MIN_COMPRESS_BYTES];
        let response = HttpResponse::json("200 OK", body);
        let should_compress = response.body.len() >= MIN_COMPRESS_BYTES
            && response
                .content_type
                .is_some_and(is_compressible_content_type);
        assert!(
            should_compress,
            "body at threshold should be eligible for compression"
        );
    }

    #[test]
    fn test_gzip_compress_above_threshold_eligible() {
        let body = vec![b'x'; MIN_COMPRESS_BYTES + 1];
        let response = HttpResponse::json("200 OK", body);
        let should_compress = response.body.len() >= MIN_COMPRESS_BYTES
            && response
                .content_type
                .is_some_and(is_compressible_content_type);
        assert!(
            should_compress,
            "body above threshold should be eligible for compression"
        );
    }

    // ---------------------------------------------------------------
    // write_response_compressed integration (m11)
    // ---------------------------------------------------------------

    /// Helper to capture the raw HTTP response bytes by using a connected
    /// socket pair.
    #[cfg(unix)]
    fn capture_response(response: HttpResponse, accepts_gzip: bool) -> Vec<u8> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let (mut server_stream, _) = listener.accept().unwrap();

        write_response_compressed(&mut server_stream, response, true, accepts_gzip, 1).unwrap();
        drop(server_stream);

        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut client, &mut buf).unwrap();
        buf
    }

    #[cfg(unix)]
    #[test]
    fn test_write_response_compressed_applies_gzip() {
        use flate2::read::GzDecoder;
        use std::io::Read;

        // Create a JSON body large enough to be compressed.
        let body = format!("{{\"data\":\"{}\"}}", "x".repeat(256));
        let response = HttpResponse::json("200 OK", body.as_bytes().to_vec());
        let raw = capture_response(response, true);
        let raw_str = String::from_utf8_lossy(&raw);

        assert!(
            raw_str.contains("Content-Encoding: gzip"),
            "should contain Content-Encoding: gzip"
        );
        assert!(
            raw_str.contains("Vary: Accept-Encoding"),
            "should contain Vary header"
        );

        // Extract the body after the \r\n\r\n separator and decompress.
        let body_start = raw_str.find("\r\n\r\n").unwrap() + 4;
        let compressed_body = &raw[body_start..];
        let mut decoder = GzDecoder::new(compressed_body);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed).unwrap();
        assert_eq!(decompressed, body);
    }

    #[cfg(unix)]
    #[test]
    fn test_write_response_compressed_skips_when_not_accepted() {
        let body = format!("{{\"data\":\"{}\"}}", "x".repeat(256));
        let response = HttpResponse::json("200 OK", body.as_bytes().to_vec());
        let raw = capture_response(response, false);
        let raw_str = String::from_utf8_lossy(&raw);

        assert!(
            !raw_str.contains("Content-Encoding: gzip"),
            "should NOT contain Content-Encoding: gzip"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_write_response_compressed_skips_small_body() {
        // Body smaller than MIN_COMPRESS_BYTES should not be compressed.
        let body = b"{\"ok\":true}".to_vec();
        let response = HttpResponse::json("200 OK", body);
        let raw = capture_response(response, true);
        let raw_str = String::from_utf8_lossy(&raw);

        assert!(
            !raw_str.contains("Content-Encoding: gzip"),
            "small body should not be compressed"
        );
        // Vary header should still be present for compressible content types.
        assert!(
            raw_str.contains("Vary: Accept-Encoding"),
            "Vary should be present for compressible type"
        );
    }
}
