use super::response::{
    HttpResponse, bad_request_response, internal_error_response, not_implemented_response,
    payload_too_large_response,
};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub(super) const MAX_HEADER_BYTES: usize = 16 * 1024;
pub(super) const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
pub(super) const MAX_UPLOAD_BODY_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) version: String,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl HttpRequest {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(header_name, value)| (header_name == name).then_some(value.as_str()))
    }

    pub(super) fn path(&self) -> &str {
        self.target
            .split('?')
            .next()
            .unwrap_or(self.target.as_str())
    }

    pub(super) fn query(&self) -> Option<&str> {
        self.target.split_once('?').map(|(_, query)| query)
    }
}

/// Partially parsed HTTP request containing only the request line and headers.
/// The body has not been read yet. Used to perform early authentication before
/// consuming the (potentially large) request body.
pub(super) struct PartialHttpRequest {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) version: String,
    pub(super) headers: Vec<(String, String)>,
    /// Bytes already buffered beyond the header terminator during header reading.
    /// These belong to the body and are passed to `read_request_body`.
    pub(super) overflow: Vec<u8>,
    /// The validated Content-Length value.
    pub(super) content_length: usize,
}

impl PartialHttpRequest {
    pub(super) fn path(&self) -> &str {
        self.target
            .split('?')
            .next()
            .unwrap_or(self.target.as_str())
    }
}

/// Reads only the HTTP request line and headers from `stream`, returning a
/// [`PartialHttpRequest`]. Any bytes already buffered beyond the header
/// terminator are stored in `overflow` so that `read_request_body` can
/// continue from the right position.
///
/// This split allows callers to inspect the method, path, and headers (e.g.
/// for authentication) *before* committing resources to read a potentially
/// large request body.
pub(super) fn read_request_headers<R>(stream: &mut R) -> Result<PartialHttpRequest, HttpResponse>
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

        // While searching for the header terminator we only enforce the
        // header-size limit.  Body-size limits are checked after headers are
        // parsed (in `read_request_body`).
        if buffer.len() > MAX_HEADER_BYTES + MAX_UPLOAD_BODY_BYTES {
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
    let max_body = max_body_for_headers(&headers);
    if content_length > max_body {
        return Err(payload_too_large_response("request body is too large"));
    }

    let overflow = buffer[(header_end + 4)..].to_vec();

    Ok(PartialHttpRequest {
        method,
        target,
        version,
        headers,
        overflow,
        content_length,
    })
}

