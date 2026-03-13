use super::http_parse::{
    HttpRequest, content_type_matches, find_subslice, find_valid_boundary, header_value,
    parse_headers,
};
use super::response::{HttpResponse, bad_request_response, unsupported_media_type_response};
use crate::{TransformOptions, WatermarkInput};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MultipartPart {
    pub(super) name: String,
    pub(super) content_type: Option<String>,
    /// Byte range within the original request body, avoiding a copy of the part data.
    pub(super) body_range: std::ops::Range<usize>,
}

pub(super) fn parse_upload_request(
    body: &[u8],
    boundary: &str,
) -> Result<(Vec<u8>, TransformOptions, Option<WatermarkInput>), HttpResponse> {
    let parts = parse_multipart_form_data(body, boundary)?;
    let mut file_range = None;
    let mut options = None;
    let mut watermark_range = None;
    let mut watermark_position = None;
    let mut watermark_opacity = None;
    let mut watermark_margin = None;

    for part in parts {
        match part.name.as_str() {
            "file" => {
                if file_range.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `file` fields",
                    ));
                }
                if part.body_range.is_empty() {
                    return Err(bad_request_response(
                        "multipart upload `file` field must not be empty",
                    ));
                }
                file_range = Some(part.body_range);
            }
            "options" => {
                if options.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `options` fields",
                    ));
                }
                let part_body = &body[part.body_range];
                if let Some(content_type) = part.content_type.as_deref()
                    && !content_type_matches(content_type, "application/json")
                {
                    return Err(bad_request_response(
                        "multipart upload `options` field must use application/json when a content type is provided",
                    ));
                }
                let payload = if part_body.is_empty() {
                    super::TransformOptionsPayload::default()
                } else {
                    serde_json::from_slice::<super::TransformOptionsPayload>(part_body).map_err(
                        |error| {
                            bad_request_response(&format!(
                                "multipart upload `options` field must contain valid JSON: {error}"
                            ))
                        },
                    )?
                };
                options = Some(payload.into_options()?);
            }
            "watermark" => {
                if watermark_range.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `watermark` fields",
                    ));
                }
                if part.body_range.is_empty() {
                    return Err(bad_request_response(
                        "multipart upload `watermark` field must not be empty",
                    ));
                }
                let watermark_size = part.body_range.len() as u64;
                if watermark_size > super::remote::MAX_WATERMARK_BYTES {
                    return Err(bad_request_response(
                        "multipart upload `watermark` field exceeds the 10 MB size limit",
                    ));
                }
                watermark_range = Some(part.body_range);
            }
            "watermark_position" => {
                if watermark_position.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `watermark_position` fields",
                    ));
                }
                let text = std::str::from_utf8(&body[part.body_range])
                    .map_err(|_| bad_request_response("watermark_position must be valid UTF-8"))?;
                watermark_position = Some(text.trim().to_string());
            }
            "watermark_opacity" => {
                if watermark_opacity.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `watermark_opacity` fields",
                    ));
                }
                let text = std::str::from_utf8(&body[part.body_range])
                    .map_err(|_| bad_request_response("watermark_opacity must be valid UTF-8"))?;
                watermark_opacity =
                    Some(text.trim().parse::<u8>().map_err(|_| {
                        bad_request_response("watermark_opacity must be an integer")
                    })?);
            }
            "watermark_margin" => {
                if watermark_margin.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `watermark_margin` fields",
                    ));
                }
                let text = std::str::from_utf8(&body[part.body_range])
                    .map_err(|_| bad_request_response("watermark_margin must be valid UTF-8"))?;
                watermark_margin =
                    Some(text.trim().parse::<u32>().map_err(|_| {
                        bad_request_response("watermark_margin must be an integer")
                    })?);
            }
            field_name => {
                return Err(bad_request_response(&format!(
                    "multipart upload contains an unsupported field `{field_name}`"
                )));
            }
        }
    }

    let file_range = file_range
        .ok_or_else(|| bad_request_response("multipart upload requires a `file` field"))?;

    let has_orphaned_watermark_params =
        watermark_position.is_some() || watermark_opacity.is_some() || watermark_margin.is_some();
    let watermark = if let Some(wm_range) = watermark_range {
        Some(super::handler::resolve_multipart_watermark(
            body[wm_range].to_vec(),
            watermark_position,
            watermark_opacity,
            watermark_margin,
        )?)
    } else if has_orphaned_watermark_params {
        return Err(bad_request_response(
            "watermark_position, watermark_opacity, and watermark_margin require a `watermark` file field",
        ));
    } else {
        None
    };

    Ok((
        body[file_range].to_vec(),
        options.unwrap_or_default(),
        watermark,
    ))
}

