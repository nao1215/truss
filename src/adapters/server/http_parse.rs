use super::response::{
    HttpResponse, bad_request_response, internal_error_response, not_implemented_response,
    payload_too_large_response,
};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub(super) const MAX_HEADER_BYTES: usize = 16 * 1024;
pub(super) const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
pub(super) const DEFAULT_MAX_UPLOAD_BODY_BYTES: usize = 100 * 1024 * 1024;

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
#[derive(Debug)]
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
pub(super) fn read_request_headers<R>(
    stream: &mut R,
    max_upload_bytes: usize,
) -> Result<PartialHttpRequest, HttpResponse>
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
        if buffer.len() > MAX_HEADER_BYTES + max_upload_bytes {
            return Err(payload_too_large_response("request is too large"));
        }

        if let Some(index) = find_header_terminator(&buffer) {
            if index > MAX_HEADER_BYTES {
                return Err(payload_too_large_response("request headers are too large"));
            }
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
    let max_body = max_body_for_headers(&headers, max_upload_bytes);
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
    let mut body = if partial.overflow.len() > partial.content_length {
        let mut overflow = partial.overflow;
        let tail = overflow.split_off(partial.content_length);
        // tail belongs to the next request on the kept-alive connection;
        // currently we do not pipeline, so we discard it here, but at least
        // we no longer lose the body bytes.
        let _ = tail;
        overflow
    } else {
        partial.overflow
    };

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

    if body.len() > partial.content_length {
        body.truncate(partial.content_length);
    }

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

        if name != name.trim() || name.is_empty() {
            return Err(bad_request_response(
                "header name must not be empty or contain leading/trailing whitespace",
            ));
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
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
    if headers.iter().any(|(name, _)| name == "transfer-encoding") {
        return Err(not_implemented_response(
            "Transfer-Encoding is not supported; use Content-Length instead",
        ));
    }

    Ok(headers)
}

pub(super) fn parse_content_length(headers: &[(String, String)]) -> Result<usize, HttpResponse> {
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
/// `max_upload_bytes` because real-world photographs easily exceed the
/// 1 MiB default. All other requests keep the tighter [`MAX_REQUEST_BODY_BYTES`]
/// limit to bound JSON parsing and header-only endpoints.
pub(super) fn max_body_for_headers(headers: &[(String, String)], max_upload_bytes: usize) -> usize {
    let is_multipart = headers.iter().any(|(name, value)| {
        name == "content-type" && content_type_matches(value, "multipart/form-data")
    });
    if is_multipart {
        max_upload_bytes
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

pub(super) fn parse_named<T, F>(value: &str, field_name: &str, parser: F) -> Result<T, HttpResponse>
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

pub(super) fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_request_line ─────────────────────────────────────────

    #[test]
    fn test_parse_request_line_valid_get() {
        let (method, target, version) = parse_request_line("GET /image.png HTTP/1.1").unwrap();
        assert_eq!(method, "GET");
        assert_eq!(target, "/image.png");
        assert_eq!(version, "HTTP/1.1");
    }

    #[test]
    fn test_parse_request_line_valid_post() {
        let (method, target, version) = parse_request_line("POST /upload HTTP/1.0").unwrap();
        assert_eq!(method, "POST");
        assert_eq!(target, "/upload");
        assert_eq!(version, "HTTP/1.0");
    }

    #[test]
    fn test_parse_request_line_missing_method() {
        let err = parse_request_line("").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_request_line_missing_target() {
        let err = parse_request_line("GET").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_request_line_missing_version() {
        let err = parse_request_line("GET /path").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_request_line_too_many_fields() {
        let err = parse_request_line("GET /path HTTP/1.1 extra").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_request_line_extra_whitespace_collapsed() {
        // split_whitespace collapses multiple spaces — method/target/version still parsed
        let (method, target, version) = parse_request_line("GET   /path   HTTP/1.1").unwrap();
        assert_eq!(method, "GET");
        assert_eq!(target, "/path");
        assert_eq!(version, "HTTP/1.1");
    }

    // ── parse_headers ──────────────────────────────────────────────

    fn header_lines(raw: &str) -> Vec<(String, String)> {
        parse_headers(raw.split("\r\n")).unwrap()
    }

    #[test]
    fn test_parse_headers_single_header() {
        let headers = header_lines("Host: example.com");
        assert_eq!(
            headers,
            vec![("host".to_string(), "example.com".to_string())]
        );
    }

    #[test]
    fn test_parse_headers_value_trimmed() {
        let headers = header_lines("Content-Type:  application/json  ");
        assert_eq!(headers[0].1, "application/json");
    }

    #[test]
    fn test_parse_headers_name_lowercased() {
        let headers = header_lines("X-Custom-Header: value");
        assert_eq!(headers[0].0, "x-custom-header");
    }

    #[test]
    fn test_parse_headers_empty_lines_skipped() {
        let headers = header_lines("\r\nHost: a\r\n\r\nAccept: b");
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn test_parse_headers_missing_colon_rejected() {
        let err = parse_headers("BadHeader".split("\r\n")).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_headers_empty_name_rejected() {
        let err = parse_headers(": value".split("\r\n")).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_headers_leading_whitespace_in_name_rejected() {
        let err = parse_headers(" Host: value".split("\r\n")).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_headers_trailing_whitespace_in_name_rejected() {
        let err = parse_headers("Host : value".split("\r\n")).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_headers_duplicate_host_rejected() {
        let err = parse_headers("Host: a\r\nHost: b".split("\r\n")).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_headers_duplicate_content_length_rejected() {
        let err =
            parse_headers("Content-Length: 10\r\nContent-Length: 20".split("\r\n")).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_headers_duplicate_authorization_rejected() {
        let err = parse_headers("Authorization: Bearer a\r\nAuthorization: Bearer b".split("\r\n"))
            .unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_headers_transfer_encoding_rejected_501() {
        let err = parse_headers("Transfer-Encoding: chunked".split("\r\n")).unwrap_err();
        assert_eq!(err.status, "501 Not Implemented");
    }

    #[test]
    fn test_parse_headers_non_singleton_duplicates_allowed() {
        let headers = header_lines("X-Custom: a\r\nX-Custom: b");
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn test_parse_headers_value_with_colon() {
        // Values may contain colons (e.g. timestamps, URLs)
        let headers = header_lines("X-Time: 12:30:00");
        assert_eq!(headers[0].1, "12:30:00");
    }

    // ── parse_content_length ───────────────────────────────────────

    #[test]
    fn test_parse_content_length_absent_defaults_to_zero() {
        let headers: Vec<(String, String)> = vec![];
        assert_eq!(parse_content_length(&headers).unwrap(), 0);
    }

    #[test]
    fn test_parse_content_length_valid() {
        let headers = vec![("content-length".to_string(), "42".to_string())];
        assert_eq!(parse_content_length(&headers).unwrap(), 42);
    }

    #[test]
    fn test_parse_content_length_zero() {
        let headers = vec![("content-length".to_string(), "0".to_string())];
        assert_eq!(parse_content_length(&headers).unwrap(), 0);
    }

    #[test]
    fn test_parse_content_length_non_numeric_rejected() {
        let headers = vec![("content-length".to_string(), "abc".to_string())];
        let err = parse_content_length(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_content_length_negative_rejected() {
        let headers = vec![("content-length".to_string(), "-1".to_string())];
        let err = parse_content_length(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_content_length_float_rejected() {
        let headers = vec![("content-length".to_string(), "1.5".to_string())];
        let err = parse_content_length(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ── resolve_storage_path ───────────────────────────────────────

    #[test]
    fn test_resolve_storage_path_simple_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("image.png");
        std::fs::File::create(&file_path).unwrap();

        let resolved = resolve_storage_path(dir.path(), "/image.png").unwrap();
        assert_eq!(resolved, file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_storage_path_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub/dir")).unwrap();
        let file_path = dir.path().join("sub/dir/image.png");
        std::fs::File::create(&file_path).unwrap();

        let resolved = resolve_storage_path(dir.path(), "/sub/dir/image.png").unwrap();
        assert_eq!(resolved, file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_storage_path_no_leading_slash() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("image.png");
        std::fs::File::create(&file_path).unwrap();

        let resolved = resolve_storage_path(dir.path(), "image.png").unwrap();
        assert_eq!(resolved, file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_storage_path_empty_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_storage_path(dir.path(), "").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_resolve_storage_path_slash_only_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_storage_path(dir.path(), "/").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_resolve_storage_path_dot_dot_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_storage_path(dir.path(), "/../etc/passwd").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_resolve_storage_path_mid_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let err = resolve_storage_path(dir.path(), "/sub/../../etc/passwd").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_resolve_storage_path_dot_segment_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_storage_path(dir.path(), "/./image.png").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_resolve_storage_path_encoded_dot_dot_via_components() {
        // Even if someone passes ".." directly it is caught by Component::ParentDir
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_storage_path(dir.path(), "..").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    #[cfg(unix)]
    fn test_resolve_storage_path_symlink_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        // Create a symlink that points outside the storage root
        let link_path = dir.path().join("escape");
        std::os::unix::fs::symlink("/etc", &link_path).unwrap();

        // The symlink target resolves outside the root -> rejected
        let err = resolve_storage_path(dir.path(), "/escape/passwd").unwrap_err();
        // Could be 400 (escapes root) or 404 (file not found) depending on OS,
        // but it must NOT succeed
        assert!(err.status.starts_with("4") || err.status.starts_with("5"));
    }

    #[test]
    fn test_resolve_storage_path_nonexistent_file() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_storage_path(dir.path(), "/no_such_file.png").unwrap_err();
        // canonicalize fails -> error response (404 from map_source_io_error)
        assert!(err.status.starts_with("4") || err.status.starts_with("5"));
    }

    // ── content_type_matches ───────────────────────────────────────

    #[test]
    fn test_content_type_matches_exact() {
        assert!(content_type_matches("application/json", "application/json"));
    }

    #[test]
    fn test_content_type_matches_case_insensitive() {
        assert!(content_type_matches("Application/JSON", "application/json"));
    }

    #[test]
    fn test_content_type_matches_with_parameters() {
        assert!(content_type_matches(
            "application/json; charset=utf-8",
            "application/json"
        ));
    }

    #[test]
    fn test_content_type_matches_with_whitespace_in_params() {
        assert!(content_type_matches(
            "multipart/form-data ; boundary=abc",
            "multipart/form-data"
        ));
    }

    #[test]
    fn test_content_type_no_match() {
        assert!(!content_type_matches("text/plain", "application/json"));
    }

    #[test]
    fn test_content_type_empty_value() {
        assert!(!content_type_matches("", "application/json"));
    }

    // ── find_subslice ──────────────────────────────────────────────

    #[test]
    fn test_find_subslice_found_at_start() {
        assert_eq!(find_subslice(b"abcdef", b"abc"), Some(0));
    }

    #[test]
    fn test_find_subslice_found_at_middle() {
        assert_eq!(find_subslice(b"abcdef", b"cde"), Some(2));
    }

    #[test]
    fn test_find_subslice_found_at_end() {
        assert_eq!(find_subslice(b"abcdef", b"def"), Some(3));
    }

    #[test]
    fn test_find_subslice_not_found() {
        assert_eq!(find_subslice(b"abcdef", b"xyz"), None);
    }

    #[test]
    fn test_find_subslice_needle_larger_than_haystack() {
        assert_eq!(find_subslice(b"ab", b"abcd"), None);
    }

    #[test]
    #[should_panic(expected = "window size must be non-zero")]
    fn test_find_subslice_empty_needle_panics() {
        // An empty needle causes windows(0) to panic. Callers must ensure
        // the needle is non-empty.
        let _ = find_subslice(b"abc", b"");
    }

    // ── find_valid_boundary ────────────────────────────────────────

    #[test]
    fn test_find_valid_boundary_with_crlf() {
        let data = b"--boundary\r\ncontent here";
        assert_eq!(find_valid_boundary(data, b"--boundary"), Some(0));
    }

    #[test]
    fn test_find_valid_boundary_closing_with_dashes() {
        let data = b"--boundary--";
        assert_eq!(find_valid_boundary(data, b"--boundary"), Some(0));
    }

    #[test]
    fn test_find_valid_boundary_false_match_without_suffix() {
        // Boundary string appears but is NOT followed by \r\n or --
        let data = b"--boundaryXXXX";
        assert_eq!(find_valid_boundary(data, b"--boundary"), None);
    }

    #[test]
    fn test_find_valid_boundary_false_match_then_real_match() {
        // First occurrence is not valid, second is
        let data = b"--boundaryXXXX--boundary\r\ndata";
        assert_eq!(find_valid_boundary(data, b"--boundary"), Some(14));
    }

    #[test]
    fn test_find_valid_boundary_at_offset() {
        let data = b"preamble\r\n--boundary\r\npart data";
        assert_eq!(find_valid_boundary(data, b"--boundary"), Some(10));
    }

    #[test]
    fn test_find_valid_boundary_not_found() {
        let data = b"no boundary here\r\n";
        assert_eq!(find_valid_boundary(data, b"--boundary"), None);
    }

    #[test]
    fn test_find_valid_boundary_truncated_suffix() {
        // Boundary at end of buffer with only 1 byte after (not enough for \r\n or --)
        let data = b"--boundary\r";
        assert_eq!(find_valid_boundary(data, b"--boundary"), None);
    }

    // ── find_header_terminator ─────────────────────────────────────

    #[test]
    fn test_find_header_terminator_present() {
        let data = b"Host: example.com\r\n\r\nbody";
        assert_eq!(find_header_terminator(data), Some(17));
    }

    #[test]
    fn test_find_header_terminator_absent() {
        let data = b"Host: example.com\r\n";
        assert_eq!(find_header_terminator(data), None);
    }

    #[test]
    fn test_find_header_terminator_at_start() {
        let data = b"\r\n\r\nbody";
        assert_eq!(find_header_terminator(data), Some(0));
    }

    #[test]
    fn test_find_header_terminator_lf_only_not_matched() {
        let data = b"Host: x\n\nbody";
        assert_eq!(find_header_terminator(data), None);
    }

    // ── max_body_for_headers ───────────────────────────────────────

    #[test]
    fn test_max_body_for_headers_default_limit() {
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        assert_eq!(
            max_body_for_headers(&headers, DEFAULT_MAX_UPLOAD_BODY_BYTES),
            MAX_REQUEST_BODY_BYTES
        );
    }

    #[test]
    fn test_max_body_for_headers_multipart_gets_upload_limit() {
        let headers = vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=abc".to_string(),
        )];
        assert_eq!(
            max_body_for_headers(&headers, DEFAULT_MAX_UPLOAD_BODY_BYTES),
            DEFAULT_MAX_UPLOAD_BODY_BYTES
        );
    }

    #[test]
    fn test_max_body_for_headers_no_content_type() {
        let headers: Vec<(String, String)> = vec![];
        assert_eq!(
            max_body_for_headers(&headers, DEFAULT_MAX_UPLOAD_BODY_BYTES),
            MAX_REQUEST_BODY_BYTES
        );
    }

    // ── request_has_json_content_type ──────────────────────────────

    #[test]
    fn test_request_has_json_content_type_true() {
        let req = HttpRequest {
            method: "POST".to_string(),
            target: "/convert".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: vec![],
        };
        assert!(request_has_json_content_type(&req));
    }

    #[test]
    fn test_request_has_json_content_type_with_charset() {
        let req = HttpRequest {
            method: "POST".to_string(),
            target: "/convert".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![(
                "content-type".to_string(),
                "application/json; charset=utf-8".to_string(),
            )],
            body: vec![],
        };
        assert!(request_has_json_content_type(&req));
    }

    #[test]
    fn test_request_has_json_content_type_false_for_other_types() {
        let req = HttpRequest {
            method: "POST".to_string(),
            target: "/upload".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![(
                "content-type".to_string(),
                "multipart/form-data".to_string(),
            )],
            body: vec![],
        };
        assert!(!request_has_json_content_type(&req));
    }

    #[test]
    fn test_request_has_json_content_type_missing_header() {
        let req = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert!(!request_has_json_content_type(&req));
    }

    // ── HttpRequest helper methods ─────────────────────────────────

    #[test]
    fn test_http_request_header_lookup() {
        let req = HttpRequest {
            method: "GET".to_string(),
            target: "/".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("host".to_string(), "example.com".to_string()),
                ("accept".to_string(), "image/webp".to_string()),
            ],
            body: vec![],
        };
        assert_eq!(req.header("host"), Some("example.com"));
        assert_eq!(req.header("accept"), Some("image/webp"));
        assert_eq!(req.header("missing"), None);
    }

    #[test]
    fn test_http_request_path_without_query() {
        let req = HttpRequest {
            method: "GET".to_string(),
            target: "/images/photo.jpg".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert_eq!(req.path(), "/images/photo.jpg");
        assert_eq!(req.query(), None);
    }

    #[test]
    fn test_http_request_path_with_query() {
        let req = HttpRequest {
            method: "GET".to_string(),
            target: "/convert?width=100&format=webp".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert_eq!(req.path(), "/convert");
        assert_eq!(req.query(), Some("width=100&format=webp"));
    }

    // ── read_request_headers + read_request_body (integration) ────

    #[test]
    fn test_read_request_headers_and_body_roundtrip() {
        let raw = b"POST /upload HTTP/1.1\r\nContent-Length: 5\r\nHost: localhost\r\n\r\nhello";
        let mut cursor = std::io::Cursor::new(raw.to_vec());

        let partial = read_request_headers(&mut cursor).unwrap();
        assert_eq!(partial.method, "POST");
        assert_eq!(partial.target, "/upload");
        assert_eq!(partial.content_length, 5);

        let req = read_request_body(&mut cursor, partial).unwrap();
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn test_read_request_headers_no_body() {
        let raw = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let mut cursor = std::io::Cursor::new(raw.to_vec());

        let partial = read_request_headers(&mut cursor).unwrap();
        assert_eq!(partial.method, "GET");
        assert_eq!(partial.content_length, 0);
    }

    #[test]
    fn test_read_request_headers_truncated_stream() {
        let raw = b"GET /health HTTP/1.1\r\nHost: loc";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        let err = read_request_headers(&mut cursor).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }
}