/// Reads the remaining body bytes for a [`PartialHttpRequest`] and assembles
/// the final [`HttpRequest`].
pub(super) fn read_request_body<R>(
    stream: &mut R,
    partial: PartialHttpRequest,
) -> Result<HttpRequest, HttpResponse>
where
    R: Read,
{
    let mut body = partial.overflow;
    let mut chunk = [0_u8; 4096];
    while body.len() < partial.content_length {
        let read = stream.read(&mut chunk).map_err(|error| {
            internal_error_response(&format!("failed to read request: {error}"))
        })?;
        if read == 0 {
            return Err(bad_request_response("request body was truncated"));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(partial.content_length);

    Ok(HttpRequest {
        method: partial.method,
        target: partial.target,
        version: partial.version,
        headers: partial.headers,
        body,
    })
}

pub(super) fn parse_request_line(
    request_line: &str,
) -> Result<(String, String, String), HttpResponse> {
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

/// Headers that must appear at most once per request. Duplicates of these create
/// interpretation differences between proxies and the origin, which is a security
/// concern when the server runs behind a reverse proxy.
pub(super) const SINGLETON_HEADERS: &[&str] = &[
    "host",
    "authorization",
    "content-length",
    "content-type",
    "transfer-encoding",
];

pub(super) fn parse_headers<'a, I>(lines: I) -> Result<Vec<(String, String)>, HttpResponse>
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

    for &singleton in SINGLETON_HEADERS {
        let count = headers.iter().filter(|(name, _)| name == singleton).count();
        if count > 1 {
            return Err(bad_request_response(&format!(
                "duplicate `{singleton}` header is not allowed"
            )));
        }
    }

    // This server does not implement chunked transfer decoding.  Accepting a
    // Transfer-Encoding header while using Content-Length for body framing
    // creates a request-smuggling vector when running behind a reverse proxy.
    // Reject it outright with 501 Not Implemented.
    if headers
        .iter()
        .any(|(name, _)| name == "transfer-encoding")
    {
        return Err(not_implemented_response(
            "Transfer-Encoding is not supported; use Content-Length instead",
        ));
    }

    Ok(headers)
}

pub(super) fn parse_content_length(
    headers: &[(String, String)],
) -> Result<usize, HttpResponse> {
    // Duplicate content-length is already rejected by the SINGLETON_HEADERS check
    // in parse_headers, so we only need to find the first (and only) value here.
    let Some(value) = headers
        .iter()
        .find_map(|(name, value)| (name == "content-length").then_some(value.as_str()))
    else {
        return Ok(0);
    };

    value
        .parse::<usize>()
        .map_err(|_| bad_request_response("content-length must be a non-negative integer"))
}

pub(super) fn request_has_json_content_type(request: &HttpRequest) -> bool {
    request
        .header("content-type")
        .is_some_and(|value| content_type_matches(value, "application/json"))
}

/// Returns the maximum allowed body size for a request. Multipart uploads
/// (identified by `content-type: multipart/form-data`) are allowed up to
/// [`MAX_UPLOAD_BODY_BYTES`] because real-world photographs easily exceed the
/// 1 MiB default. All other requests keep the tighter [`MAX_REQUEST_BODY_BYTES`]
/// limit to bound JSON parsing and header-only endpoints.
pub(super) fn max_body_for_headers(headers: &[(String, String)]) -> usize {
    let is_multipart = headers.iter().any(|(name, value)| {
        name == "content-type" && content_type_matches(value, "multipart/form-data")
    });
    if is_multipart {
        MAX_UPLOAD_BODY_BYTES
    } else {
        MAX_REQUEST_BODY_BYTES
    }
}

pub(super) fn parse_optional_named<T, F>(
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

pub(super) fn parse_named<T, F>(
    value: &str,
    field_name: &str,
    parser: F,
) -> Result<T, HttpResponse>
where
    F: Fn(&str) -> Result<T, String>,
{
    parser(value).map_err(|reason| bad_request_response(&format!("{field_name}: {reason}")))
}

pub(super) fn resolve_storage_path(
    storage_root: &Path,
    source_path: &str,
) -> Result<PathBuf, HttpResponse> {
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
    let canonical_candidate = candidate
        .canonicalize()
        .map_err(super::response::map_source_io_error)?;

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(bad_request_response("source path escapes the storage root"));
    }

    Ok(canonical_candidate)
}

pub(super) fn header_value<'a>(
    headers: &'a [(String, String)],
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find_map(|(header_name, value)| (header_name == name).then_some(value.as_str()))
}

pub(super) fn content_type_matches(value: &str, expected: &str) -> bool {
    value
        .split(';')
        .next()
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

pub(super) fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Finds a multipart boundary delimiter in `haystack`, verifying that the
/// bytes immediately following the match are either `\r\n` (next part) or
/// `--` (closing boundary).  This prevents false matches when the boundary
/// string appears inside binary payload data.
pub(super) fn find_valid_boundary(haystack: &[u8], delimiter: &[u8]) -> Option<usize> {
    let mut start = 0;
    while start + delimiter.len() <= haystack.len() {
        if let Some(pos) = find_subslice(&haystack[start..], delimiter) {
            let abs = start + pos;
            let after = abs + delimiter.len();
            // A valid boundary must be followed by CRLF (next part) or "--" (closing).
            if after + 2 <= haystack.len() {
                let suffix = &haystack[after..after + 2];
                if suffix == b"\r\n" || suffix == b"--" {
                    return Some(abs);
                }
            }
            // Not a valid boundary; skip past this match and keep searching.
            start = abs + 1;
        } else {
            break;
        }
    }
    None
}

pub(super) fn find_header_terminator(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}