pub(super) fn parse_multipart_boundary(request: &HttpRequest) -> Result<String, HttpResponse> {
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

pub(super) fn parse_multipart_form_data(
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

        let body_end = find_valid_boundary(&body[cursor..], &delimiter).ok_or_else(|| {
            bad_request_response("multipart part is missing the next boundary delimiter")
        })?;
        let body_range = cursor..(cursor + body_end);
        let part_name = parse_multipart_part_name(&headers)?;
        let content_type = header_value(&headers, "content-type").map(str::to_string);
        parts.push(MultipartPart {
            name: part_name,
            content_type,
            body_range,
        });

        cursor += body_end + 2;
    }

    Ok(parts)
}

pub(super) fn parse_part_headers(
    header_bytes: &[u8],
) -> Result<Vec<(String, String)>, HttpResponse> {
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| bad_request_response("multipart part headers must be valid UTF-8"))?;
    parse_headers(header_text.split("\r\n"))
}

pub(super) fn parse_multipart_part_name(
    headers: &[(String, String)],
) -> Result<String, HttpResponse> {
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

#[cfg(test)]
mod tests {
    use super::super::http_parse::HttpRequest;
    use super::*;

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    fn make_request(content_type: Option<&str>) -> HttpRequest {
        let mut headers = Vec::new();
        if let Some(ct) = content_type {
            headers.push(("content-type".to_string(), ct.to_string()));
        }
        HttpRequest {
            method: "POST".to_string(),
            target: "/upload".to_string(),
            version: "HTTP/1.1".to_string(),
            headers,
            body: Vec::new(),
        }
    }

    /// Builds a raw multipart body from parts.
    /// Each entry is (name, content_type (optional), body bytes).
    fn build_multipart_body(boundary: &str, parts: &[(&str, Option<&str>, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        for (name, ct, body) in parts {
            buf.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            buf.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
            );
            if let Some(ct) = ct {
                buf.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
            }
            buf.extend_from_slice(b"\r\n");
            buf.extend_from_slice(body);
            buf.extend_from_slice(b"\r\n");
        }
        buf.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        buf
    }

    // ---------------------------------------------------------------------------
    // parse_multipart_boundary
    // ---------------------------------------------------------------------------

    #[test]
    fn test_parse_boundary_valid() {
        let req = make_request(Some("multipart/form-data; boundary=abc123"));
        let boundary = parse_multipart_boundary(&req).unwrap();
        assert_eq!(boundary, "abc123");
    }

    #[test]
    fn test_parse_boundary_quoted() {
        let req = make_request(Some("multipart/form-data; boundary=\"abc123\""));
        let boundary = parse_multipart_boundary(&req).unwrap();
        assert_eq!(boundary, "abc123");
    }

    #[test]
    fn test_parse_boundary_case_insensitive_media_type() {
        let req = make_request(Some("Multipart/Form-Data; boundary=xyz"));
        let boundary = parse_multipart_boundary(&req).unwrap();
        assert_eq!(boundary, "xyz");
    }

    #[test]
    fn test_parse_boundary_case_insensitive_param_name() {
        let req = make_request(Some("multipart/form-data; Boundary=xyz"));
        let boundary = parse_multipart_boundary(&req).unwrap();
        assert_eq!(boundary, "xyz");
    }

    #[test]
    fn test_parse_boundary_missing_content_type() {
        let req = make_request(None);
        let err = parse_multipart_boundary(&req).unwrap_err();
        assert_eq!(err.status, "415 Unsupported Media Type");
    }

    #[test]
    fn test_parse_boundary_wrong_media_type() {
        let req = make_request(Some("application/json"));
        let err = parse_multipart_boundary(&req).unwrap_err();
        assert_eq!(err.status, "415 Unsupported Media Type");
    }

    #[test]
    fn test_parse_boundary_missing_boundary_param() {
        let req = make_request(Some("multipart/form-data; charset=utf-8"));
        let err = parse_multipart_boundary(&req).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_boundary_empty_boundary() {
        let req = make_request(Some("multipart/form-data; boundary="));
        let err = parse_multipart_boundary(&req).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_boundary_empty_quoted_boundary() {
        let req = make_request(Some("multipart/form-data; boundary=\"\""));
        let err = parse_multipart_boundary(&req).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_boundary_malformed_param_no_equals() {
        let req = make_request(Some("multipart/form-data; boundary"));
        let err = parse_multipart_boundary(&req).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ---------------------------------------------------------------------------
    // parse_part_headers
    // ---------------------------------------------------------------------------

    #[test]
    fn test_parse_part_headers_valid() {
        let input = b"Content-Disposition: form-data; name=\"file\"\r\nContent-Type: image/png";
        let headers = parse_part_headers(input).unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0, "content-disposition");
        assert_eq!(headers[1].0, "content-type");
        assert_eq!(headers[1].1, "image/png");
    }

    #[test]
    fn test_parse_part_headers_empty() {
        let headers = parse_part_headers(b"").unwrap();
        assert!(headers.is_empty());
    }

    #[test]
    fn test_parse_part_headers_invalid_utf8() {
        let input: &[u8] = &[0xff, 0xfe, 0xfd];
        let err = parse_part_headers(input).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ---------------------------------------------------------------------------
    // parse_multipart_part_name
    // ---------------------------------------------------------------------------

    #[test]
    fn test_parse_part_name_valid() {
        let headers = vec![(
            "content-disposition".to_string(),
            "form-data; name=\"file\"".to_string(),
        )];
        let name = parse_multipart_part_name(&headers).unwrap();
        assert_eq!(name, "file");
    }

    #[test]
    fn test_parse_part_name_unquoted() {
        let headers = vec![(
            "content-disposition".to_string(),
            "form-data; name=file".to_string(),
        )];
        let name = parse_multipart_part_name(&headers).unwrap();
        assert_eq!(name, "file");
    }

    #[test]
    fn test_parse_part_name_missing_disposition() {
        let headers = vec![("content-type".to_string(), "image/png".to_string())];
        let err = parse_multipart_part_name(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_part_name_not_form_data() {
        let headers = vec![(
            "content-disposition".to_string(),
            "attachment; name=\"file\"".to_string(),
        )];
        let err = parse_multipart_part_name(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_part_name_missing_name_param() {
        let headers = vec![(
            "content-disposition".to_string(),
            "form-data; filename=\"test.png\"".to_string(),
        )];
        let err = parse_multipart_part_name(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_part_name_empty_name() {
        let headers = vec![(
            "content-disposition".to_string(),
            "form-data; name=\"\"".to_string(),
        )];
        let err = parse_multipart_part_name(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_part_name_malformed_param() {
        let headers = vec![(
            "content-disposition".to_string(),
            "form-data; name".to_string(),
        )];
        let err = parse_multipart_part_name(&headers).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    // ---------------------------------------------------------------------------
    // parse_multipart_form_data
    // ---------------------------------------------------------------------------

    #[test]
    fn test_parse_form_data_single_part() {
        let boundary = "BOUNDARY";
        let body = build_multipart_body(boundary, &[("file", Some("image/png"), b"PNG_DATA")]);
        let parts = parse_multipart_form_data(&body, boundary).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "file");
        assert_eq!(parts[0].content_type.as_deref(), Some("image/png"));
        assert_eq!(&body[parts[0].body_range.clone()], b"PNG_DATA");
    }

    #[test]
    fn test_parse_form_data_multiple_parts() {
        let boundary = "----WebKitBoundary";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/jpeg"), b"\xff\xd8\xff"),
                ("options", Some("application/json"), b"{\"width\":100}"),
            ],
        );
        let parts = parse_multipart_form_data(&body, boundary).unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "file");
        assert_eq!(&body[parts[0].body_range.clone()], b"\xff\xd8\xff");
        assert_eq!(parts[1].name, "options");
        assert_eq!(&body[parts[1].body_range.clone()], b"{\"width\":100}",);
    }

    #[test]
    fn test_parse_form_data_empty_body_part() {
        let boundary = "bnd";
        let body = build_multipart_body(boundary, &[("file", None, b"")]);
        let parts = parse_multipart_form_data(&body, boundary).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].body_range.is_empty());
    }

    #[test]
    fn test_parse_form_data_no_content_type() {
        let boundary = "bnd";
        let body = build_multipart_body(boundary, &[("field", None, b"value")]);
        let parts = parse_multipart_form_data(&body, boundary).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].content_type.is_none());
    }

    #[test]
    fn test_parse_form_data_does_not_start_with_boundary() {
        let body = b"garbage\r\n--bnd\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\nval\r\n--bnd--\r\n";
        let err = parse_multipart_form_data(body, "bnd").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_form_data_missing_header_terminator() {
        let body = b"--bnd\r\nContent-Disposition: form-data; name=\"x\"";
        let err = parse_multipart_form_data(body, "bnd").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_form_data_missing_closing_boundary() {
        let body = b"--bnd\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\ndata";
        let err = parse_multipart_form_data(body, "bnd").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_form_data_trailing_data_after_close() {
        // Build body with trailing garbage after the closing boundary.
        let body =
            b"--bnd\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\nval\r\n--bnd--GARBAGE";
        let err = parse_multipart_form_data(body, "bnd").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_form_data_no_crlf_after_boundary() {
        let body = b"--bndContent-Disposition: form-data; name=\"x\"\r\n\r\nval\r\n--bnd--\r\n";
        let err = parse_multipart_form_data(body, "bnd").unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_parse_form_data_boundary_inside_binary_payload() {
        // The boundary string appears inside the payload but is NOT followed
        // by \r\n or --, so the parser must not treat it as a real delimiter.
        let boundary = "bnd";
        let payload = b"some\r\n--bndNOT_A_REAL_BOUNDARY data";
        let body = build_multipart_body(boundary, &[("file", None, payload)]);
        let parts = parse_multipart_form_data(&body, boundary).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(&body[parts[0].body_range.clone()], payload);
    }

    #[test]
    fn test_parse_form_data_only_closing_boundary() {
        // A body that starts with the closing boundary (no parts at all).
        let body = b"--bnd--\r\n";
        let parts = parse_multipart_form_data(body, "bnd").unwrap();
        assert!(parts.is_empty());
    }

    // ---------------------------------------------------------------------------
    // parse_upload_request
    // ---------------------------------------------------------------------------

    #[test]
    fn test_upload_request_file_only() {
        let boundary = "b";
        let file_bytes = b"FAKE_IMAGE_DATA";
        let body = build_multipart_body(boundary, &[("file", Some("image/png"), file_bytes)]);
        let (data, opts, watermark) = parse_upload_request(&body, boundary).unwrap();
        assert_eq!(data, file_bytes);
        assert_eq!(opts, TransformOptions::default());
        assert!(watermark.is_none());
    }

    #[test]
    fn test_upload_request_file_with_options() {
        let boundary = "b";
        let options_json = br#"{"width":200,"height":100}"#;
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("options", Some("application/json"), options_json),
            ],
        );
        let (data, opts, _) = parse_upload_request(&body, boundary).unwrap();
        assert_eq!(data, b"IMG");
        assert_eq!(opts.width, Some(200));
        assert_eq!(opts.height, Some(100));
    }

    #[test]
    fn test_upload_request_file_with_empty_options() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("options", Some("application/json"), b""),
            ],
        );
        let (_, opts, _) = parse_upload_request(&body, boundary).unwrap();
        assert_eq!(opts, TransformOptions::default());
    }

    #[test]
    fn test_upload_request_missing_file() {
        let boundary = "b";
        let body = build_multipart_body(boundary, &[("options", Some("application/json"), b"{}")]);
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("file"));
    }

    #[test]
    fn test_upload_request_empty_file() {
        let boundary = "b";
        let body = build_multipart_body(boundary, &[("file", Some("image/png"), b"")]);
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("empty"));
    }

    #[test]
    fn test_upload_request_duplicate_file() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG1"),
                ("file", Some("image/png"), b"IMG2"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("multiple"));
    }

    #[test]
    fn test_upload_request_duplicate_options() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("options", Some("application/json"), b"{}"),
                ("options", Some("application/json"), b"{}"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("multiple"));
    }

    #[test]
    fn test_upload_request_options_wrong_content_type() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("options", Some("text/plain"), b"{}"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("application/json"));
    }

    #[test]
    fn test_upload_request_options_invalid_json() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("options", Some("application/json"), b"NOT JSON"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("JSON"));
    }

    #[test]
    fn test_upload_request_unsupported_field() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("unknown_field", None, b"value"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("unknown_field"));
    }

    #[test]
    fn test_upload_request_orphaned_watermark_position() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_position", None, b"center"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("watermark"));
    }

    #[test]
    fn test_upload_request_orphaned_watermark_opacity() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_opacity", None, b"50"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_upload_request_orphaned_watermark_margin() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_margin", None, b"10"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_upload_request_duplicate_watermark_position() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_position", None, b"center"),
                ("watermark_position", None, b"top-left"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("multiple"));
    }

    #[test]
    fn test_upload_request_duplicate_watermark_opacity() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_opacity", None, b"50"),
                ("watermark_opacity", None, b"75"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_upload_request_duplicate_watermark_margin() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_margin", None, b"10"),
                ("watermark_margin", None, b"20"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
    }

    #[test]
    fn test_upload_request_watermark_opacity_not_integer() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_opacity", None, b"abc"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("integer"));
    }

    #[test]
    fn test_upload_request_watermark_margin_not_integer() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark_margin", None, b"xyz"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("integer"));
    }

    #[test]
    fn test_upload_request_empty_watermark() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark", Some("image/png"), b""),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("empty"));
    }

    #[test]
    fn test_upload_request_duplicate_watermark() {
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("watermark", Some("image/png"), b"WM1"),
                ("watermark", Some("image/png"), b"WM2"),
            ],
        );
        let err = parse_upload_request(&body, boundary).unwrap_err();
        assert_eq!(err.status, "400 Bad Request");
        assert!(String::from_utf8_lossy(&err.body).contains("multiple"));
    }

    #[test]
    fn test_upload_request_options_no_content_type_is_ok() {
        // When options part has no Content-Type header, it should still be parsed as JSON.
        let boundary = "b";
        let body = build_multipart_body(
            boundary,
            &[
                ("file", Some("image/png"), b"IMG"),
                ("options", None, b"{\"width\":50}"),
            ],
        );
        let (_, opts, _) = parse_upload_request(&body, boundary).unwrap();
        assert_eq!(opts.width, Some(50));
    }
}
