use super::response::{HttpResponse, not_acceptable_response};
use crate::{Artifact, MediaType};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcceptRange {
    Exact(&'static str),
    TypeWildcard(&'static str),
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AcceptPreference {
    pub(super) range: AcceptRange,
    pub(super) q_millis: u16,
    pub(super) specificity: u8,
}

pub(super) fn negotiate_output_format(
    accept_header: Option<&str>,
    artifact: &Artifact,
) -> Result<Option<MediaType>, HttpResponse> {
    let Some(accept_header) = accept_header
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let (preferences, had_any_segment) = parse_accept_header(accept_header);
    if preferences.is_empty() {
        // The header contained segments but none were recognized image types
        // (e.g. Accept: application/json). This is an explicit mismatch.
        if had_any_segment {
            return Err(not_acceptable_response(
                "Accept does not allow any supported output media type",
            ));
        }
        return Ok(None);
    }

    let mut best_candidate = None;
    let mut best_q = 0_u16;
    let mut best_specificity = 0_u8;

    for candidate in preferred_output_media_types(artifact) {
        let (candidate_q, candidate_specificity) =
            match_accept_preferences(candidate, &preferences);
        if candidate_q > best_q
            || (candidate_q == best_q && candidate_specificity > best_specificity)
        {
            best_q = candidate_q;
            best_specificity = candidate_specificity;
            best_candidate = Some(candidate);
        }
    }

    if best_q == 0 {
        return Err(not_acceptable_response(
            "Accept does not allow any supported output media type",
        ));
    }

    Ok(best_candidate)
}

/// Parses Accept header segments. Returns `(recognized, had_any_segments)` where
/// `had_any_segments` is true if the header contained at least one parseable media range
/// (even if not recognized by this server). This distinction lets the caller differentiate
/// "empty/malformed header" from "explicit but unsupported types".
pub(super) fn parse_accept_header(value: &str) -> (Vec<AcceptPreference>, bool) {
    let mut preferences = Vec::new();
    let mut had_any_segment = false;
    for segment in value.split(',') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        had_any_segment = true;
        if let Some(pref) = parse_accept_segment(segment) {
            preferences.push(pref);
        }
    }
    (preferences, had_any_segment)
}

pub(super) fn parse_accept_segment(segment: &str) -> Option<AcceptPreference> {
    if segment.is_empty() {
        return None;
    }

    let mut parts = segment.split(';');
    let media_range = parts.next()?.trim().to_ascii_lowercase();
    let (range, specificity) = parse_accept_range(&media_range)?;
    let mut q_millis = 1000_u16;

    for parameter in parts {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("q") {
            q_millis = parse_accept_qvalue(value.trim())?;
        }
    }

    Some(AcceptPreference {
        range,
        q_millis,
        specificity,
    })
}

pub(super) fn parse_accept_range(value: &str) -> Option<(AcceptRange, u8)> {
    match value {
        "*/*" => Some((AcceptRange::Any, 0)),
        "image/*" => Some((AcceptRange::TypeWildcard("image"), 1)),
        "image/jpeg" => Some((AcceptRange::Exact("image/jpeg"), 2)),
        "image/png" => Some((AcceptRange::Exact("image/png"), 2)),
        "image/webp" => Some((AcceptRange::Exact("image/webp"), 2)),
        "image/avif" => Some((AcceptRange::Exact("image/avif"), 2)),
        "image/bmp" => Some((AcceptRange::Exact("image/bmp"), 2)),
        "image/svg+xml" => Some((AcceptRange::Exact("image/svg+xml"), 2)),
        _ => None,
    }
}

pub(super) fn parse_accept_qvalue(value: &str) -> Option<u16> {
    let parsed = value.parse::<f32>().ok()?;
    if !(0.0..=1.0).contains(&parsed) {
        return None;
    }

    Some((parsed * 1000.0).round() as u16)
}

/// Returns the list of candidate output media types for Accept negotiation,
/// ordered by server preference.
///
/// The input format is always included so that "preserve the input format"
/// is a valid negotiation outcome (matching the OpenAPI spec). SVG is included
/// when the input is SVG.
pub(super) fn preferred_output_media_types(artifact: &Artifact) -> Vec<MediaType> {
    let base: &[MediaType] = if artifact.metadata.has_alpha == Some(true) {
        &[
            MediaType::Avif,
            MediaType::Webp,
            MediaType::Png,
            MediaType::Jpeg,
        ]
    } else {
        &[
            MediaType::Avif,
            MediaType::Webp,
            MediaType::Jpeg,
            MediaType::Png,
        ]
    };

    let input = artifact.media_type;
    if base.contains(&input) {
        base.to_vec()
    } else {
        // Input format (e.g. SVG, BMP) is not in the base list -- prepend it
        // so the client can request the original format via Accept negotiation.
        let mut candidates = vec![input];
        candidates.extend_from_slice(base);
        candidates
    }
}

pub(super) fn match_accept_preferences(
    media_type: MediaType,
    preferences: &[AcceptPreference],
) -> (u16, u8) {
    let mut best_q = 0_u16;
    let mut best_specificity = 0_u8;

    for preference in preferences {
        if accept_range_matches(preference.range, media_type)
            && (preference.q_millis > best_q
                || (preference.q_millis == best_q && preference.specificity > best_specificity))
        {
            best_q = preference.q_millis;
            best_specificity = preference.specificity;
        }
    }

    (best_q, best_specificity)
}

pub(super) fn accept_range_matches(range: AcceptRange, media_type: MediaType) -> bool {
    match range {
        AcceptRange::Exact(expected) => media_type.as_mime() == expected,
        AcceptRange::TypeWildcard(expected_type) => media_type
            .as_mime()
            .split('/')
            .next()
            .is_some_and(|actual_type| actual_type == expected_type),
        AcceptRange::Any => true,
    }
}

pub(super) fn build_image_etag(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    format!("\"sha256-{}\"", hex::encode(digest))
}

/// Whether a transform response was served from the cache or freshly computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheHitStatus {
    /// The response was served from the on-disk transform cache.
    Hit,
    /// The transform was freshly computed (no cache entry or stale).
    Miss,
    /// No cache is configured.
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImageResponsePolicy {
    PublicGet,
    PrivateTransform,
}

pub(super) fn build_image_response_headers(
    media_type: MediaType,
    etag: &str,
    response_policy: ImageResponsePolicy,
    negotiation_used: bool,
    cache_status: CacheHitStatus,
    public_max_age: u32,
    public_swr: u32,
) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "Cache-Control".to_string(),
            match response_policy {
                ImageResponsePolicy::PublicGet => {
                    format!("public, max-age={public_max_age}, stale-while-revalidate={public_swr}")
                }
                ImageResponsePolicy::PrivateTransform => "no-store".to_string(),
            },
        ),
        ("ETag".to_string(), etag.to_string()),
        ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
        (
            "Content-Disposition".to_string(),
            format!("inline; filename=\"truss.{}\"", media_type.as_name()),
        ),
    ];

    if negotiation_used {
        headers.push(("Vary".to_string(), "Accept".to_string()));
    }

    // SVG outputs get a Content-Security-Policy sandbox to prevent script execution
    // when served inline. This mitigates XSS risk from user-supplied SVG content.
    if media_type == MediaType::Svg {
        headers.push(("Content-Security-Policy".to_string(), "sandbox".to_string()));
    }

    // Cache-Status per RFC 9211.
    let cache_status_value = match cache_status {
        CacheHitStatus::Hit => "\"truss\"; hit".to_string(),
        CacheHitStatus::Miss | CacheHitStatus::Disabled => "\"truss\"; fwd=miss".to_string(),
    };
    headers.push(("Cache-Status".to_string(), cache_status_value));

    headers
}

pub(super) fn if_none_match_matches(value: Option<&str>, etag: &str) -> bool {
    let Some(value) = value else {
        return false;
    };

    value.split(',').map(str::trim).any(|candidate| {
        if candidate == "*" {
            return true;
        }
        let normalized = candidate
            .strip_prefix("W/")
            .unwrap_or(candidate)
            .trim()
            .trim_matches('"');
        let expected = etag.trim_matches('"');
        normalized == expected
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicSourceKind {
    Path,
    Url,
}
