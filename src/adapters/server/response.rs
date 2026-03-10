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
    pub(super) headers: Vec<(&'static str, String)>,
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
        headers: Vec<(&'static str, String)>,
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
        headers: Vec<(&'static str, String)>,
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

    pub(super) fn empty(status: &'static str, headers: Vec<(&'static str, String)>) -> Self {
        Self {
            status,
            content_type: None,
            headers,
            body: Vec::new(),
        }
    }
}

pub(super) fn write_response(
    stream: &mut TcpStream,
    response: HttpResponse,
    close: bool,
) -> io::Result<()> {
    use std::fmt::Write as FmtWrite;

    let connection_value = if close { "close" } else { "keep-alive" };
    let mut header = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: {connection_value}\r\n",
        response.status,
        response.body.len()
    );

    if let Some(content_type) = response.content_type {
        let _ = write!(header, "Content-Type: {content_type}\r\n");
    }

    for (name, value) in response.headers {
        let _ = write!(header, "{name}: {value}\r\n");
    }

    header.push_str("\r\n");

    stream.write_all(header.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

pub(super) fn bad_request_response(message: &str) -> HttpResponse {
    problem_response("400 Bad Request", 400, "Bad Request", message)
}

pub(super) fn auth_required_response(message: &str) -> HttpResponse {
    HttpResponse::problem_with_headers(
        "401 Unauthorized",
        vec![("WWW-Authenticate", "Bearer".to_string())],
        problem_detail_body(401, "Unauthorized", message),
    )
}

pub(super) fn signed_url_unauthorized_response(message: &str) -> HttpResponse {
    problem_response("401 Unauthorized", 401, "Unauthorized", message)
}

pub(super) fn not_found_response(message: &str) -> HttpResponse {
    problem_response("404 Not Found", 404, "Not Found", message)
}

pub(super) fn forbidden_response(message: &str) -> HttpResponse {
    problem_response("403 Forbidden", 403, "Forbidden", message)
}

pub(super) fn unsupported_media_type_response(message: &str) -> HttpResponse {
    problem_response(
        "415 Unsupported Media Type",
        415,
        "Unsupported Media Type",
        message,
    )
}

pub(super) fn not_acceptable_response(message: &str) -> HttpResponse {
    problem_response("406 Not Acceptable", 406, "Not Acceptable", message)
}

pub(super) fn payload_too_large_response(message: &str) -> HttpResponse {
    problem_response("413 Payload Too Large", 413, "Payload Too Large", message)
}

pub(super) fn internal_error_response(message: &str) -> HttpResponse {
    problem_response(
        "500 Internal Server Error",
        500,
        "Internal Server Error",
        message,
    )
}

pub(super) fn bad_gateway_response(message: &str) -> HttpResponse {
    problem_response("502 Bad Gateway", 502, "Bad Gateway", message)
}

pub(super) fn service_unavailable_response(message: &str) -> HttpResponse {
    problem_response(
        "503 Service Unavailable",
        503,
        "Service Unavailable",
        message,
    )
}

pub(super) fn too_many_redirects_response(message: &str) -> HttpResponse {
    problem_response("508 Loop Detected", 508, "Loop Detected", message)
}

pub(super) fn not_implemented_response(message: &str) -> HttpResponse {
    problem_response("501 Not Implemented", 501, "Not Implemented", message)
}

/// Builds an RFC 7807 Problem Details error response.
///
/// The response uses `application/problem+json` content type and includes
/// `type`, `title`, `status`, and `detail` fields as specified by RFC 7807.
/// The `type` field uses `about:blank` to indicate that the HTTP status code
/// itself is sufficient to describe the problem type.
pub(super) fn problem_response(
    status: &'static str,
    status_code: u16,
    title: &str,
    detail: &str,
) -> HttpResponse {
    HttpResponse::problem(status, problem_detail_body(status_code, title, detail))
}

/// Serializes an RFC 7807 Problem Details JSON body.
pub(super) fn problem_detail_body(status: u16, title: &str, detail: &str) -> Vec<u8> {
    let mut body = serde_json::to_vec(&json!({
        "type": "about:blank",
        "title": title,
        "status": status,
        "detail": detail,
    }))
    .expect("serialize problem detail body");
    body.push(b'\n');
    body
}

pub(super) fn transform_error_response(error: TransformError) -> HttpResponse {
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
        TransformError::LimitExceeded(reason) => payload_too_large_response(&reason),
    }
}

pub(super) fn map_source_io_error(error: io::Error) -> HttpResponse {
    match error.kind() {
        io::ErrorKind::NotFound => not_found_response("source artifact was not found"),
        _ => internal_error_response(&format!("failed to access source artifact: {error}")),
    }
}
