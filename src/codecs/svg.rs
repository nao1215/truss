//! SVG sanitization and rasterization codec.
//!
//! This module provides two SVG processing modes:
//!
//! - **Sanitize-only** (SVG→SVG): removes dangerous elements (`<script>`, `<foreignObject>`,
//!   `<iframe>`, `<embed>`, `<object>`), event handlers, `javascript:` URIs, external hrefs,
//!   `xml:base`, external CSS `url()` references, and `@import` rules.
//! - **Rasterize** (SVG→JPEG/PNG/WebP/AVIF): sanitizes first, then renders via `resvg` and
//!   encodes to the requested raster format.
//!
//! # Security model
//!
//! The sanitizer is a streaming XML filter, not a full DOM rewrite. It operates on the
//! assumption that the output will be served with `Content-Security-Policy: sandbox` and
//! `X-Content-Type-Options: nosniff` headers. The sanitizer is defense-in-depth, not a
//! standalone guarantee. Non-UTF-8 attribute names/values are dropped entirely.
//!
//! # Limitations
//!
//! - `resvg` does not expose a cancellation token, so deadline checks can only prevent
//!   *starting* an expensive rasterization, not abort one in progress.
//! - System fonts are not loaded; SVGs with text will render with missing glyphs in
//!   environments without fonts (e.g., distroless containers).
//! - SVG-to-SVG mode silently ignores resize/rotate/fit options since those are raster
//!   operations.

use crate::core::{
    Artifact, ArtifactMetadata, MAX_OUTPUT_PIXELS, MediaType, Rotation, TransformError,
    TransformRequest, TransformResult,
};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ColorType, ImageEncoder, RgbaImage};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use std::io::Cursor;
use std::time::Instant;

/// Transforms an SVG artifact by sanitizing and optionally rasterizing it.
///
/// When the output format is SVG, the input is sanitized (dangerous elements and attributes
/// are removed) and returned as sanitized SVG. When the output format is a raster type
/// (JPEG, PNG, WebP, AVIF, BMP), the SVG is rasterized using `resvg` and encoded into the
/// target format.
///
/// # Errors
///
/// Returns [`TransformError::InvalidOptions`] when the request fails validation,
/// [`TransformError::DecodeFailed`] when the SVG cannot be parsed or rasterized,
/// and [`TransformError::EncodeFailed`] when raster encoding fails.
///
/// # Examples
///
/// ```
/// use truss::{sniff_artifact, RawArtifact, TransformRequest, TransformOptions, MediaType};
/// use truss::transform_svg;
///
/// let svg_bytes = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><rect width=\"10\" height=\"10\" fill=\"red\"/></svg>";
/// let input = sniff_artifact(RawArtifact::new(svg_bytes.to_vec(), None)).unwrap();
/// let result = transform_svg(TransformRequest::new(
///     input,
///     TransformOptions {
///         format: Some(MediaType::Png),
///         width: Some(10),
///         height: Some(10),
///         ..TransformOptions::default()
///     },
/// )).unwrap();
/// assert_eq!(result.artifact.media_type, MediaType::Png);
/// ```
pub fn transform_svg(request: TransformRequest) -> Result<TransformResult, TransformError> {
    if request.options.blur.is_some() {
        return Err(TransformError::InvalidOptions(
            "blur is not supported for SVG inputs".to_string(),
        ));
    }
    if request.options.sharpen.is_some() {
        return Err(TransformError::InvalidOptions(
            "sharpen is not supported for SVG inputs".to_string(),
        ));
    }
    if request.watermark.is_some() {
        return Err(TransformError::InvalidOptions(
            "watermark is not supported for SVG inputs".to_string(),
        ));
    }

    let normalized = request.normalize()?;
    let deadline = normalized.options.deadline;
    let start = deadline.map(|_| Instant::now());

    let sanitized = sanitize_svg(&normalized.input.bytes)?;

    if let (Some(start), Some(limit)) = (start, deadline) {
        crate::codecs::raster::check_deadline(start.elapsed(), limit, "sanitize")?;
    }

    if normalized.options.format == MediaType::Svg {
        // Sanitize-only: return the sanitized SVG.
        return Ok(TransformResult {
            artifact: Artifact::new(
                sanitized.into_bytes(),
                MediaType::Svg,
                ArtifactMetadata {
                    width: None,
                    height: None,
                    frame_count: 1,
                    duration: None,
                    has_alpha: Some(true),
                },
            ),
            warnings: vec![],
        });
    }

    // Parse the SVG tree once for both size determination and rasterization.
    let tree = resvg::usvg::Tree::from_str(&sanitized, &resvg::usvg::Options::default())
        .map_err(|e| TransformError::DecodeFailed(format!("SVG parse error: {e}")))?;

    let (width, height) =
        determine_render_size(&tree, normalized.options.width, normalized.options.height);

    let pixel_count = width as u64 * height as u64;
    if pixel_count > MAX_OUTPUT_PIXELS {
        return Err(TransformError::LimitExceeded(format!(
            "requested SVG rasterization size {width}x{height} ({pixel_count} pixels) exceeds limit of {MAX_OUTPUT_PIXELS}"
        )));
    }

    let rgba_image = rasterize_svg(&tree, width, height)?;

    if let (Some(start), Some(limit)) = (start, deadline) {
        crate::codecs::raster::check_deadline(start.elapsed(), limit, "rasterize")?;
    }

    // Apply rotation if requested.
    let rgba_image = if normalized.options.rotate != Rotation::Deg0 {
        let dynamic = image::DynamicImage::ImageRgba8(rgba_image);
        let rotated = match normalized.options.rotate {
            Rotation::Deg90 => dynamic.rotate90(),
            Rotation::Deg180 => dynamic.rotate180(),
            Rotation::Deg270 => dynamic.rotate270(),
            Rotation::Deg0 => dynamic,
        };
        rotated.into_rgba8()
    } else {
        rgba_image
    };

    let (out_width, out_height) = (rgba_image.width(), rgba_image.height());

    let bytes = encode_raster_output(
        &rgba_image,
        normalized.options.format,
        normalized.options.quality,
    )?;

    if let (Some(start), Some(limit)) = (start, deadline) {
        crate::codecs::raster::check_deadline(start.elapsed(), limit, "encode")?;
    }

    let format = normalized.options.format;

    Ok(TransformResult {
        artifact: Artifact::new(
            bytes,
            format,
            ArtifactMetadata {
                width: Some(out_width),
                height: Some(out_height),
                frame_count: 1,
                duration: None,
                has_alpha: Some(format != MediaType::Jpeg),
            },
        ),
        warnings: vec![],
    })
}

