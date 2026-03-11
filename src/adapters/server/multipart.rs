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
                let text = std::str::from_utf8(&body[part.body_range])
                    .map_err(|_| bad_request_response("watermark_position must be valid UTF-8"))?;
                watermark_position = Some(text.trim().to_string());
            }
            "watermark_opacity" => {
                let text = std::str::from_utf8(&body[part.body_range])
                    .map_err(|_| bad_request_response("watermark_opacity must be valid UTF-8"))?;
                watermark_opacity =
                    Some(text.trim().parse::<u8>().map_err(|_| {
                        bad_request_response("watermark_opacity must be an integer")
                    })?);
            }
            "watermark_margin" => {
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
        Some(super::resolve_multipart_watermark(
            body[wm_range].to_vec(),
            watermark_position,
            watermark_opacity,
            watermark_margin,
        )?)
    } else if has_orphaned_watermark_params {
        return Err(super::bad_request_response(
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
