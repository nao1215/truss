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
    format_preference: &[MediaType],
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

    for candidate in preferred_output_media_types(artifact, format_preference) {
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
/// When `format_preference` is non-empty, it overrides the built-in default
/// order. Formats present in `format_preference` come first (in the given
/// order), followed by any remaining built-in formats not already listed.
///
/// The input format is always included so that "preserve the input format"
/// is a valid negotiation outcome (matching the OpenAPI spec). SVG is included
/// when the input is SVG.
pub(super) fn preferred_output_media_types(
    artifact: &Artifact,
    format_preference: &[MediaType],
) -> Vec<MediaType> {
    let default_base: &[MediaType] = if artifact.metadata.has_alpha == Some(true) {
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

    let base: Vec<MediaType> = if format_preference.is_empty() {
        default_base.to_vec()
    } else {
        // Start with formats from the preference list that are in the default set,
        // then append any remaining default formats not already covered.
        let mut ordered = Vec::new();
        for &mt in format_preference {
            if default_base.contains(&mt) && !ordered.contains(&mt) {
                ordered.push(mt);
            }
        }
        for &mt in default_base {
            if !ordered.contains(&mt) {
                ordered.push(mt);
            }
        }
        ordered
    };

    let input = artifact.media_type;
    if base.contains(&input) {
        base
    } else {
        // Input format (e.g. SVG, BMP) is not in the base list -- prepend it
        // so the client can request the original format via Accept negotiation.
        let mut candidates = vec![input];
        candidates.extend_from_slice(&base);
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

#[allow(clippy::too_many_arguments)]
pub(super) fn build_image_response_headers(
    media_type: MediaType,
    etag: &str,
    response_policy: ImageResponsePolicy,
    negotiation_used: bool,
    cache_status: CacheHitStatus,
    public_max_age: u32,
    public_swr: u32,
    custom_headers: &[(String, String)],
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

    // Operator-configured custom headers (e.g. CDN-specific cache directives).
    for (name, value) in custom_headers {
        headers.push((name.clone(), value.clone()));
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Artifact, ArtifactMetadata, MediaType};

    fn make_artifact(media_type: MediaType, has_alpha: Option<bool>) -> Artifact {
        Artifact::new(
            vec![0xFF, 0xD8, 0xFF],
            media_type,
            ArtifactMetadata {
                has_alpha,
                ..Default::default()
            },
        )
    }

    // ── parse_accept_qvalue ──────────────────────────────────────────

    #[test]
    fn test_parse_accept_qvalue_one() {
        assert_eq!(parse_accept_qvalue("1"), Some(1000));
    }

    #[test]
    fn test_parse_accept_qvalue_one_point_zero() {
        assert_eq!(parse_accept_qvalue("1.0"), Some(1000));
    }

    #[test]
    fn test_parse_accept_qvalue_zero() {
        assert_eq!(parse_accept_qvalue("0"), Some(0));
    }

    #[test]
    fn test_parse_accept_qvalue_zero_point_zero() {
        assert_eq!(parse_accept_qvalue("0.0"), Some(0));
    }

    #[test]
    fn test_parse_accept_qvalue_mid_range() {
        assert_eq!(parse_accept_qvalue("0.5"), Some(500));
    }

    #[test]
    fn test_parse_accept_qvalue_three_decimal_places() {
        assert_eq!(parse_accept_qvalue("0.123"), Some(123));
    }

    #[test]
    fn test_parse_accept_qvalue_one_decimal_place() {
        assert_eq!(parse_accept_qvalue("0.9"), Some(900));
    }

    #[test]
    fn test_parse_accept_qvalue_above_one_rejected() {
        assert_eq!(parse_accept_qvalue("1.1"), None);
    }

    #[test]
    fn test_parse_accept_qvalue_negative_rejected() {
        assert_eq!(parse_accept_qvalue("-0.5"), None);
    }

    #[test]
    fn test_parse_accept_qvalue_non_numeric_rejected() {
        assert_eq!(parse_accept_qvalue("abc"), None);
    }

    #[test]
    fn test_parse_accept_qvalue_empty_rejected() {
        assert_eq!(parse_accept_qvalue(""), None);
    }

    #[test]
    fn test_parse_accept_qvalue_large_number_rejected() {
        assert_eq!(parse_accept_qvalue("999"), None);
    }

    // ── parse_accept_segment ─────────────────────────────────────────

    #[test]
    fn test_parse_accept_segment_simple_jpeg() {
        let pref = parse_accept_segment("image/jpeg").unwrap();
        assert_eq!(pref.range, AcceptRange::Exact("image/jpeg"));
        assert_eq!(pref.q_millis, 1000);
        assert_eq!(pref.specificity, 2);
    }

    #[test]
    fn test_parse_accept_segment_with_qvalue() {
        let pref = parse_accept_segment("image/webp;q=0.8").unwrap();
        assert_eq!(pref.range, AcceptRange::Exact("image/webp"));
        assert_eq!(pref.q_millis, 800);
    }

    #[test]
    fn test_parse_accept_segment_with_spaces_around_qvalue() {
        let pref = parse_accept_segment("image/png ; q = 0.5").unwrap();
        assert_eq!(pref.range, AcceptRange::Exact("image/png"));
        assert_eq!(pref.q_millis, 500);
    }

    #[test]
    fn test_parse_accept_segment_wildcard() {
        let pref = parse_accept_segment("*/*").unwrap();
        assert_eq!(pref.range, AcceptRange::Any);
        assert_eq!(pref.specificity, 0);
        assert_eq!(pref.q_millis, 1000);
    }

    #[test]
    fn test_parse_accept_segment_type_wildcard() {
        let pref = parse_accept_segment("image/*").unwrap();
        assert_eq!(pref.range, AcceptRange::TypeWildcard("image"));
        assert_eq!(pref.specificity, 1);
    }

    #[test]
    fn test_parse_accept_segment_unknown_type_returns_none() {
        assert!(parse_accept_segment("application/json").is_none());
    }

    #[test]
    fn test_parse_accept_segment_empty_returns_none() {
        assert!(parse_accept_segment("").is_none());
    }

    #[test]
    fn test_parse_accept_segment_invalid_qvalue_returns_none() {
        assert!(parse_accept_segment("image/jpeg;q=2.0").is_none());
    }

    #[test]
    fn test_parse_accept_segment_case_insensitive() {
        let pref = parse_accept_segment("IMAGE/JPEG").unwrap();
        assert_eq!(pref.range, AcceptRange::Exact("image/jpeg"));
    }

    #[test]
    fn test_parse_accept_segment_svg() {
        let pref = parse_accept_segment("image/svg+xml").unwrap();
        assert_eq!(pref.range, AcceptRange::Exact("image/svg+xml"));
        assert_eq!(pref.specificity, 2);
    }

    #[test]
    fn test_parse_accept_segment_parameter_without_equals_ignored() {
        // A parameter like ";level=1" without q= should not break parsing
        let pref = parse_accept_segment("image/jpeg;level").unwrap();
        assert_eq!(pref.q_millis, 1000);
    }

    // ── parse_accept_header ──────────────────────────────────────────

    #[test]
    fn test_parse_accept_header_single_type() {
        let (prefs, had_any) = parse_accept_header("image/png");
        assert!(had_any);
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].range, AcceptRange::Exact("image/png"));
    }

    #[test]
    fn test_parse_accept_header_multiple_types() {
        let (prefs, had_any) = parse_accept_header("image/webp, image/jpeg;q=0.9, */*;q=0.1");
        assert!(had_any);
        assert_eq!(prefs.len(), 3);
    }

    #[test]
    fn test_parse_accept_header_empty_string() {
        let (prefs, had_any) = parse_accept_header("");
        assert!(!had_any);
        assert!(prefs.is_empty());
    }

    #[test]
    fn test_parse_accept_header_only_unknown_types() {
        let (prefs, had_any) = parse_accept_header("application/json, text/html");
        assert!(had_any);
        assert!(prefs.is_empty());
    }

    #[test]
    fn test_parse_accept_header_trailing_comma() {
        let (prefs, had_any) = parse_accept_header("image/jpeg,");
        assert!(had_any);
        assert_eq!(prefs.len(), 1);
    }

    #[test]
    fn test_parse_accept_header_multiple_commas() {
        let (prefs, had_any) = parse_accept_header("image/jpeg,,image/png");
        assert!(had_any);
        assert_eq!(prefs.len(), 2);
    }

    #[test]
    fn test_parse_accept_header_mixed_known_unknown() {
        let (prefs, had_any) = parse_accept_header("text/html, image/avif;q=0.8, application/xml");
        assert!(had_any);
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].range, AcceptRange::Exact("image/avif"));
        assert_eq!(prefs[0].q_millis, 800);
    }

    // ── accept_range_matches ─────────────────────────────────────────

    #[test]
    fn test_accept_range_matches_exact_hit() {
        assert!(accept_range_matches(
            AcceptRange::Exact("image/jpeg"),
            MediaType::Jpeg
        ));
    }

    #[test]
    fn test_accept_range_matches_exact_miss() {
        assert!(!accept_range_matches(
            AcceptRange::Exact("image/jpeg"),
            MediaType::Png
        ));
    }

    #[test]
    fn test_accept_range_matches_type_wildcard_hit() {
        assert!(accept_range_matches(
            AcceptRange::TypeWildcard("image"),
            MediaType::Webp
        ));
    }

    #[test]
    fn test_accept_range_matches_any_matches_all() {
        assert!(accept_range_matches(AcceptRange::Any, MediaType::Jpeg));
        assert!(accept_range_matches(AcceptRange::Any, MediaType::Svg));
        assert!(accept_range_matches(AcceptRange::Any, MediaType::Bmp));
        assert!(accept_range_matches(AcceptRange::Any, MediaType::Avif));
    }

    #[test]
    fn test_accept_range_matches_exact_svg() {
        assert!(accept_range_matches(
            AcceptRange::Exact("image/svg+xml"),
            MediaType::Svg
        ));
    }

    #[test]
    fn test_accept_range_matches_exact_bmp() {
        assert!(accept_range_matches(
            AcceptRange::Exact("image/bmp"),
            MediaType::Bmp
        ));
    }

    // ── if_none_match_matches ────────────────────────────────────────

    #[test]
    fn test_if_none_match_matches_exact() {
        assert!(if_none_match_matches(
            Some("\"sha256-abc123\""),
            "\"sha256-abc123\""
        ));
    }

    #[test]
    fn test_if_none_match_matches_none_header() {
        assert!(!if_none_match_matches(None, "\"sha256-abc123\""));
    }

    #[test]
    fn test_if_none_match_matches_wildcard() {
        assert!(if_none_match_matches(Some("*"), "\"sha256-abc123\""));
    }

    #[test]
    fn test_if_none_match_matches_weak_etag() {
        assert!(if_none_match_matches(
            Some("W/\"sha256-abc123\""),
            "\"sha256-abc123\""
        ));
    }

    #[test]
    fn test_if_none_match_matches_multiple_etags() {
        assert!(if_none_match_matches(
            Some("\"sha256-other\", \"sha256-abc123\""),
            "\"sha256-abc123\""
        ));
    }

    #[test]
    fn test_if_none_match_no_match() {
        assert!(!if_none_match_matches(
            Some("\"sha256-different\""),
            "\"sha256-abc123\""
        ));
    }

    #[test]
    fn test_if_none_match_matches_weak_among_multiple() {
        assert!(if_none_match_matches(
            Some("\"sha256-x\", W/\"sha256-target\", \"sha256-y\""),
            "\"sha256-target\""
        ));
    }

    #[test]
    fn test_if_none_match_empty_value() {
        assert!(!if_none_match_matches(Some(""), "\"sha256-abc\""));
    }

    #[test]
    fn test_if_none_match_wildcard_among_others() {
        assert!(if_none_match_matches(
            Some("\"sha256-x\", *"),
            "\"sha256-anything\""
        ));
    }

    // ── build_image_etag ─────────────────────────────────────────────

    #[test]
    fn test_build_image_etag_deterministic() {
        let body = b"hello world";
        let etag1 = build_image_etag(body);
        let etag2 = build_image_etag(body);
        assert_eq!(etag1, etag2);
    }

    #[test]
    fn test_build_image_etag_format() {
        let etag = build_image_etag(b"test");
        assert!(etag.starts_with("\"sha256-"));
        assert!(etag.ends_with('"'));
    }

    #[test]
    fn test_build_image_etag_different_bodies_differ() {
        let etag1 = build_image_etag(b"body1");
        let etag2 = build_image_etag(b"body2");
        assert_ne!(etag1, etag2);
    }

    #[test]
    fn test_build_image_etag_empty_body() {
        let etag = build_image_etag(b"");
        assert!(etag.starts_with("\"sha256-"));
        // SHA-256 of empty input is a known hash
        assert!(etag.contains("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
    }

    // ── build_image_response_headers ─────────────────────────────────

    #[test]
    fn test_build_image_response_headers_public_get() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            3600,
            86400,
            &[],
        );
        let cache_control = headers.iter().find(|(k, _)| *k == "Cache-Control").unwrap();
        assert_eq!(
            cache_control.1,
            "public, max-age=3600, stale-while-revalidate=86400"
        );
    }

    #[test]
    fn test_build_image_response_headers_private_transform() {
        let headers = build_image_response_headers(
            MediaType::Png,
            "\"etag\"",
            ImageResponsePolicy::PrivateTransform,
            false,
            CacheHitStatus::Disabled,
            0,
            0,
            &[],
        );
        let cache_control = headers.iter().find(|(k, _)| *k == "Cache-Control").unwrap();
        assert_eq!(cache_control.1, "no-store");
    }

    #[test]
    fn test_build_image_response_headers_vary_when_negotiated() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            true,
            CacheHitStatus::Hit,
            60,
            120,
            &[],
        );
        let vary = headers.iter().find(|(k, _)| *k == "Vary");
        assert!(vary.is_some());
        assert_eq!(vary.unwrap().1, "Accept");
    }

    #[test]
    fn test_build_image_response_headers_no_vary_when_not_negotiated() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            60,
            120,
            &[],
        );
        let vary = headers.iter().find(|(k, _)| *k == "Vary");
        assert!(vary.is_none());
    }

    #[test]
    fn test_build_image_response_headers_svg_has_csp() {
        let headers = build_image_response_headers(
            MediaType::Svg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            60,
            120,
            &[],
        );
        let csp = headers
            .iter()
            .find(|(k, _)| *k == "Content-Security-Policy");
        assert!(csp.is_some());
        assert_eq!(csp.unwrap().1, "sandbox");
    }

    #[test]
    fn test_build_image_response_headers_non_svg_no_csp() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            60,
            120,
            &[],
        );
        let csp = headers
            .iter()
            .find(|(k, _)| *k == "Content-Security-Policy");
        assert!(csp.is_none());
    }

    #[test]
    fn test_build_image_response_headers_nosniff_always_present() {
        let headers = build_image_response_headers(
            MediaType::Webp,
            "\"etag\"",
            ImageResponsePolicy::PrivateTransform,
            false,
            CacheHitStatus::Disabled,
            0,
            0,
            &[],
        );
        let nosniff = headers.iter().find(|(k, _)| *k == "X-Content-Type-Options");
        assert!(nosniff.is_some());
        assert_eq!(nosniff.unwrap().1, "nosniff");
    }

    #[test]
    fn test_build_image_response_headers_content_disposition() {
        let headers = build_image_response_headers(
            MediaType::Avif,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            60,
            120,
            &[],
        );
        let cd = headers
            .iter()
            .find(|(k, _)| *k == "Content-Disposition")
            .unwrap();
        assert_eq!(cd.1, "inline; filename=\"truss.avif\"");
    }

    #[test]
    fn test_build_image_response_headers_cache_status_hit() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Hit,
            60,
            120,
            &[],
        );
        let cs = headers.iter().find(|(k, _)| *k == "Cache-Status").unwrap();
        assert_eq!(cs.1, "\"truss\"; hit");
    }

    #[test]
    fn test_build_image_response_headers_cache_status_miss() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            60,
            120,
            &[],
        );
        let cs = headers.iter().find(|(k, _)| *k == "Cache-Status").unwrap();
        assert_eq!(cs.1, "\"truss\"; fwd=miss");
    }

    #[test]
    fn test_build_image_response_headers_cache_status_disabled() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Disabled,
            60,
            120,
            &[],
        );
        let cs = headers.iter().find(|(k, _)| *k == "Cache-Status").unwrap();
        assert_eq!(cs.1, "\"truss\"; fwd=miss");
    }

    #[test]
    fn test_build_image_response_headers_custom_headers_appended() {
        let custom = vec![
            ("CDN-Cache-Control".to_string(), "max-age=86400".to_string()),
            ("Surrogate-Control".to_string(), "max-age=3600".to_string()),
        ];
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            3600,
            86400,
            &custom,
        );
        let cdn = headers
            .iter()
            .find(|(k, _)| *k == "CDN-Cache-Control")
            .unwrap();
        assert_eq!(cdn.1, "max-age=86400");
        let surrogate = headers
            .iter()
            .find(|(k, _)| *k == "Surrogate-Control")
            .unwrap();
        assert_eq!(surrogate.1, "max-age=3600");
    }

    #[test]
    fn test_build_image_response_headers_no_custom_headers() {
        let headers = build_image_response_headers(
            MediaType::Jpeg,
            "\"etag\"",
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            3600,
            86400,
            &[],
        );
        // Should not have CDN-specific headers.
        assert!(
            headers
                .iter()
                .find(|(k, _)| *k == "CDN-Cache-Control")
                .is_none()
        );
    }

    // ── negotiate_output_format ──────────────────────────────────────

    #[test]
    fn test_negotiate_output_format_none_accept() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(None, &artifact, &[]);
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_negotiate_output_format_empty_accept() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some(""), &artifact, &[]);
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_negotiate_output_format_whitespace_only_accept() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some("   "), &artifact, &[]);
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_negotiate_output_format_exact_match() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some("image/jpeg"), &artifact, &[]);
        assert_eq!(result.unwrap(), Some(MediaType::Jpeg));
    }

    #[test]
    fn test_negotiate_output_format_prefers_avif_over_jpeg() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some("image/avif, image/jpeg"), &artifact, &[]);
        // Both have q=1.0, but avif is server-preferred (first in the candidate list)
        assert_eq!(result.unwrap(), Some(MediaType::Avif));
    }

    #[test]
    fn test_negotiate_output_format_respects_client_q_preference() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some("image/avif;q=0.5, image/jpeg;q=1.0"), &artifact, &[]);
        // jpeg has higher q, so it should win
        assert_eq!(result.unwrap(), Some(MediaType::Jpeg));
    }

    #[test]
    fn test_negotiate_output_format_wildcard_fallback() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some("*/*"), &artifact, &[]);
        // Wildcard matches all; server-preferred (avif) should win
        assert_eq!(result.unwrap(), Some(MediaType::Avif));
    }

    #[test]
    fn test_negotiate_output_format_unsupported_type_returns_error() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some("application/json"), &artifact, &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_negotiate_output_format_q_zero_excluded() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        // All image types explicitly excluded with q=0
        let result = negotiate_output_format(
            Some("image/avif;q=0, image/webp;q=0, image/jpeg;q=0, image/png;q=0"),
            &artifact,
            &[],
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_negotiate_output_format_svg_input_available_in_candidates() {
        let artifact = make_artifact(MediaType::Svg, None);
        let result = negotiate_output_format(Some("image/svg+xml"), &artifact, &[]);
        assert_eq!(result.unwrap(), Some(MediaType::Svg));
    }

    #[test]
    fn test_negotiate_output_format_bmp_input_available_in_candidates() {
        let artifact = make_artifact(MediaType::Bmp, None);
        let result = negotiate_output_format(Some("image/bmp"), &artifact, &[]);
        assert_eq!(result.unwrap(), Some(MediaType::Bmp));
    }

    #[test]
    fn test_negotiate_output_format_image_wildcard() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let result = negotiate_output_format(Some("image/*"), &artifact, &[]);
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_negotiate_output_format_specificity_beats_wildcard() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        // image/jpeg explicit q=1.0 is more specific than */* q=1.0
        let result = negotiate_output_format(Some("*/*;q=0.1, image/jpeg;q=1.0"), &artifact, &[]);
        assert_eq!(result.unwrap(), Some(MediaType::Jpeg));
    }

    #[test]
    fn test_negotiate_output_format_alpha_prefers_png_over_jpeg() {
        let artifact = make_artifact(MediaType::Png, Some(true));
        // When has_alpha, server order is avif, webp, png, jpeg
        // If only png and jpeg are accepted, png should win
        let result = negotiate_output_format(Some("image/png, image/jpeg"), &artifact, &[]);
        assert_eq!(result.unwrap(), Some(MediaType::Png));
    }

    // ── preferred_output_media_types ─────────────────────────────────

    #[test]
    fn test_preferred_output_media_types_no_alpha() {
        let artifact = make_artifact(MediaType::Jpeg, Some(false));
        let types = preferred_output_media_types(&artifact, &[]);
        assert_eq!(
            types,
            vec![
                MediaType::Avif,
                MediaType::Webp,
                MediaType::Jpeg,
                MediaType::Png
            ]
        );
    }

    #[test]
    fn test_preferred_output_media_types_with_alpha() {
        let artifact = make_artifact(MediaType::Png, Some(true));
        let types = preferred_output_media_types(&artifact, &[]);
        assert_eq!(
            types,
            vec![
                MediaType::Avif,
                MediaType::Webp,
                MediaType::Png,
                MediaType::Jpeg
            ]
        );
    }

    #[test]
    fn test_preferred_output_media_types_svg_prepended() {
        let artifact = make_artifact(MediaType::Svg, None);
        let types = preferred_output_media_types(&artifact, &[]);
        assert_eq!(types[0], MediaType::Svg);
        assert!(types.len() == 5);
    }

    #[test]
    fn test_preferred_output_media_types_bmp_prepended() {
        let artifact = make_artifact(MediaType::Bmp, None);
        let types = preferred_output_media_types(&artifact, &[]);
        assert_eq!(types[0], MediaType::Bmp);
    }

    // ── match_accept_preferences ─────────────────────────────────────

    #[test]
    fn test_match_accept_preferences_no_match() {
        let prefs = vec![AcceptPreference {
            range: AcceptRange::Exact("image/png"),
            q_millis: 1000,
            specificity: 2,
        }];
        let (q, _) = match_accept_preferences(MediaType::Jpeg, &prefs);
        assert_eq!(q, 0);
    }

    #[test]
    fn test_match_accept_preferences_picks_highest_q() {
        let prefs = vec![
            AcceptPreference {
                range: AcceptRange::Any,
                q_millis: 100,
                specificity: 0,
            },
            AcceptPreference {
                range: AcceptRange::Exact("image/jpeg"),
                q_millis: 900,
                specificity: 2,
            },
        ];
        let (q, spec) = match_accept_preferences(MediaType::Jpeg, &prefs);
        assert_eq!(q, 900);
        assert_eq!(spec, 2);
    }

    #[test]
    fn test_match_accept_preferences_same_q_picks_higher_specificity() {
        let prefs = vec![
            AcceptPreference {
                range: AcceptRange::Any,
                q_millis: 1000,
                specificity: 0,
            },
            AcceptPreference {
                range: AcceptRange::Exact("image/jpeg"),
                q_millis: 1000,
                specificity: 2,
            },
        ];
        let (q, spec) = match_accept_preferences(MediaType::Jpeg, &prefs);
        assert_eq!(q, 1000);
        assert_eq!(spec, 2);
    }

    #[test]
    fn test_match_accept_preferences_empty_list() {
        let (q, spec) = match_accept_preferences(MediaType::Jpeg, &[]);
        assert_eq!(q, 0);
        assert_eq!(spec, 0);
    }

    // ── parse_accept_range ───────────────────────────────────────────

    #[test]
    fn test_parse_accept_range_all_supported_types() {
        let cases = vec![
            ("*/*", AcceptRange::Any, 0),
            ("image/*", AcceptRange::TypeWildcard("image"), 1),
            ("image/jpeg", AcceptRange::Exact("image/jpeg"), 2),
            ("image/png", AcceptRange::Exact("image/png"), 2),
            ("image/webp", AcceptRange::Exact("image/webp"), 2),
            ("image/avif", AcceptRange::Exact("image/avif"), 2),
            ("image/bmp", AcceptRange::Exact("image/bmp"), 2),
            ("image/svg+xml", AcceptRange::Exact("image/svg+xml"), 2),
        ];
        for (input, expected_range, expected_spec) in cases {
            let (range, spec) = parse_accept_range(input).unwrap();
            assert_eq!(range, expected_range, "failed for input: {input}");
            assert_eq!(spec, expected_spec, "failed specificity for input: {input}");
        }
    }

    #[test]
    fn test_parse_accept_range_unsupported_returns_none() {
        assert!(parse_accept_range("text/html").is_none());
        assert!(parse_accept_range("application/json").is_none());
        assert!(parse_accept_range("video/mp4").is_none());
        assert!(parse_accept_range("image/gif").is_none());
        assert!(parse_accept_range("image/tiff").is_none());
    }

    // ── format_preference ──────────────────────────────────────────────

    #[test]
    fn test_preferred_output_media_types_custom_preference_reorders() {
        let artifact = make_artifact(MediaType::Jpeg, Some(false));
        let pref = &[MediaType::Webp, MediaType::Jpeg, MediaType::Png, MediaType::Avif];
        let types = preferred_output_media_types(&artifact, pref);
        assert_eq!(
            types,
            vec![MediaType::Webp, MediaType::Jpeg, MediaType::Png, MediaType::Avif]
        );
    }

    #[test]
    fn test_preferred_output_media_types_partial_preference_appends_remaining() {
        let artifact = make_artifact(MediaType::Jpeg, Some(false));
        // Only specify webp; the rest should follow the default order.
        let pref = &[MediaType::Webp];
        let types = preferred_output_media_types(&artifact, pref);
        assert_eq!(types[0], MediaType::Webp);
        // Remaining default formats (avif, jpeg, png) follow.
        assert_eq!(types.len(), 4);
        assert!(types.contains(&MediaType::Avif));
        assert!(types.contains(&MediaType::Jpeg));
        assert!(types.contains(&MediaType::Png));
    }

    #[test]
    fn test_preferred_output_media_types_preference_with_alpha() {
        let artifact = make_artifact(MediaType::Png, Some(true));
        let pref = &[MediaType::Png, MediaType::Webp];
        let types = preferred_output_media_types(&artifact, pref);
        // Png should come first per preference, then webp, then remaining defaults.
        assert_eq!(types[0], MediaType::Png);
        assert_eq!(types[1], MediaType::Webp);
        assert_eq!(types.len(), 4);
    }

    #[test]
    fn test_negotiate_with_custom_preference_webp_first() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let pref = &[MediaType::Webp, MediaType::Avif, MediaType::Jpeg, MediaType::Png];
        // Both webp and avif have q=1.0, but webp is preferred by config.
        let result = negotiate_output_format(Some("image/avif, image/webp"), &artifact, pref);
        assert_eq!(result.unwrap(), Some(MediaType::Webp));
    }

    #[test]
    fn test_negotiate_with_custom_preference_jpeg_first() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let pref = &[MediaType::Jpeg, MediaType::Png];
        // Client accepts everything via wildcard; server prefers jpeg.
        let result = negotiate_output_format(Some("*/*"), &artifact, pref);
        assert_eq!(result.unwrap(), Some(MediaType::Jpeg));
    }

    #[test]
    fn test_negotiate_with_custom_preference_client_q_still_wins() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        let pref = &[MediaType::Webp, MediaType::Avif];
        // Server prefers webp, but client explicitly gives avif higher q.
        let result =
            negotiate_output_format(Some("image/webp;q=0.5, image/avif;q=1.0"), &artifact, pref);
        assert_eq!(result.unwrap(), Some(MediaType::Avif));
    }

    #[test]
    fn test_preferred_output_media_types_svg_input_with_preference() {
        let artifact = make_artifact(MediaType::Svg, None);
        let pref = &[MediaType::Webp, MediaType::Png];
        let types = preferred_output_media_types(&artifact, pref);
        // SVG input is prepended regardless of preference.
        assert_eq!(types[0], MediaType::Svg);
        // Then webp, png from preference, then remaining defaults.
        assert_eq!(types[1], MediaType::Webp);
        assert_eq!(types[2], MediaType::Png);
    }

    #[test]
    fn test_preferred_output_media_types_preference_ignores_non_default_formats() {
        let artifact = make_artifact(MediaType::Jpeg, None);
        // Svg and Bmp are not in the default base list for raster inputs.
        let pref = &[MediaType::Svg, MediaType::Webp];
        let types = preferred_output_media_types(&artifact, pref);
        // Svg is not in default_base, so it's ignored from preference.
        assert_eq!(types[0], MediaType::Webp);
        assert_eq!(types.len(), 4);
    }
}