/// Sanitizes an SVG document by removing dangerous elements and attributes.
///
/// Removes:
/// - `<script>` elements and their contents
/// - `<foreignObject>` elements and their contents
/// - Event handler attributes (`onclick`, `onload`, etc.)
/// - External references in `href`/`xlink:href` (keeps internal `#fragment` refs)
/// - `data:` URLs containing scripts (allows `data:image/*`)
/// - External `url()` references inside `<style>` text (keeps local `url(#id)` refs)
fn sanitize_svg(bytes: &[u8]) -> Result<String, TransformError> {
    let input = std::str::from_utf8(bytes)
        .map_err(|e| TransformError::DecodeFailed(format!("SVG is not valid UTF-8: {e}")))?;

    let mut reader = Reader::from_str(input);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut skip_depth: usize = 0;
    let mut in_style = false;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Ok(Event::Start(ref e)) => {
                let name = local_name(e.name().as_ref());
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                if is_forbidden_element(&name) {
                    skip_depth = 1;
                    continue;
                }
                if name == "style" {
                    in_style = true;
                }
                let sanitized = sanitize_attributes(e);
                writer
                    .write_event(Event::Start(sanitized))
                    .map_err(|e| TransformError::DecodeFailed(format!("SVG write error: {e}")))?;
            }
            Ok(Event::End(ref e)) => {
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                let name = local_name(e.name().as_ref());
                if name == "style" {
                    in_style = false;
                }
                writer
                    .write_event(Event::End(e.to_owned()))
                    .map_err(|e| TransformError::DecodeFailed(format!("SVG write error: {e}")))?;
            }
            Ok(Event::Empty(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                let name = local_name(e.name().as_ref());
                if is_forbidden_element(&name) {
                    continue;
                }
                let sanitized = sanitize_attributes(e);
                writer
                    .write_event(Event::Empty(sanitized))
                    .map_err(|e| TransformError::DecodeFailed(format!("SVG write error: {e}")))?;
            }
            Ok(Event::Text(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                if in_style {
                    let decoded = e.decode().unwrap_or_default();
                    let text = quick_xml::escape::unescape(&decoded).unwrap_or_default();
                    let sanitized_css = sanitize_css_urls(&text);
                    let text_event = quick_xml::events::BytesText::new(&sanitized_css);
                    writer
                        .write_event(Event::Text(text_event.into_owned()))
                        .map_err(|e| {
                            TransformError::DecodeFailed(format!("SVG write error: {e}"))
                        })?;
                } else {
                    writer.write_event(Event::Text(e.to_owned())).map_err(|e| {
                        TransformError::DecodeFailed(format!("SVG write error: {e}"))
                    })?;
                }
            }
            Ok(Event::CData(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                if in_style {
                    // CDATA inside <style> can contain @import/url() that loads
                    // external resources.  Sanitize the CSS content, then emit
                    // as a regular Text event (the CDATA wrapper is unnecessary
                    // after sanitization and would hide the content from further
                    // processing by downstream parsers).
                    let text = String::from_utf8_lossy(e.as_ref());
                    let sanitized_css = sanitize_css_urls(&text);
                    let text_event = quick_xml::events::BytesText::new(&sanitized_css);
                    writer
                        .write_event(Event::Text(text_event.into_owned()))
                        .map_err(|e| {
                            TransformError::DecodeFailed(format!("SVG write error: {e}"))
                        })?;
                } else {
                    writer
                        .write_event(Event::CData(e.to_owned()))
                        .map_err(|e| {
                            TransformError::DecodeFailed(format!("SVG write error: {e}"))
                        })?;
                }
            }
            Ok(event) => {
                if skip_depth > 0 {
                    continue;
                }
                writer
                    .write_event(event)
                    .map_err(|e| TransformError::DecodeFailed(format!("SVG write error: {e}")))?;
            }
            Err(e) => {
                return Err(TransformError::DecodeFailed(format!(
                    "SVG parse error: {e}"
                )));
            }
        }
    }

    let result = writer.into_inner().into_inner();
    String::from_utf8(result)
        .map_err(|e| TransformError::DecodeFailed(format!("SVG output is not valid UTF-8: {e}")))
}

/// Returns the local name of an XML element (strips namespace prefix).
fn local_name(name: &[u8]) -> String {
    let name_str = std::str::from_utf8(name).unwrap_or("");
    name_str
        .rsplit_once(':')
        .map_or(name_str, |(_, local)| local)
        .to_ascii_lowercase()
}

/// Returns `true` if the element should be completely removed from the SVG.
///
/// Blocks elements that can execute scripts, load external content, or embed
/// arbitrary HTML/plugin content.
fn is_forbidden_element(local_name: &str) -> bool {
    matches!(
        local_name,
        "script" | "foreignobject" | "iframe" | "embed" | "object"
    )
}

/// Returns `true` if the attribute is an event handler (starts with "on").
fn is_event_handler(attr_name: &str) -> bool {
    let lower = attr_name.to_ascii_lowercase();
    lower.starts_with("on") && lower.len() > 2 && lower.as_bytes()[2].is_ascii_alphabetic()
}

/// Returns `true` if the href value is dangerous.
///
/// Uses an allowlist approach: only empty values, `#fragment` references, and
/// `data:image/*` URLs are considered safe.  Everything else — including
/// `file:`, `ftp:`, `javascript:`, `http://`, unknown schemes, and bare
/// paths — is blocked.
fn is_dangerous_href(value: &str) -> bool {
    let trimmed = value.trim();

    // Allow empty hrefs (harmless).
    if trimmed.is_empty() {
        return false;
    }

    // Allow internal fragment references (#id).
    if trimmed.starts_with('#') {
        return false;
    }

    // Allow safe raster data:image/* URLs, but reject data:image/svg+xml
    // to prevent embedded SVGs from bypassing sanitization.
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("data:image/") {
        return lower.starts_with("data:image/svg");
    }

    // Everything else is dangerous.
    true
}

/// Sanitizes attributes on an SVG element, removing dangerous attributes.
///
/// Removes event handlers, dangerous `href`/`xlink:href` values, `xml:base`
/// (which can redirect relative references externally), and external `url()`
/// references inside inline `style` attributes. Non-UTF-8 attributes are
/// dropped entirely as a safety measure.
fn sanitize_attributes<'a>(element: &'a BytesStart<'a>) -> BytesStart<'a> {
    let mut sanitized = BytesStart::new(
        std::str::from_utf8(element.name().as_ref())
            .unwrap_or("unknown")
            .to_string(),
    );

    for attr in element.attributes().flatten() {
        // Drop attributes with non-UTF-8 names or values. A browser's lenient
        // parser might interpret them differently than quick-xml, so keeping
        // them would be a security risk.
        let Ok(key) = std::str::from_utf8(attr.key.as_ref()) else {
            continue;
        };
        let Ok(value) = std::str::from_utf8(&attr.value) else {
            continue;
        };

        // Remove event handler attributes.
        if is_event_handler(key) {
            continue;
        }

        let key_lower = key.to_ascii_lowercase();
        let key_local = key_lower
            .rsplit_once(':')
            .map_or(key_lower.as_str(), |(_, local)| local);

        // Block xml:base which can redirect relative references externally.
        if key_lower == "xml:base" {
            continue;
        }

        // Check href/xlink:href for dangerous values.
        if key_local == "href" && is_dangerous_href(value) {
            continue;
        }

        // Sanitize inline style attributes to remove external url() references.
        if key_local == "style" {
            let sanitized_value = sanitize_css_urls(value);
            sanitized.push_attribute((key, sanitized_value.as_str()));
            continue;
        }

        sanitized.push_attribute((key, value));
    }

    sanitized
}

/// Removes external `url()` references and `@import` rules from CSS text.
///
/// Keeps local references like `url(#gradientId)` and `url(data:image/...)`, but removes
/// external URLs (`url(http://...)`, `url(https://...)`, `url(//)`) and non-image data URLs
/// by replacing them with `url()` (empty, which CSS treats as invalid and ignores).
/// Also removes `@import` rules which can load external stylesheets.
fn sanitize_css_urls(css: &str) -> String {
    // First remove @import rules (external stylesheet loading).
    let mut result = String::with_capacity(css.len());
    let mut remaining = css;

    // Remove @import rules. They can appear as:
    //   @import url("...");
    //   @import "...";
    while let Some(pos) = remaining.to_ascii_lowercase().find("@import") {
        result.push_str(&remaining[..pos]);
        // Skip everything until the next semicolon or end of string.
        let after_import = &remaining[pos + 7..];
        if let Some(semi) = after_import.find(';') {
            remaining = &after_import[semi + 1..];
        } else {
            remaining = "";
        }
    }
    result.push_str(remaining);

    // Then sanitize url() references.
    let css_after_import = result;
    let mut result = String::with_capacity(css_after_import.len());
    let mut remaining = css_after_import.as_str();

    while let Some(start) = remaining.to_ascii_lowercase().find("url(") {
        result.push_str(&remaining[..start]);
        let after_url = &remaining[start + 4..];

        let (url_value, rest) = extract_css_url_value(after_url);
        let trimmed = url_value
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .trim();

        if is_dangerous_css_url(trimmed) {
            result.push_str("url()");
        } else {
            result.push_str("url(");
            result.push_str(url_value);
            result.push(')');
        }
        remaining = rest;
    }

    result.push_str(remaining);
    result
}

/// Extracts the value between `url(` and `)`, returning (value, rest_after_closing_paren).
fn extract_css_url_value(s: &str) -> (&str, &str) {
    let mut depth = 0u32;
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    return (&s[..i], &s[i + 1..]);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    // No closing paren found; treat the rest as the value.
    (s, "")
}

/// Returns `true` if a CSS `url()` value points to a dangerous resource.
///
/// Uses an allowlist approach: only `#fragment` references and `data:image/*`
/// URLs are considered safe.  Everything else is blocked.
fn is_dangerous_css_url(value: &str) -> bool {
    let trimmed = value.trim();

    // Allow empty url() (harmless, CSS treats it as invalid).
    if trimmed.is_empty() {
        return false;
    }

    // Allow local fragment references (#id).
    if trimmed.starts_with('#') {
        return false;
    }

    // Allow safe raster data:image/* URLs, but reject data:image/svg+xml
    // to prevent embedded SVGs from bypassing sanitization.
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("data:image/") {
        return lower.starts_with("data:image/svg");
    }

    // Everything else is dangerous.
    true
}

/// Determines the render size for SVG rasterization from a pre-parsed tree.
///
/// If explicit width and height are provided, uses those. Otherwise, uses the
/// tree's intrinsic dimensions. Falls back to a default of 300x150 if
/// the SVG has no explicit dimensions (matching the HTML spec default).
fn determine_render_size(
    tree: &resvg::usvg::Tree,
    requested_width: Option<u32>,
    requested_height: Option<u32>,
) -> (u32, u32) {
    if let (Some(w), Some(h)) = (requested_width, requested_height) {
        return (w, h);
    }

    let size = tree.size();
    let intrinsic_w = size.width() as u32;
    let intrinsic_h = size.height() as u32;

    let (w, h) = match (requested_width, requested_height) {
        (Some(w), None) => {
            let h = if intrinsic_w > 0 {
                (w as f64 * intrinsic_h as f64 / intrinsic_w as f64).round() as u32
            } else {
                intrinsic_h
            };
            (w, h.max(1))
        }
        (None, Some(h)) => {
            let w = if intrinsic_h > 0 {
                (h as f64 * intrinsic_w as f64 / intrinsic_h as f64).round() as u32
            } else {
                intrinsic_w
            };
            (w.max(1), h)
        }
        (None, None) => {
            let w = if intrinsic_w > 0 { intrinsic_w } else { 300 };
            let h = if intrinsic_h > 0 { intrinsic_h } else { 150 };
            (w, h)
        }
        _ => unreachable!(),
    };

    (w.max(1u32), h.max(1u32))
}

/// Rasterizes a pre-parsed SVG tree into an RGBA pixel buffer using `resvg`.
fn rasterize_svg(
    tree: &resvg::usvg::Tree,
    width: u32,
    height: u32,
) -> Result<RgbaImage, TransformError> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
        TransformError::DecodeFailed(format!(
            "failed to create {width}x{height} pixel buffer for SVG rasterization"
        ))
    })?;

    let scale_x = width as f32 / tree.size().width();
    let scale_y = height as f32 / tree.size().height();
    let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);

    resvg::render(tree, transform, &mut pixmap.as_mut());

    // resvg produces premultiplied RGBA. Convert to straight alpha for image crate.
    let mut rgba_data = pixmap.take();
    for chunk in rgba_data.chunks_exact_mut(4) {
        let a = chunk[3] as u16;
        if a > 0 && a < 255 {
            chunk[0] = ((chunk[0] as u16 * 255 + a / 2) / a).min(255) as u8;
            chunk[1] = ((chunk[1] as u16 * 255 + a / 2) / a).min(255) as u8;
            chunk[2] = ((chunk[2] as u16 * 255 + a / 2) / a).min(255) as u8;
        }
    }

    RgbaImage::from_raw(width, height, rgba_data)
        .ok_or_else(|| TransformError::DecodeFailed("SVG rasterization buffer mismatch".into()))
}

/// Encodes an RGBA image to the specified raster format.
fn encode_raster_output(
    image: &RgbaImage,
    format: MediaType,
    quality: Option<u8>,
) -> Result<Vec<u8>, TransformError> {
    let mut bytes = Vec::new();
    let (width, height) = (image.width(), image.height());

    match format {
        MediaType::Jpeg => {
            let quality = quality.unwrap_or(80);
            let encoder = JpegEncoder::new_with_quality(&mut bytes, quality);
            // Convert to RGB for JPEG (no alpha).
            let rgb: Vec<u8> = image.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
            encoder
                .write_image(&rgb, width, height, ColorType::Rgb8.into())
                .map_err(|e| TransformError::EncodeFailed(format!("JPEG encode failed: {e}")))?;
        }
        MediaType::Png => {
            let encoder = PngEncoder::new(&mut bytes);
            encoder
                .write_image(image.as_ref(), width, height, ColorType::Rgba8.into())
                .map_err(|e| TransformError::EncodeFailed(format!("PNG encode failed: {e}")))?;
        }
        MediaType::Webp => {
            if let Some(q) = quality {
                #[cfg(feature = "webp-lossy")]
                {
                    let lossy_encoder = webp::Encoder::from_rgba(image.as_ref(), width, height);
                    let encoded = lossy_encoder.encode(q as f32);
                    bytes = encoded.to_vec();
                }
                #[cfg(not(feature = "webp-lossy"))]
                {
                    let _ = q;
                    return Err(TransformError::CapabilityMissing(
                        "lossy WebP encoding is not enabled in this build".into(),
                    ));
                }
            } else {
                let encoder = WebPEncoder::new_lossless(&mut bytes);
                encoder
                    .write_image(image.as_ref(), width, height, ColorType::Rgba8.into())
                    .map_err(|e| {
                        TransformError::EncodeFailed(format!("WebP encode failed: {e}"))
                    })?;
            }
        }
        MediaType::Avif => {
            let quality = quality.unwrap_or(80);
            let encoder =
                image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut bytes, 4, quality);
            encoder
                .write_image(image.as_ref(), width, height, ColorType::Rgba8.into())
                .map_err(|e| TransformError::EncodeFailed(format!("AVIF encode failed: {e}")))?;
        }
        MediaType::Bmp => {
            let encoder = image::codecs::bmp::BmpEncoder::new(&mut bytes);
            encoder
                .write_image(image.as_ref(), width, height, ColorType::Rgba8.into())
                .map_err(|e| TransformError::EncodeFailed(format!("BMP encode failed: {e}")))?;
        }
        MediaType::Svg => {
            return Err(TransformError::InvalidOptions(
                "SVG-to-SVG rasterization is not meaningful".into(),
            ));
        }
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{RawArtifact, Rotation, TransformOptions, sniff_artifact};

    fn svg_with_script() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert('xss')</script><rect width=\"10\" height=\"10\"/></svg>".to_vec()
    }

    fn svg_with_event_handler() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect onclick=\"alert('xss')\" width=\"10\" height=\"10\"/></svg>".to_vec()
    }

    fn svg_with_foreign_object() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><foreignObject><body>hi</body></foreignObject></svg>".to_vec()
    }

    fn svg_with_external_href() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"https://evil.com/img.png\"/></svg>".to_vec()
    }

    fn svg_with_data_script() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"><a href=\"data:text/html,<script>alert(1)</script>\">click</a></svg>".to_vec()
    }

    fn simple_svg() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20\" height=\"10\"><rect width=\"20\" height=\"10\" fill=\"blue\"/></svg>".to_vec()
    }

    #[test]
    fn sanitize_removes_script_element() {
        let result = sanitize_svg(&svg_with_script()).unwrap();
        assert!(
            !result.contains("<script"),
            "script element should be removed"
        );
        assert!(
            !result.contains("alert"),
            "script content should be removed"
        );
        assert!(result.contains("<rect"), "rect element should be preserved");
    }

    #[test]
    fn sanitize_removes_event_handlers() {
        let result = sanitize_svg(&svg_with_event_handler()).unwrap();
        assert!(!result.contains("onclick"), "onclick should be removed");
        assert!(result.contains("<rect"), "rect element should be preserved");
        assert!(
            result.contains("width"),
            "width attribute should be preserved"
        );
    }

    #[test]
    fn sanitize_removes_foreign_object() {
        let result = sanitize_svg(&svg_with_foreign_object()).unwrap();
        assert!(
            !result.contains("foreignObject"),
            "foreignObject should be removed"
        );
    }

    #[test]
    fn sanitize_removes_external_href() {
        let result = sanitize_svg(&svg_with_external_href()).unwrap();
        assert!(
            !result.contains("https://evil.com"),
            "external href should be removed"
        );
    }

    #[test]
    fn sanitize_removes_data_script_href() {
        let result = sanitize_svg(&svg_with_data_script()).unwrap();
        assert!(
            !result.contains("data:text/html"),
            "data script href should be removed"
        );
    }

    #[test]
    fn sanitize_preserves_valid_svg() {
        let result = sanitize_svg(&simple_svg()).unwrap();
        assert!(result.contains("<svg"), "svg element should be preserved");
        assert!(result.contains("<rect"), "rect element should be preserved");
        assert!(
            result.contains("fill=\"blue\""),
            "fill attribute should be preserved"
        );
    }

    #[test]
    fn sanitize_allows_data_image_href() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"data:image/png;base64,abc\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            result.contains("data:image/png"),
            "data:image/* href should be preserved"
        );
    }

    #[test]
    fn sanitize_allows_internal_fragment_href() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><use href=\"#myShape\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            result.contains("#myShape"),
            "internal fragment href should be preserved"
        );
    }

    #[test]
    fn sanitize_removes_external_css_url() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style>rect { fill: url(https://evil.com/style.css) }</style><rect width=\"10\" height=\"10\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("evil.com"),
            "external CSS url() should be removed"
        );
        assert!(
            result.contains("url()"),
            "dangerous url() should be emptied"
        );
    }

    #[test]
    fn sanitize_preserves_local_css_url() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style>rect { fill: url(#myGradient) }</style><rect width=\"10\" height=\"10\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            result.contains("url(#myGradient)"),
            "local CSS url(#id) should be preserved"
        );
    }

    #[test]
    fn sanitize_removes_data_script_css_url() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style>rect { background: url(data:text/html,<script>alert(1)</script>) }</style></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("data:text/html"),
            "data:text/html CSS url() should be removed"
        );
    }

    #[test]
    fn sanitize_removes_javascript_href() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><a href=\"javascript:alert(1)\">click</a></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("javascript:"),
            "javascript: href should be removed"
        );
    }

    #[test]
    fn sanitize_removes_mixed_case_javascript_href() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><a href=\"JaVaScRiPt:alert(1)\">click</a></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("alert"),
            "mixed-case javascript: href should be removed"
        );
    }

    #[test]
    fn sanitize_removes_mixed_case_data_href() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><a href=\"DATA:text/html,evil\">click</a></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("DATA:text/html"),
            "mixed-case DATA: href should be removed"
        );
    }

    #[test]
    fn sanitize_removes_iframe_element() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><iframe src=\"https://evil.com\"></iframe></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(!result.contains("iframe"), "iframe should be removed");
    }

    #[test]
    fn sanitize_removes_xml_base_attribute() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" xml:base=\"https://evil.com/\"><use href=\"img.svg\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("xml:base"),
            "xml:base attribute should be removed"
        );
    }

    #[test]
    fn sanitize_removes_inline_style_external_url() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect style=\"background:url(https://evil.com/track)\" width=\"10\" height=\"10\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("evil.com"),
            "external url() in inline style should be removed"
        );
        assert!(
            result.contains("url()"),
            "dangerous url() should be emptied"
        );
    }

    #[test]
    fn sanitize_removes_entity_escaped_external_css_url() {
        // Entity-escaped text: `&amp;` in the URL and the scheme itself
        // must still be detected as dangerous after unescape.
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style>rect { fill: url(https://evil.example/a?x=1&amp;y=2) }</style><rect width=\"10\" height=\"10\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("evil.example"),
            "entity-escaped external CSS url() should be removed"
        );
        assert!(
            result.contains("url()"),
            "dangerous url() should be emptied"
        );
    }

    #[test]
    fn sanitize_removes_css_import() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style>@import url(\"https://evil.com/style.css\"); rect { fill: red }</style></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("@import"),
            "@import should be removed from style"
        );
        assert!(
            !result.contains("evil.com"),
            "imported URL should be removed"
        );
        assert!(
            result.contains("fill: red"),
            "legitimate CSS should be preserved"
        );
    }

    #[test]
    fn sniff_detects_svg_input() {
        let artifact =
            sniff_artifact(RawArtifact::new(simple_svg(), None)).expect("should detect SVG");
        assert_eq!(artifact.media_type, MediaType::Svg);
        assert_eq!(artifact.metadata.has_alpha, Some(true));
    }

    #[test]
    fn sniff_detects_svg_with_xml_declaration() {
        let svg = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        let artifact =
            sniff_artifact(RawArtifact::new(svg.to_vec(), None)).expect("should detect SVG");
        assert_eq!(artifact.media_type, MediaType::Svg);
    }

    #[test]
    fn transform_svg_sanitize_only() {
        let input = sniff_artifact(RawArtifact::new(svg_with_script(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Svg),
                ..TransformOptions::default()
            },
        ))
        .expect("sanitize should succeed");

        assert_eq!(result.artifact.media_type, MediaType::Svg);
        let output = std::str::from_utf8(&result.artifact.bytes).unwrap();
        assert!(!output.contains("<script"), "script should be removed");
        assert!(output.contains("<rect"), "rect should be preserved");
    }

    #[test]
    fn transform_svg_to_png() {
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(20),
                height: Some(10),
                ..TransformOptions::default()
            },
        ))
        .expect("SVG to PNG should succeed");

        assert_eq!(result.artifact.media_type, MediaType::Png);
        assert_eq!(result.artifact.metadata.width, Some(20));
        assert_eq!(result.artifact.metadata.height, Some(10));
    }

    #[test]
    fn transform_svg_to_jpeg() {
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                width: Some(20),
                height: Some(10),
                ..TransformOptions::default()
            },
        ))
        .expect("SVG to JPEG should succeed");

        assert_eq!(result.artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn transform_svg_uses_intrinsic_dimensions() {
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("SVG to PNG with intrinsic size should succeed");

        assert_eq!(result.artifact.metadata.width, Some(20));
        assert_eq!(result.artifact.metadata.height, Some(10));
    }

    #[test]
    fn transform_svg_to_png_with_rotate_90() {
        // simple_svg() is 20x10.  Rotating 90 degrees should produce 10x20.
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                rotate: Rotation::Deg90,
                ..TransformOptions::default()
            },
        ))
        .expect("SVG to PNG with rotate 90 should succeed");

        assert_eq!(result.artifact.media_type, MediaType::Png);
        assert_eq!(
            result.artifact.metadata.width,
            Some(10),
            "width should be swapped after 90 degree rotation"
        );
        assert_eq!(
            result.artifact.metadata.height,
            Some(20),
            "height should be swapped after 90 degree rotation"
        );
    }

    #[test]
    fn transform_svg_to_png_with_rotate_180() {
        // 180 degrees should preserve dimensions.
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                rotate: Rotation::Deg180,
                ..TransformOptions::default()
            },
        ))
        .expect("SVG to PNG with rotate 180 should succeed");

        assert_eq!(result.artifact.metadata.width, Some(20));
        assert_eq!(result.artifact.metadata.height, Some(10));
    }

    #[test]
    fn transform_svg_rejects_preserve_exif_with_svg_output() {
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let err = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Svg),
                preserve_exif: true,
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect_err("preserveExif + svg should fail");

        assert!(
            matches!(err, TransformError::InvalidOptions(_)),
            "expected InvalidOptions, got {err:?}"
        );
    }

    #[test]
    fn transform_svg_rejects_invalid_svg() {
        let artifact = Artifact::new(
            b"not an svg".to_vec(),
            MediaType::Svg,
            ArtifactMetadata {
                width: None,
                height: None,
                frame_count: 1,
                duration: None,
                has_alpha: Some(true),
            },
        );
        let err = transform_svg(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(100),
                height: Some(100),
                ..TransformOptions::default()
            },
        ))
        .expect_err("invalid SVG should fail");

        assert!(
            matches!(err, TransformError::DecodeFailed(_)),
            "expected DecodeFailed, got {err:?}"
        );
    }

    // --- Allowlist href/url() tests ---

    #[test]
    fn sanitize_removes_file_scheme_href() {
        let svg =
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"file:///etc/passwd\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("file:///etc/passwd"),
            "file: href should be removed"
        );
    }

    #[test]
    fn sanitize_removes_ftp_scheme_href() {
        let svg =
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"><image href=\"ftp://evil.com/img.png\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(!result.contains("ftp://"), "ftp: href should be removed");
    }

    #[test]
    fn sanitize_keeps_fragment_href() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><use href=\"#myShape\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            result.contains("#myShape"),
            "fragment href should be preserved"
        );
    }

    #[test]
    fn sanitize_removes_cdata_import_in_style() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style><![CDATA[@import url(https://evil.example/a.css); rect { fill: red }]]></style></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("@import"),
            "@import inside CDATA should be removed"
        );
        assert!(
            !result.contains("evil.example"),
            "external URL inside CDATA should be removed"
        );
        assert!(
            result.contains("fill: red"),
            "legitimate CSS should be preserved"
        );
    }

    #[test]
    fn sanitize_removes_cdata_external_url_in_style() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style><![CDATA[rect { background: url(https://evil.example/bg.png) }]]></style></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("evil.example"),
            "external url() inside CDATA should be removed"
        );
    }

    #[test]
    fn sanitize_removes_file_scheme_in_css_url() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style>rect { fill: url(file:///etc/passwd) }</style></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("file:///etc/passwd"),
            "file: url() in CSS should be removed"
        );
    }

    #[test]
    fn sanitize_keeps_local_css_url_fragment() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect style=\"fill: url(#gradient1)\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            result.contains("#gradient1"),
            "local fragment url() should be preserved"
        );
    }

    #[test]
    fn is_dangerous_href_blocks_file_scheme() {
        assert!(is_dangerous_href("file:///etc/passwd"));
    }

    #[test]
    fn is_dangerous_href_blocks_ftp_scheme() {
        assert!(is_dangerous_href("ftp://evil.com/file"));
    }

    #[test]
    fn is_dangerous_href_allows_fragment() {
        assert!(!is_dangerous_href("#myId"));
    }

    #[test]
    fn is_dangerous_href_allows_data_image() {
        assert!(!is_dangerous_href("data:image/png;base64,abc"));
    }

    #[test]
    fn is_dangerous_href_blocks_data_text() {
        assert!(is_dangerous_href(
            "data:text/html,<script>alert(1)</script>"
        ));
    }

    #[test]
    fn is_dangerous_css_url_blocks_file_scheme() {
        assert!(is_dangerous_css_url("file:///etc/passwd"));
    }

    #[test]
    fn is_dangerous_css_url_allows_fragment() {
        assert!(!is_dangerous_css_url("#gradientId"));
    }

    #[test]
    fn is_dangerous_css_url_allows_data_image() {
        assert!(!is_dangerous_css_url("data:image/png;base64,abc"));
    }

    #[test]
    fn svg_rejects_blur() {
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let request = TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                blur: Some(2.0),
                ..TransformOptions::default()
            },
        );
        let err = transform_svg(request).unwrap_err();
        assert!(
            matches!(err, TransformError::InvalidOptions(ref msg) if msg.contains("blur")),
            "expected InvalidOptions about blur, got: {err}"
        );
    }

    #[test]
    fn svg_rejects_sharpen() {
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let request = TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                sharpen: Some(2.0),
                ..TransformOptions::default()
            },
        );
        let err = transform_svg(request).unwrap_err();
        assert!(
            matches!(err, TransformError::InvalidOptions(ref msg) if msg.contains("sharpen")),
            "expected InvalidOptions about sharpen, got: {err}"
        );
    }

    #[test]
    fn svg_rejects_watermark() {
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let wm_input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let mut request = TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        );
        request.watermark = Some(crate::core::WatermarkInput {
            image: wm_input,
            position: crate::core::Position::Center,
            opacity: 50,
            margin: 0,
        });
        let err = transform_svg(request).unwrap_err();
        assert!(
            matches!(err, TransformError::InvalidOptions(ref msg) if msg.contains("watermark")),
            "expected InvalidOptions about watermark, got: {err}"
        );
    }
}
