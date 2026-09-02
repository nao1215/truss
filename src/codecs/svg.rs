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
//! - SVG-to-SVG mode silently ignores resize/rotate/fit/grayscale options since those are raster
//!   operations.

use crate::core::{
    Artifact, ArtifactMetadata, MAX_OUTPUT_PIXELS, MediaType, TransformError, TransformRequest,
    TransformResult,
};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ColorType, ImageEncoder, RgbaImage};
use quick_xml::events::{BytesStart, Event};

/// Maps a crop rectangle from the drawing's own coordinates into the space it is rasterized
/// in, and returns the size the whole drawing has to be rasterized at.
///
/// The fit mode was asked about the region the crop keeps, so that region has to come out at
/// `region_render`. The whole drawing is therefore rasterized at the same scale, and the
/// rectangle is scaled with it. Both dimensions use one scale, taken from whichever axis the
/// region is measured on more precisely, so the drawing is not stretched.
fn scale_crop_into_render_space(
    crop: crate::core::CropRegion,
    intrinsic: (u32, u32),
    region_render: (u32, u32),
) -> ((u32, u32), crate::core::CropRegion) {
    let scale_x = f64::from(region_render.0) / f64::from(crop.width.max(1));
    let scale_y = f64::from(region_render.1) / f64::from(crop.height.max(1));
    let scale = if scale_x.is_finite() && scale_y.is_finite() {
        (scale_x + scale_y) / 2.0
    } else {
        1.0
    };

    let clamp = |value: f64, floor: u32| -> u32 {
        if value < f64::from(floor) {
            floor
        } else if value > f64::from(u32::MAX) {
            u32::MAX
        } else {
            value as u32
        }
    };
    // An origin and a size have different floors. Zero is a perfectly good origin, and it is
    // the commonest one; a size floored at zero would be an empty rectangle. Scaling both
    // with the size's floor pushed every rectangle starting at the drawing's edge one pixel
    // inwards, which the clamp below then paid for out of the width.
    let origin = |value: u32| clamp((f64::from(value) * scale).round(), 0);
    let extent = |value: u32| clamp((f64::from(value) * scale).round(), 1);

    let full = (extent(intrinsic.0), extent(intrinsic.1));
    // The rectangle is clamped to the buffer it will be taken from, so rounding cannot put
    // its far edge past the last pixel.
    let x = origin(crop.x).min(full.0.saturating_sub(1));
    let y = origin(crop.y).min(full.1.saturating_sub(1));
    let width = extent(crop.width).min(full.0 - x);
    let height = extent(crop.height).min(full.1 - y);

    (
        full,
        crate::core::CropRegion {
            x,
            y,
            width,
            height,
        },
    )
}

/// The sentence truss gives an SVG that does not parse, in place of the parser's own.
///
/// `SVG data parsing failed cause the root node was opened but never closed` and
/// `attribute value not closed: \`"\` not found before end of input` are the XML parser's
/// wording, its grammar, and its idea of what a caller needs to know. Both reached the
/// CLI's stderr and the `detail` of the server's problem body, where they say nothing a
/// caller of an image endpoint can act on beyond what one sentence says.
fn svg_parse_failure() -> String {
    "svg document is not well-formed XML".to_string()
}
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use std::io::Cursor;

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
pub(crate) fn transform_svg(request: TransformRequest) -> Result<TransformResult, TransformError> {
    let normalized = request.normalize()?;
    let budget = crate::codecs::raster::EncodeDeadline::starting(normalized.options.deadline);

    let sanitized = sanitize_svg(&normalized.input.bytes)?;
    budget.check("sanitize")?;

    if normalized.options.format == MediaType::Svg {
        // With SVG output there is no pipeline: the document is handed back as its author
        // wrote it, so a stage that asks for different pixels cannot be honoured. The
        // stages that ask for a different picture are refused in `normalize`; these four
        // are the ones that need pixels to work on.
        for (requested, name) in [
            (normalized.options.blur.is_some(), "blur"),
            (normalized.options.sharpen.is_some(), "sharpen"),
            (normalized.watermark.is_some(), "watermark"),
            (normalized.options.crop.is_some(), "crop"),
        ] {
            if requested {
                return Err(TransformError::InvalidOptions(format!(
                    "{name} is not supported with SVG output; choose a raster output format such as png"
                )));
            }
        }

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
                    orientation: None,
                },
            ),
            warnings: vec![],
        });
    }

    // Parse the SVG tree once for both size determination and rasterization.
    let tree = resvg::usvg::Tree::from_str(&sanitized, &resvg::usvg::Options::default())
        .map_err(|_| TransformError::DecodeFailed(svg_parse_failure()))?;

    // The pipeline order is rotate then resize, so the size the fit mode is asked about is
    // the size of the rotated drawing, not of the drawing as it was written. Asking about
    // the unrotated one and turning the finished canvas afterwards returns the rotated
    // bounding box of the requested box, which is not the size the caller named.
    let intrinsic = intrinsic_render_size(&tree);
    let rotated_intrinsic = crate::codecs::raster::rotated_bounding_box(
        intrinsic.0,
        intrinsic.1,
        normalized.options.rotate.as_degrees(),
    );

    // A crop runs before the resize, so the size the fit mode is asked about is the size of
    // the region the crop keeps. The rectangle is in the coordinate space of the rotated
    // drawing at its own size, which is the size `truss inspect` reports.
    let cropped_intrinsic = match normalized.options.crop {
        Some(crop) => {
            if crop.x.saturating_add(crop.width) > rotated_intrinsic.0
                || crop.y.saturating_add(crop.height) > rotated_intrinsic.1
            {
                return Err(TransformError::InvalidOptions(format!(
                    "crop region {}x{}+{}+{} exceeds image bounds {}x{}",
                    crop.width,
                    crop.height,
                    crop.x,
                    crop.y,
                    rotated_intrinsic.0,
                    rotated_intrinsic.1
                )));
            }
            (crop.width, crop.height)
        }
        None => rotated_intrinsic,
    };

    // The drawing is rasterized at the size the fit mode scales the content to, not at the
    // requested box, so the scale stays uniform on both axes and the padding or cropping the
    // mode calls for is done afterwards by the same helpers the raster codec uses. Drawing
    // straight into the box would be `fill` whatever was asked for.
    let rotated_render = crate::codecs::raster::resize_content_size(
        cropped_intrinsic,
        normalized.options.width,
        normalized.options.height,
        normalized.options.fit,
        normalized.options.without_enlargement,
    );
    // With a crop the region the fit was asked about is a part of the drawing, so the scale
    // is the one that region needs: rasterizing the drawing at its own size and enlarging
    // afterwards would throw away the resolution a vector source is kept for. What that
    // scale is applied to depends on the rotation. With none, the rectangle is in the
    // drawing's own coordinates and the region is rendered directly, so the buffer is the
    // size of the output rather than of the drawing. With one, the rectangle is in the
    // rotated drawing's coordinates, the turn has to happen before the cut, and the whole
    // drawing is rendered at that scale.
    let (rotated_render_full, scaled_crop) = match normalized.options.crop {
        Some(crop) => {
            let (full, scaled) =
                scale_crop_into_render_space(crop, rotated_intrinsic, rotated_render);
            (full, Some(scaled))
        }
        None => (rotated_render, None),
    };
    let render_region = normalized
        .options
        .rotate
        .is_identity()
        .then_some(scaled_crop)
        .flatten();
    let render = pre_rotation_render_size(
        rotated_render_full,
        intrinsic,
        rotated_intrinsic,
        normalized.options.rotate.as_degrees(),
    );
    // The buffer that is actually allocated: the region when it is rendered directly, and
    // the whole drawing otherwise.
    let rasterization = match render_region {
        Some(region) => (region.width, region.height),
        None => render,
    };
    let canvas = crate::codecs::raster::resolved_output_dimensions(
        cropped_intrinsic,
        normalized.options.width,
        normalized.options.height,
        normalized.options.fit,
        normalized.options.without_enlargement,
    );

    // Both are checked from dimensions alone, before anything is allocated: `cover` scales
    // the content past the box it returns, so the buffer it materializes is not the size of
    // the output. That is the shape of #316, on the other codec.
    for (label, (width, height)) in [("rasterization", rasterization), ("output", canvas)] {
        let pixel_count = u64::from(width) * u64::from(height);
        if pixel_count > MAX_OUTPUT_PIXELS {
            return Err(TransformError::LimitExceeded(format!(
                "requested SVG {label} size {width}x{height} ({pixel_count} pixels) exceeds limit of {MAX_OUTPUT_PIXELS}"
            )));
        }
    }

    let rgba_image = rasterize_svg(&tree, render.0, render.1, render_region)?;
    budget.check("rasterize")?;

    // From here the drawing is pixels, so it goes through the stages the raster codec runs,
    // in the one place their order is written down. That is what `docs/pipeline.md` means by
    // a drawing joining the raster pipeline: the rotation, the crop, the resize, the filters,
    // the desaturation, and the watermark are the same code with the same background rules
    // and the same limits, reached from a different source.
    //
    // Only the crop rectangle differs from the request's own. A region rendered directly is
    // already the cut, so there is nothing left to take; a rotated drawing was rendered whole
    // and carries the rectangle scaled into the space it was rendered in.
    let crop = match scaled_crop {
        Some(crop) if render_region.is_none() => Some(crop),
        _ => None,
    };
    let image = crate::codecs::raster::apply_pixel_stages(
        image::DynamicImage::ImageRgba8(rgba_image),
        &normalized,
        budget,
        crop,
    )?;

    // Formats without an alpha channel need the transparency resolved before the encoder
    // sees it. The raster codec owns that rule too, so both paths flatten the same way.
    let rgba_image = crate::codecs::raster::flatten_for_opaque_output(
        image,
        normalized.options.background,
        normalized.options.format,
    )
    .into_rgba8();

    let (out_width, out_height) = (rgba_image.width(), rgba_image.height());

    let bytes = encode_raster_output(
        &rgba_image,
        normalized.options.format,
        normalized.options.quality,
    )?;
    budget.check("encode")?;

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
                has_alpha: Some(crate::codecs::raster::format_carries_alpha(format)),
                orientation: None,
            },
        ),
        warnings: vec![],
    })
}

/// Maximum number of XML elements allowed in a single SVG document.
/// Prevents CPU exhaustion from extremely complex SVGs.
const MAX_SVG_ELEMENTS: usize = 100_000;

/// Maximum nesting depth allowed in an SVG document.
/// Prevents stack-like exhaustion from deeply nested elements.
const MAX_SVG_NESTING_DEPTH: usize = 256;

/// Sanitizes an SVG document by removing dangerous elements and attributes.
///
/// Removes:
/// - `<script>`, `<foreignObject>`, `<iframe>`, `<embed>`, `<object>`, `<handler>`,
///   and the SMIL animation elements, along with their contents
/// - Event handler attributes (`onclick`, `onload`, etc.), under any namespace prefix
/// - External references in `href`/`xlink:href` (keeps internal `#fragment` refs)
/// - `data:` URLs containing scripts (allows `data:image/*`)
/// - External `url()` references wherever they appear: `<style>` text, the `style`
///   attribute, and the presentation attributes that take a `<funciri>`
/// - At-rules outside [`ALLOWED_AT_RULES`], which is what removes `@import` however
///   its at-keyword is spelled
/// - Processing instructions, which a browser honours and which can load an external
///   stylesheet; the XML declaration is kept
///
/// Refuses a document whose doctype declares an external entity or nests one entity
/// inside another, because removing only the declarations would leave the references
/// to them dangling.
fn sanitize_svg(bytes: &[u8]) -> Result<String, TransformError> {
    let input = std::str::from_utf8(bytes)
        .map_err(|e| TransformError::DecodeFailed(format!("SVG is not valid UTF-8: {e}")))?;

    let mut reader = Reader::from_str(input);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut skip_depth: usize = 0;
    let mut in_style = false;
    let mut element_count: usize = 0;
    let mut nesting_depth: usize = 0;

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
                element_count += 1;
                if element_count > MAX_SVG_ELEMENTS {
                    return Err(TransformError::LimitExceeded(format!(
                        "SVG exceeds maximum element count ({MAX_SVG_ELEMENTS})"
                    )));
                }
                nesting_depth += 1;
                if nesting_depth > MAX_SVG_NESTING_DEPTH {
                    return Err(TransformError::LimitExceeded(format!(
                        "SVG exceeds maximum nesting depth ({MAX_SVG_NESTING_DEPTH})"
                    )));
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
                nesting_depth = nesting_depth.saturating_sub(1);
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
                element_count += 1;
                if element_count > MAX_SVG_ELEMENTS {
                    return Err(TransformError::LimitExceeded(format!(
                        "SVG exceeds maximum element count ({MAX_SVG_ELEMENTS})"
                    )));
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
                    let text = quick_xml::escape::unescape(e.as_ref()).unwrap_or_default();
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
                    let sanitized_css = sanitize_css_urls(e.as_ref());
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
            // A processing instruction is honoured by a browser rendering the
            // document, and `<?xml-stylesheet?>` loads an external stylesheet —
            // an XSLT one generates arbitrary markup, which defeats every element
            // and attribute rule at once. Declarative styling from outside the
            // document is not something a sanitizing image pipeline preserves.
            // The XML declaration is a separate event and is kept.
            Ok(Event::PI(_)) => {}
            // The doctype itself is inert in every renderer, and an editor's
            // literal entity declarations have to survive or the references to
            // them dangle. A subset that declares an external entity or nests one
            // entity inside another is an XXE or billion-laughs payload; the
            // document is refused rather than stripped, because the content
            // references those entities and removing only the declarations would
            // emit a document that is no longer well-formed.
            Ok(Event::DocType(ref e)) => {
                if skip_depth > 0 {
                    continue;
                }
                if doctype_carries_unsafe_declarations(e.as_ref()) {
                    return Err(TransformError::DecodeFailed(
                        "SVG doctype declares external or nested entities".to_string(),
                    ));
                }
                writer
                    .write_event(Event::DocType(e.to_owned()))
                    .map_err(|e| TransformError::DecodeFailed(format!("SVG write error: {e}")))?;
            }
            Ok(event) => {
                if skip_depth > 0 {
                    continue;
                }
                writer
                    .write_event(event)
                    .map_err(|e| TransformError::DecodeFailed(format!("SVG write error: {e}")))?;
            }
            Err(_) => {
                return Err(TransformError::DecodeFailed(svg_parse_failure()));
            }
        }
    }

    let result = writer.into_inner().into_inner();
    String::from_utf8(result)
        .map_err(|e| TransformError::DecodeFailed(format!("SVG output is not valid UTF-8: {e}")))
}

/// Returns `true` when a doctype's internal subset declares something a
/// sanitized document should not carry.
///
/// Two shapes qualify. An external identifier (`SYSTEM` or `PUBLIC`) inside the
/// subset declares an external entity, which is an XXE payload; XML keeps those
/// keywords uppercase, so the search is exact. An `&` inside the subset means one
/// entity's replacement text references another, which is the billion-laughs
/// shape — truss never expands entities, but a consumer of the sanitized document
/// might. An editor's declarations are flat literals and contain neither.
///
/// A `SYSTEM` or `PUBLIC` identifier on the doctype itself, outside the subset,
/// points at a DTD that no renderer fetches and is left alone.
fn doctype_carries_unsafe_declarations(doctype: &str) -> bool {
    let Some(subset) = doctype.split_once('[').map(|(_, rest)| rest) else {
        return false;
    };
    subset.contains("SYSTEM") || subset.contains("PUBLIC") || subset.contains('&')
}

/// Returns the local name of an XML element (strips namespace prefix).
fn local_name(name: &str) -> String {
    name.rsplit_once(':')
        .map_or(name, |(_, local)| local)
        .to_ascii_lowercase()
}

/// Returns `true` if the element should be completely removed from the SVG.
///
/// Blocks elements that can execute scripts, load external content, or embed
/// arbitrary HTML/plugin content.
///
/// The SMIL animation elements are here because they set attributes at render time:
/// `<animate attributeName="href" to="javascript:...">` restores exactly what the attribute
/// filter removes, and does so through `to`, `values`, `from`, or `by` rather than through
/// `href`. Dropping the elements is what makes the attribute rules hold; a sanitizing image
/// pipeline has no use for declarative animation. `handler` is SVG Tiny's script container.
fn is_forbidden_element(local_name: &str) -> bool {
    matches!(
        local_name,
        "script"
            | "foreignobject"
            | "iframe"
            | "embed"
            | "object"
            | "animate"
            | "set"
            | "animatetransform"
            | "animatemotion"
            | "animatecolor"
            | "handler"
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
    let mut sanitized = BytesStart::new(element.name().as_ref().to_string());

    for attr in element.attributes().flatten() {
        let key: &str = attr.key.as_ref();
        let value: &str = &attr.value;

        let key_lower = key.to_ascii_lowercase();
        let key_local = key_lower
            .rsplit_once(':')
            .map_or(key_lower.as_str(), |(_, local)| local);

        // Remove event handler attributes. The local name is what the href and
        // style rules below already match on, and having two notions of an
        // attribute's name inside one function is how the next gap gets in.
        if is_event_handler(key_local) {
            continue;
        }

        // Block xml:base which can redirect relative references externally.
        if key_lower == "xml:base" {
            continue;
        }

        // Check href/xlink:href for dangerous values.
        if key_local == "href" && is_dangerous_href(value) {
            continue;
        }

        // A `url()` means the same thing wherever it is written, so `style` is not
        // the only attribute that carries one: every presentation attribute taking
        // a <funciri> — `fill`, `stroke`, `filter`, `mask`, `clip-path`, the
        // `marker-*` family, `cursor` — is another spelling of the same
        // declaration. Deciding from the value rather than from a list of names does
        // not need revisiting when SVG grows another such attribute.
        if key_local == "style" || contains_css_url(value) {
            let sanitized_value = sanitize_css_urls(value);
            sanitized.push_attribute((key, sanitized_value.as_str()));
            continue;
        }

        sanitized.push_attribute((key, value));
    }

    sanitized
}

/// Returns `true` when the value contains a CSS `url(` token, ignoring case.
///
/// Avoids allocating a lowercased copy of every attribute value just to answer
/// whether the value is worth rewriting.
fn contains_css_url(value: &str) -> bool {
    value
        .as_bytes()
        .windows(4)
        .any(|window| window.eq_ignore_ascii_case(b"url("))
}

/// At-rules kept in sanitized CSS.
///
/// Everything not listed is dropped whole, which is what makes the rule hold
/// however the at-keyword is spelled: `@import` is the one at-rule that fetches
/// a stylesheet by itself, and CSS identifiers admit escapes, so `@\69 mport`
/// and `@\import` are the same rule to a renderer and match no literal search.
/// Removing the class rather than the spelling is the same move that removing
/// the SMIL elements made for the attribute rules.
const ALLOWED_AT_RULES: &[&str] = &[
    "charset",
    "container",
    "counter-style",
    "font-face",
    "font-feature-values",
    "keyframes",
    "layer",
    "media",
    "page",
    "property",
    "scope",
    "starting-style",
    "supports",
];

/// Reads a CSS identifier starting at `s`, decoding escapes.
///
/// Returns the lowercased identifier and the number of bytes it occupies in the
/// input. An escape is a backslash followed by one to six hex digits and at most
/// one trailing whitespace character, or a backslash followed by any other
/// character, which stands for that character.
fn read_css_identifier(s: &str) -> (String, usize) {
    let bytes = s.as_bytes();
    let mut name = String::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            index += 1;
            let mut hex = String::new();
            while index < bytes.len() && hex.len() < 6 && bytes[index].is_ascii_hexdigit() {
                hex.push(bytes[index] as char);
                index += 1;
            }
            if hex.is_empty() {
                // A backslash before a non-hex character stands for that character.
                if index < bytes.len() {
                    let ch = s[index..].chars().next().unwrap_or('\u{FFFD}');
                    name.push(ch);
                    index += ch.len_utf8();
                }
            } else {
                // One whitespace character after the hex digits terminates the
                // escape and is consumed rather than being part of the name.
                if index < bytes.len() && bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    name.push(ch);
                }
            }
            continue;
        }

        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte >= 0x80 {
            let ch = s[index..].chars().next().unwrap_or('\u{FFFD}');
            name.push(ch);
            index += ch.len_utf8();
            continue;
        }

        break;
    }

    (name.to_ascii_lowercase(), index)
}

/// Returns the byte offset just past an at-rule, where `s` starts just after its at-keyword.
///
/// A statement at-rule ends at the first top-level `;`; a block at-rule ends at
/// the matching `}`. Quoted strings are skipped so a `;` or `}` inside one does
/// not end the rule early.
fn end_of_at_rule(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut index = 0;
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;

    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open) = quote {
            if byte == b'\\' {
                // Skip the escaped character whole. Advancing a fixed two bytes
                // would leave the scan inside a multi-byte character; every byte
                // it could then stop on is a continuation byte, so nothing is
                // currently misread, but the offset this returns is used to slice
                // the string and should not depend on that.
                index += 1;
                if let Some(escaped) = s[index..].chars().next() {
                    index += escaped.len_utf8();
                }
                continue;
            }
            if byte == open {
                quote = None;
            }
            index += 1;
            continue;
        }

        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'{' => depth += 1,
            b'}' => {
                if depth <= 1 {
                    return index + 1;
                }
                depth -= 1;
            }
            b';' if depth == 0 => return index + 1,
            _ => {}
        }
        index += 1;
    }

    bytes.len()
}

/// Removes at-rules outside [`ALLOWED_AT_RULES`] from CSS text.
fn strip_disallowed_at_rules(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut result = String::with_capacity(css.len());
    let mut index = 0;
    let mut quote: Option<u8> = None;

    while index < bytes.len() {
        let byte = bytes[index];

        if let Some(open) = quote {
            // Advance by characters, not bytes: pushing each byte of a multi-byte
            // character as its own `char` would turn `content: "café"` into mojibake.
            let ch = css[index..].chars().next().unwrap_or('\u{FFFD}');
            result.push(ch);
            index += ch.len_utf8();
            if ch == '\\' {
                if let Some(escaped) = css[index..].chars().next() {
                    result.push(escaped);
                    index += escaped.len_utf8();
                }
                continue;
            }
            if ch == open as char {
                quote = None;
            }
            continue;
        }

        if byte == b'"' || byte == b'\'' {
            quote = Some(byte);
            result.push(byte as char);
            index += 1;
            continue;
        }

        if byte == b'@' {
            let (name, consumed) = read_css_identifier(&css[index + 1..]);
            if !name.is_empty() && !ALLOWED_AT_RULES.contains(&name.as_str()) {
                let after_keyword = index + 1 + consumed;
                index = after_keyword + end_of_at_rule(&css[after_keyword..]);
                continue;
            }
        }

        let ch = css[index..].chars().next().unwrap_or('\u{FFFD}');
        result.push(ch);
        index += ch.len_utf8();
    }

    result
}

/// Removes external `url()` references and disallowed at-rules from CSS text.
///
/// Keeps local references like `url(#gradientId)` and `url(data:image/...)`, but removes
/// external URLs (`url(http://...)`, `url(https://...)`, `url(//)`) and non-image data URLs
/// by replacing them with `url()` (empty, which CSS treats as invalid and ignores).
/// At-rules outside [`ALLOWED_AT_RULES`] are dropped whole, which is what removes
/// `@import` and anything else that could load a stylesheet.
fn sanitize_css_urls(css: &str) -> String {
    let css_after_import = strip_disallowed_at_rules(css);
    let lower_after_import = css_after_import.to_ascii_lowercase();
    let mut result = String::with_capacity(css_after_import.len());
    let mut offset = 0;

    while let Some(start) = lower_after_import[offset..].find("url(") {
        result.push_str(&css_after_import[offset..offset + start]);
        let url_open = offset + start + 4;
        let after_url = &css_after_import[url_open..];

        let (url_value, rest) = extract_css_url_value(after_url);
        let consumed = after_url.len() - rest.len();
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
        offset = url_open + consumed;
    }

    result.push_str(&css_after_import[offset..]);
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

/// The size a pre-parsed SVG tree describes, which is where a resize starts from.
///
/// This is the equivalent of a raster source's stored dimensions: the fit mode, the
/// enlargement policy, and the requested box are applied to it by the shared resize
/// helpers, so a vector source and a raster source of the same size answer alike.
/// The size to rasterize the drawing at so that rotating the result lands on `target`.
///
/// `target` is the size the fit mode scales the rotated drawing to, so this maps it back
/// through the rotation. A quarter turn is exact: it either leaves the axes alone or swaps
/// them. Any other angle grows the canvas to a bounding box that no size maps onto exactly,
/// so the drawing is scaled uniformly by the larger of the two ratios, which is never
/// coarser than the output needs; `apply_resize` corrects the remaining pixel afterwards.
fn pre_rotation_render_size(
    target: (u32, u32),
    intrinsic: (u32, u32),
    rotated: (u32, u32),
    degrees: u16,
) -> (u32, u32) {
    match degrees {
        0 | 180 => target,
        90 | 270 => (target.1, target.0),
        _ => {
            let scale = f64::max(
                f64::from(target.0) / f64::from(rotated.0.max(1)),
                f64::from(target.1) / f64::from(rotated.1.max(1)),
            );
            let scaled = |value: u32| {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let scaled = (f64::from(value) * scale).round() as u32;
                scaled.max(1)
            };
            (scaled(intrinsic.0), scaled(intrinsic.1))
        }
    }
}

fn intrinsic_render_size(tree: &resvg::usvg::Tree) -> (u32, u32) {
    let size = tree.size();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let width = size.width() as u32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let height = size.height() as u32;

    // A document that resolves to no size still has to be drawn somewhere. 300x150 is the
    // replaced-element default a browser uses for the same situation.
    (
        if width > 0 { width } else { 300 },
        if height > 0 { height } else { 150 },
    )
}

/// Rasterizes a pre-parsed SVG tree into an RGBA pixel buffer using `resvg`.
fn rasterize_svg(
    tree: &resvg::usvg::Tree,
    width: u32,
    height: u32,
    region: Option<crate::core::CropRegion>,
) -> Result<RgbaImage, TransformError> {
    // `width` and `height` are the size the whole drawing would be rendered at, which is
    // what fixes the scale. A region shifts the drawing under a buffer of the region's own
    // size, so a crop allocates what its output needs rather than what its source is.
    let (buffer_width, buffer_height) = match region {
        Some(region) => (region.width, region.height),
        None => (width, height),
    };
    let mut pixmap =
        resvg::tiny_skia::Pixmap::new(buffer_width, buffer_height).ok_or_else(|| {
            TransformError::DecodeFailed(format!(
                "failed to create {buffer_width}x{buffer_height} pixel buffer for SVG rasterization"
            ))
        })?;

    let scale_x = width as f32 / tree.size().width();
    let scale_y = height as f32 / tree.size().height();
    let scale = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);
    let transform = match region {
        Some(region) => {
            resvg::tiny_skia::Transform::from_translate(-(region.x as f32), -(region.y as f32))
                .pre_concat(scale)
        }
        None => scale,
    };

    resvg::render(tree, transform, &mut pixmap.as_mut());

    // resvg produces premultiplied RGBA. Convert to straight alpha for image crate.
    let mut rgba_data = pixmap.take();
    // `as_chunks_mut` over `chunks_exact_mut(4)`: the chunk size is a constant, so this
    // yields `[u8; 4]` and the indexing below needs no bounds checks. The remainder is
    // empty by construction, since a pixmap buffer is always a whole number of pixels.
    let (pixels, _) = rgba_data.as_chunks_mut::<4>();
    for pixel in pixels {
        let a = u16::from(pixel[3]);
        if a > 0 && a < 255 {
            pixel[0] = ((u16::from(pixel[0]) * 255 + a / 2) / a).min(255) as u8;
            pixel[1] = ((u16::from(pixel[1]) * 255 + a / 2) / a).min(255) as u8;
            pixel[2] = ((u16::from(pixel[2]) * 255 + a / 2) / a).min(255) as u8;
        }
    }

    RgbaImage::from_raw(buffer_width, buffer_height, rgba_data)
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
            #[cfg(feature = "avif")]
            {
                let quality = quality.unwrap_or(80);
                let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(
                    &mut bytes, 4, quality,
                );
                encoder
                    .write_image(image.as_ref(), width, height, ColorType::Rgba8.into())
                    .map_err(|e| {
                        TransformError::EncodeFailed(format!("AVIF encode failed: {e}"))
                    })?;
            }
            #[cfg(not(feature = "avif"))]
            {
                let _ = quality;
                return Err(TransformError::CapabilityMissing(
                    "AVIF encoding is not enabled in this build".to_string(),
                ));
            }
        }
        MediaType::Bmp => {
            let encoder = image::codecs::bmp::BmpEncoder::new(&mut bytes);
            encoder
                .write_image(image.as_ref(), width, height, ColorType::Rgba8.into())
                .map_err(|e| TransformError::EncodeFailed(format!("BMP encode failed: {e}")))?;
        }
        MediaType::Tiff => {
            let mut cursor = std::io::Cursor::new(bytes);
            image::codecs::tiff::TiffEncoder::new(&mut cursor)
                .write_image(image.as_ref(), width, height, ColorType::Rgba8.into())
                .map_err(|e| TransformError::EncodeFailed(format!("TIFF encode failed: {e}")))?;
            bytes = cursor.into_inner();
        }
        MediaType::Svg => {
            return Err(TransformError::InvalidOptions(
                "SVG-to-SVG rasterization is not meaningful".into(),
            ));
        }
        MediaType::Gif => {
            return Err(TransformError::UnsupportedOutputMediaType(MediaType::Gif));
        }
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Fit, Position, RawArtifact, Rotation, TransformOptions, sniff_artifact};
    use rstest::rstest;

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

    /// A 100x100 drawing, so a rotation that transposes the axes leaves the intrinsic size
    /// alone and only the requested box can explain the output size.
    fn square_svg() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"><rect width=\"100\" height=\"100\" fill=\"red\"/></svg>".to_vec()
    }

    fn simple_svg() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"20\" height=\"10\"><rect width=\"20\" height=\"10\" fill=\"blue\"/></svg>".to_vec()
    }

    #[test]
    fn sanitize_removes_animation_elements() {
        for element in ["animate", "set", "animateTransform", "animateMotion"] {
            let svg = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\"><a><{element} attributeName=\"href\" to=\"#a\"/><text>x</text></a></svg>"
            );
            let result = sanitize_svg(svg.as_bytes()).unwrap();
            assert!(
                !result
                    .to_ascii_lowercase()
                    .contains(&element.to_ascii_lowercase()),
                "<{element}> should be removed, got: {result}"
            );
            assert!(result.contains("<text"), "<text> should be preserved");
        }
    }

    /// SMIL sets attributes at render time, so a value the attribute filter would reject
    /// must not survive by arriving through `to`, `values`, `from`, or `by`.
    #[test]
    fn sanitize_removes_javascript_uri_in_animation_values() {
        for attribute in ["to", "values", "from", "by"] {
            let svg = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\"><a><animate attributeName=\"href\" {attribute}=\"javascript:alert(1)\" begin=\"0s\"/><text>click</text></a></svg>"
            );
            let result = sanitize_svg(svg.as_bytes()).unwrap();
            assert!(
                !result.contains("javascript:"),
                "javascript: survived through {attribute}: {result}"
            );
        }
    }

    /// The same mechanism restores external references, which the sanitizer also removes.
    #[test]
    fn sanitize_removes_external_reference_in_animation_values() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><image x="0" y="0" width="100" height="100"><set attributeName="href" to="https://evil.example.com/track.png" begin="0s"/></image></svg>"#;
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("evil.example.com"),
            "external reference survived: {result}"
        );
    }

    /// `xlink:href` as the animated attribute name is the same attack.
    #[test]
    fn sanitize_removes_animation_targeting_xlink_href() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><a><set attributeName="xlink:href" to="javascript:alert(1)"/><text>x</text></a></svg>"#;
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("javascript:"),
            "javascript: survived: {result}"
        );
    }

    /// `<handler>` is SVG Tiny's script container.
    #[test]
    fn sanitize_removes_handler_element() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><handler type="text/javascript">alert(1)</handler><rect/></svg>"#;
        let result = sanitize_svg(svg).unwrap();
        assert!(!result.contains("handler"), "handler survived: {result}");
        assert!(!result.contains("alert"), "script body survived: {result}");
        assert!(result.contains("<rect"), "rect should be preserved");
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

    /// A 100x50 drawing with a red disc that touches the top and bottom edges. The disc is
    /// what makes a stretch visible: under any aspect-preserving fit its extent is equal on
    /// both axes, and under a stretch it is not.
    fn disc_svg() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"50\" viewBox=\"0 0 100 50\"><circle cx=\"50\" cy=\"25\" r=\"25\" fill=\"#ff0000\"/></svg>".to_vec()
    }

    fn render_svg(bytes: Vec<u8>, options: TransformOptions) -> image::RgbaImage {
        let input = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff svg");
        let result =
            transform_svg(TransformRequest::new(input, options)).expect("rasterize should succeed");
        image::load_from_memory(&result.artifact.bytes)
            .expect("decode output")
            .to_rgba8()
    }

    /// Returns the inclusive extent of the opaque red pixels along the middle row and the
    /// middle column, as a count of pixels on each axis.
    fn red_extent(image: &image::RgbaImage) -> (u32, u32) {
        let (width, height) = (image.width(), image.height());
        let horizontal = (0..width)
            .filter(|&x| is_red(image.get_pixel(x, height / 2)))
            .count() as u32;
        let vertical = (0..height)
            .filter(|&y| is_red(image.get_pixel(width / 2, y)))
            .count() as u32;
        (horizontal, vertical)
    }

    fn is_red(pixel: &image::Rgba<u8>) -> bool {
        pixel[3] > 128 && pixel[0] > 150 && pixel[1] < 100 && pixel[2] < 100
    }

    #[rstest]
    #[case(Fit::Contain, 100, 100)]
    #[case(Fit::Cover, 100, 100)]
    #[case(Fit::Fill, 100, 100)]
    #[case(Fit::Inside, 100, 50)]
    fn svg_rasterization_honours_the_fit_mode_dimensions(
        #[case] fit: Fit,
        #[case] expected_width: u32,
        #[case] expected_height: u32,
    ) {
        let image = render_svg(
            disc_svg(),
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(100),
                height: Some(100),
                fit: Some(fit),
                ..TransformOptions::default()
            },
        );

        assert_eq!(
            (image.width(), image.height()),
            (expected_width, expected_height),
            "{fit:?} produced the wrong canvas"
        );
    }

    /// Dimensions alone cannot separate contain, cover, and fill: all three return the
    /// requested box. What separates them is where the drawing lands inside it, which is why
    /// this measures the disc rather than the canvas.
    #[test]
    fn svg_contain_preserves_the_aspect_ratio_and_pads_the_rest() {
        let image = render_svg(
            disc_svg(),
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(200),
                height: Some(200),
                ..TransformOptions::default()
            },
        );

        let (horizontal, vertical) = red_extent(&image);
        assert_eq!(
            horizontal, vertical,
            "contain stretched the disc: {horizontal}x{vertical}"
        );
        assert_eq!(
            image.get_pixel(0, 0)[3],
            0,
            "contain should leave the padding transparent"
        );
    }

    #[test]
    fn svg_fill_stretches_each_axis_on_its_own() {
        let image = render_svg(
            disc_svg(),
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(200),
                height: Some(200),
                fit: Some(Fit::Fill),
                ..TransformOptions::default()
            },
        );

        let (horizontal, vertical) = red_extent(&image);
        assert!(
            vertical > horizontal + 40,
            "fill should stretch the disc, got {horizontal}x{vertical}"
        );
    }

    #[test]
    fn svg_without_enlargement_keeps_the_intrinsic_scale() {
        let image = render_svg(
            disc_svg(),
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(200),
                height: Some(200),
                without_enlargement: true,
                ..TransformOptions::default()
            },
        );

        let (horizontal, vertical) = red_extent(&image);
        assert_eq!((image.width(), image.height()), (200, 200));
        assert!(
            (45..=51).contains(&horizontal) && (45..=51).contains(&vertical),
            "the disc should stay at its intrinsic 50 pixels, got {horizontal}x{vertical}"
        );
    }

    /// Cover crops, so the anchor decides which part survives. The disc sits in the middle of
    /// a 100x50 drawing, so cropping to a tall box from the left edge loses it and cropping
    /// from the centre keeps it.
    #[test]
    fn svg_cover_crops_at_the_requested_position() {
        let options = |position| TransformOptions {
            format: Some(MediaType::Png),
            width: Some(20),
            height: Some(50),
            fit: Some(Fit::Cover),
            position,
            ..TransformOptions::default()
        };

        let centred = render_svg(disc_svg(), options(Some(Position::Center)));
        let left = render_svg(disc_svg(), options(Some(Position::Left)));

        assert_eq!((centred.width(), centred.height()), (20, 50));
        assert!(
            red_extent(&centred).0 > 0,
            "the centre crop should hold the disc"
        );
        assert_eq!(
            red_extent(&left).0,
            0,
            "the left crop should not hold the disc"
        );
    }

    /// The intermediate buffer cover materializes is larger than the box it returns, which is
    /// the shape of #316. The raster path checks it from dimensions alone; so must this one.
    #[test]
    fn svg_cover_checks_the_pre_crop_buffer_against_the_limit() {
        let input = sniff_artifact(RawArtifact::new(
            b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10000\" height=\"1\"><rect width=\"10000\" height=\"1\" fill=\"blue\"/></svg>".to_vec(),
            None,
        ))
        .expect("sniff svg");
        let err = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(3),
                height: Some(9999),
                fit: Some(Fit::Cover),
                ..TransformOptions::default()
            },
        ))
        .expect_err("the pre-crop buffer exceeds the output pixel limit");

        assert!(
            matches!(err, TransformError::LimitExceeded(_)),
            "expected LimitExceeded, got: {err}"
        );
    }

    /// `inspect` reads the size through `sniff_artifact` and `convert` reads it through
    /// `usvg`, so the two can drift. Issue #322 closed that gap for EXIF orientation by
    /// having one function answer for both; here the two readers stay separate on purpose —
    /// parsing a whole tree at sniff time would run before the sanitizer — so the agreement
    /// is asserted instead.
    #[rstest]
    #[case(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#)]
    #[case(r#"<svg xmlns="http://www.w3.org/2000/svg" width="1in" height="2in"/>"#)]
    #[case(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 60"/>"#)]
    #[case(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%" viewBox="0 0 30 20"/>"#)]
    #[case(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" viewBox="0 0 30 20"/>"#)]
    fn sniffed_svg_dimensions_match_the_size_it_rasterizes_at(#[case] document: &str) {
        let input = sniff_artifact(RawArtifact::new(document.as_bytes().to_vec(), None))
            .expect("sniff svg");
        let sniffed = input
            .metadata
            .width
            .zip(input.metadata.height)
            .expect("these documents declare an absolute size");

        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("rasterize at the intrinsic size");

        assert_eq!(
            (
                result.artifact.metadata.width,
                result.artifact.metadata.height
            ),
            (Some(sniffed.0), Some(sniffed.1))
        );
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
                rotate: Rotation::DEG_90,
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

    /// The same picture as a PNG of the drawing's own size, so the two codecs can be asked
    /// the same question. Whatever the fit modes mean, they have to mean it in both.
    fn square_png() -> Artifact {
        let input = sniff_artifact(RawArtifact::new(square_svg(), None)).unwrap();
        let rasterized = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("rasterize the reference drawing");
        sniff_artifact(RawArtifact::new(rasterized.artifact.bytes, None)).expect("sniff png")
    }

    #[rstest]
    #[case(Fit::Contain, 90)]
    #[case(Fit::Cover, 90)]
    #[case(Fit::Fill, 90)]
    #[case(Fit::Inside, 90)]
    #[case(Fit::Contain, 45)]
    #[case(Fit::Inside, 270)]
    fn transform_svg_rotates_before_it_resizes(#[case] fit: Fit, #[case] degrees: i32) {
        // The documented order is rotate then resize, so the turn happens first and the
        // resize fits the turned drawing into the box. Turning the finished canvas instead
        // returns the rotated bounding box of the box, which is a different size from the
        // one the caller named, and which no raster source would have returned.
        let options = TransformOptions {
            format: Some(MediaType::Png),
            rotate: Rotation::from_degrees(degrees),
            width: Some(200),
            height: Some(100),
            fit: Some(fit),
            ..TransformOptions::default()
        };

        let input = sniff_artifact(RawArtifact::new(square_svg(), None)).unwrap();
        let from_svg = transform_svg(TransformRequest::new(input, options.clone()))
            .expect("SVG to PNG with rotate and a box should succeed");
        let from_png =
            crate::codecs::raster::transform_raster(TransformRequest::new(square_png(), options))
                .expect("PNG to PNG with rotate and a box should succeed");

        assert_eq!(
            (
                from_svg.artifact.metadata.width,
                from_svg.artifact.metadata.height
            ),
            (
                from_png.artifact.metadata.width,
                from_png.artifact.metadata.height
            ),
            "{fit:?} at {degrees} degrees must answer the same for both codecs"
        );
    }

    #[test]
    fn transform_svg_to_png_with_grayscale() {
        // simple_svg() is a solid blue rect, so every rasterized pixel must come out
        // neutral once desaturated.
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                grayscale: true,
                ..TransformOptions::default()
            },
        ))
        .expect("SVG to PNG with grayscale should succeed");

        let output =
            image::load_from_memory_with_format(&result.artifact.bytes, image::ImageFormat::Png)
                .expect("decode rasterized output")
                .to_rgba8();
        for (x, y, pixel) in output.enumerate_pixels() {
            assert!(
                pixel[0] == pixel[1] && pixel[1] == pixel[2],
                "pixel ({x},{y}) is not neutral gray: {pixel:?}"
            );
        }
    }

    #[test]
    fn transform_svg_to_png_with_rotate_180() {
        // 180 degrees should preserve dimensions.
        let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                rotate: Rotation::DEG_180,
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
                orientation: None,
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

    // --- Prolog constructs ---

    /// A processing instruction is honoured by a browser rendering the document.
    /// `xml-stylesheet` loads an external stylesheet, and an XSLT one generates
    /// arbitrary markup, so it defeats every element and attribute rule at once.
    #[rstest]
    #[case::xslt_stylesheet(
        "<?xml-stylesheet type=\"text/xsl\" href=\"https://evil.example/x.xsl\"?>"
    )]
    #[case::css_stylesheet(
        "<?xml-stylesheet type=\"text/css\" href=\"https://evil.example/x.css\"?>"
    )]
    #[case::unknown_target("<?evil-target data=\"https://evil.example/x\"?>")]
    fn sanitize_removes_processing_instructions(#[case] instruction: &str) {
        let svg = format!(
            "<?xml version=\"1.0\"?>\n{instruction}\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
        );
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            !result.contains("evil.example") && !result.contains("<?xml-stylesheet"),
            "processing instruction should be removed, got: {result}"
        );
    }

    /// The href and style rules match on the namespace-stripped local name, so
    /// the event handler rule has to as well.
    #[rstest]
    #[case::plain("onload")]
    #[case::mixed_case("oNlOaD")]
    #[case::uppercase("ONCLICK")]
    #[case::namespaced("xlink:onload")]
    #[case::namespaced_unknown_prefix("evil:onclick")]
    fn sanitize_removes_event_handlers_under_any_prefix(#[case] attribute: &str) {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xmlns:evil=\"urn:e\"><rect {attribute}=\"alert(1)\"/></svg>"
        );
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            !result.contains("alert(1)"),
            "`{attribute}` should be removed, got: {result}"
        );
    }

    /// The rule is `on` followed by a letter, so an attribute that only shares a
    /// prefix with that shape is left alone.
    #[rstest]
    #[case::opacity("opacity")]
    #[case::offset("offset")]
    #[case::on_alone("on")]
    fn sanitize_keeps_attributes_that_are_not_event_handlers(#[case] attribute: &str) {
        let svg =
            format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><rect {attribute}=\"1\"/></svg>");
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            result.contains(attribute),
            "`{attribute}` is not an event handler and should survive: {result}"
        );
    }

    #[test]
    fn sanitize_keeps_the_xml_declaration() {
        let svg = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            result.contains("<?xml version="),
            "the declaration is not a processing instruction to strip: {result}"
        );
    }

    /// An internal subset declaring an external entity is an XXE payload being
    /// carried through a document truss has called safe. Removing only the
    /// declarations would leave the references to them dangling and the output no
    /// longer well-formed, so the document is refused instead.
    #[rstest]
    #[case::external_entity(
        "<!DOCTYPE svg [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><text>&xxe;</text></svg>"
    )]
    #[case::nested_entities(
        "<!DOCTYPE svg [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;&a;&a;\">]>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><text>&b;</text></svg>"
    )]
    fn sanitize_rejects_a_doctype_declaring_external_or_nested_entities(#[case] document: &str) {
        let err = sanitize_svg(document.as_bytes())
            .expect_err("document should be refused, not laundered");
        assert!(
            matches!(err, TransformError::DecodeFailed(ref msg) if msg.contains("external or nested entities")),
            "expected a doctype refusal, got: {err:?}"
        );
    }

    #[test]
    fn sanitize_keeps_a_doctype_declaring_literal_entities() {
        let svg = b"<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\" [<!ENTITY ns_extend \"http://ns.adobe.com/Extensibility/1.0/\">]>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            result.contains("ns_extend"),
            "an editor's literal entity declarations must survive so references to them resolve: {result}"
        );
    }

    // --- Presentation attributes carry the same url() as `style` ---

    /// Every SVG presentation attribute that takes a `<funciri>` is another
    /// spelling of the same CSS declaration, so the sanitizer has to give the
    /// two spellings the same answer.
    #[rstest]
    #[case::fill("fill")]
    #[case::stroke("stroke")]
    #[case::filter("filter")]
    #[case::mask("mask")]
    #[case::clip_path("clip-path")]
    #[case::marker_start("marker-start")]
    #[case::marker_mid("marker-mid")]
    #[case::marker_end("marker-end")]
    #[case::cursor("cursor")]
    fn sanitize_removes_external_url_from_presentation_attributes(#[case] attribute: &str) {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect {attribute}=\"url(https://evil.example/x.svg#r)\"/></svg>"
        );
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            !result.contains("evil.example"),
            "external url() in `{attribute}` should be removed, got: {result}"
        );

        let styled = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect style=\"{attribute}:url(https://evil.example/x.svg#r)\"/></svg>"
        );
        let styled_result = sanitize_svg(styled.as_bytes()).unwrap();
        assert!(
            !styled_result.contains("evil.example"),
            "the `style` spelling was already handled and must stay handled: {styled_result}"
        );
    }

    /// Internal references are what these attributes are normally for; removing
    /// them would break every gradient, clip path, and filter in the document
    /// without closing anything.
    #[rstest]
    #[case::fill("fill", "url(#gradient1)", "#gradient1")]
    #[case::filter("filter", "url(#blur)", "#blur")]
    #[case::clip_path("clip-path", "url(#clip)", "#clip")]
    #[case::plain_colour("fill", "red", "red")]
    #[case::data_image("fill", "url(data:image/png;base64,iVBORw0KGgo=)", "data:image/png")]
    fn sanitize_keeps_safe_presentation_attribute_values(
        #[case] attribute: &str,
        #[case] value: &str,
        #[case] expected: &str,
    ) {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><rect {attribute}=\"{value}\"/></svg>"
        );
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            result.contains(expected),
            "`{attribute}=\"{value}\"` should survive, got: {result}"
        );
    }

    #[test]
    fn sanitize_removes_embedded_svg_data_url_from_a_presentation_attribute() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><rect fill=\"url(data:image/svg+xml,%3Csvg%3E)\"/></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("image/svg"),
            "a data: URL that smuggles another SVG should be removed: {result}"
        );
    }

    // --- @import survives however the at-keyword is spelled ---

    /// CSS identifiers admit escapes, so `@\\69 mport` and `@\\import` are the same
    /// at-rule as `@import`. The string form carries no `url()`, so the separate
    /// url() pass does not catch what the at-rule search misses.
    #[rstest]
    #[case::plain("@import \"https://evil.example/x.css\";")]
    #[case::plain_url("@import url(\"https://evil.example/x.css\");")]
    #[case::uppercase("@IMPORT \"https://evil.example/x.css\";")]
    #[case::hex_escape("@\\69 mport \"https://evil.example/x.css\";")]
    #[case::hex_escape_padded("@\\000069 mport \"https://evil.example/x.css\";")]
    #[case::backslash_escape("@\\import \"https://evil.example/x.css\";")]
    #[case::escape_mid_keyword("@im\\70 ort \"https://evil.example/x.css\";")]
    fn sanitize_removes_at_import_however_it_is_spelled(#[case] css: &str) {
        let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><style>{css}</style></svg>");
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            !result.contains("evil.example"),
            "external stylesheet should be removed from `{css}`, got: {result}"
        );
    }

    /// Rewriting the stylesheet must not corrupt the text it copies through, and
    /// must not leave an offset inside a multi-byte character for the caller to
    /// slice on.
    #[rstest]
    #[case::accented_string("rect::after { content: \"caf\u{e9}\" }", "caf\u{e9}")]
    #[case::emoji_string("rect::after { content: \"\u{1f600}\" }", "\u{1f600}")]
    #[case::accent_after_escape("rect::after { content: \"\\\\\u{e9}\" }", "\u{e9}")]
    #[case::accent_inside_a_dropped_rule(
        "@import \"https://evil.example/\u{e9}.css\"; rect { fill: red }",
        "fill: red"
    )]
    fn sanitize_preserves_non_ascii_css_text(#[case] css: &str, #[case] expected: &str) {
        let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><style>{css}</style></svg>");
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            result.contains(expected),
            "`{css}` should keep `{expected}` intact, got: {result}"
        );
    }

    /// The fix must not empty every `<style>` element it does not understand.
    #[rstest]
    #[case::plain_rule("rect { fill: red }", "fill: red")]
    #[case::media_query("@media screen { rect { fill: red } }", "fill: red")]
    #[case::local_url("rect { fill: url(#gradient1) }", "#gradient1")]
    #[case::at_in_a_string("rect::after { content: \"a@import b\" }", "rect::after")]
    fn sanitize_keeps_stylesheets_with_no_external_reference(
        #[case] css: &str,
        #[case] expected: &str,
    ) {
        let svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\"><style>{css}</style></svg>");
        let result = sanitize_svg(svg.as_bytes()).unwrap();
        assert!(
            result.contains(expected),
            "`{css}` should keep `{expected}`, got: {result}"
        );
    }

    /// The url() rule allowlists `#fragment` and `data:image/*`, so a scheme
    /// written with an escape is already rejected for not being on the list; pin
    /// that, so this path stays closed whichever way the at-rule search is fixed.
    #[test]
    fn sanitize_removes_an_escaped_scheme_from_a_css_url() {
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><style>rect { fill: url(\\68 ttps://evil.example/x.png) }</style></svg>";
        let result = sanitize_svg(svg).unwrap();
        assert!(
            !result.contains("evil.example"),
            "escaped scheme in url() should be removed: {result}"
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

    /// A rasterized SVG reaches the pixel stages, which is what `docs/pipeline.md` says
    /// happens: the drawing is rasterized and joins the raster pipeline. Rotation at an
    /// arbitrary angle and grayscale already did; blur, sharpen, crop, and watermark were
    /// refused before the rasterization they run after, which cut the pipeline in two places
    /// for one input class and in none for the others.
    #[test]
    fn a_rasterized_svg_reaches_the_pixel_stages() {
        let plain = render_svg_to_png(TransformOptions {
            format: Some(MediaType::Png),
            width: Some(40),
            height: Some(40),
            ..TransformOptions::default()
        });

        let cases: &[(&str, TransformOptions)] = &[
            (
                "blur",
                TransformOptions {
                    format: Some(MediaType::Png),
                    width: Some(40),
                    height: Some(40),
                    blur: Some(3.0),
                    ..TransformOptions::default()
                },
            ),
            (
                "sharpen",
                TransformOptions {
                    format: Some(MediaType::Png),
                    width: Some(40),
                    height: Some(40),
                    sharpen: Some(3.0),
                    ..TransformOptions::default()
                },
            ),
        ];

        for (name, options) in cases {
            let produced = render_svg_to_png(options.clone());
            assert_ne!(
                produced, plain,
                "{name} was accepted but changed nothing about the picture"
            );
        }
    }

    /// A crop of a rasterized SVG is the same crop of the same drawing rasterized first.
    ///
    /// `docs/pipeline.md` says a drawing with a raster output joins the raster pipeline, so
    /// the raster path is the oracle: it decodes at the source's own size, crops, and
    /// resizes. Scaling the rectangle into the space the drawing is rasterized in has to
    /// land on the same pixels.
    #[test]
    fn a_crop_of_a_rasterized_svg_matches_the_same_crop_of_the_raster() {
        for (width, height) in [(10_u32, 10_u32), (40, 24), (24, 40), (33, 7)] {
            let drawing = marked_svg(width, height);
            let whole = transform_svg(TransformRequest::new(
                sniff_artifact(RawArtifact::new(drawing.clone(), None)).unwrap(),
                TransformOptions {
                    format: Some(MediaType::Png),
                    ..TransformOptions::default()
                },
            ))
            .expect("rasterize the drawing")
            .artifact
            .bytes;

            for (x, y, w, h) in [
                (0, 0, width, height),
                (0, 0, 1, 1),
                (0, 0, width / 2, height / 2),
                (width - 1, height - 1, 1, 1),
                (1, 1, width - 1, height - 1),
                (width / 3, height / 3, width / 3, height / 3),
            ] {
                if w == 0 || h == 0 {
                    continue;
                }
                let crop = crate::core::CropRegion {
                    x,
                    y,
                    width: w,
                    height: h,
                };
                let options = TransformOptions {
                    format: Some(MediaType::Png),
                    crop: Some(crop),
                    ..TransformOptions::default()
                };
                let from_drawing = transform_svg(TransformRequest::new(
                    sniff_artifact(RawArtifact::new(drawing.clone(), None)).unwrap(),
                    options.clone(),
                ))
                .expect("crop the drawing")
                .artifact
                .bytes;
                let from_raster = crate::codecs::transform(TransformRequest::new(
                    sniff_artifact(RawArtifact::new(whole.clone(), None)).unwrap(),
                    options,
                ))
                .expect("crop the raster")
                .artifact
                .bytes;

                let a = image::load_from_memory(&from_drawing).unwrap().to_rgba8();
                let b = image::load_from_memory(&from_raster).unwrap().to_rgba8();
                assert_eq!(
                    a.dimensions(),
                    b.dimensions(),
                    "{width}x{height} crop {x},{y},{w},{h}: the two paths disagree on the size"
                );
                assert_eq!(
                    a.into_raw(),
                    b.into_raw(),
                    "{width}x{height} crop {x},{y},{w},{h}: the two paths disagree on the pixels"
                );
            }
        }
    }

    /// A small region of a large drawing is served, not refused for a buffer nobody asked
    /// for.
    #[test]
    fn a_small_crop_of_a_large_drawing_is_served() {
        let drawing = marked_svg(1000, 3);
        let result = transform_svg(TransformRequest::new(
            sniff_artifact(RawArtifact::new(drawing, None)).unwrap(),
            TransformOptions {
                format: Some(MediaType::Png),
                crop: Some(crate::core::CropRegion {
                    x: 0,
                    y: 0,
                    width: 250,
                    height: 1,
                }),
                width: Some(200),
                height: Some(200),
                fit: Some(crate::core::Fit::Cover),
                ..TransformOptions::default()
            },
        ))
        .expect("a 200x200 output should not need 120 million pixels");

        assert_eq!(result.artifact.metadata.width, Some(200));
        assert_eq!(result.artifact.metadata.height, Some(200));
    }

    /// A drawing with a mark in every corner, so a rectangle that is off by a pixel shows it.
    fn marked_svg(width: u32, height: u32) -> Vec<u8> {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\">\
             <rect width=\"{width}\" height=\"{height}\" fill=\"white\"/>\
             <rect x=\"0\" y=\"0\" width=\"1\" height=\"1\" fill=\"red\"/>\
             <rect x=\"{last_x}\" y=\"0\" width=\"1\" height=\"1\" fill=\"lime\"/>\
             <rect x=\"0\" y=\"{last_y}\" width=\"1\" height=\"1\" fill=\"blue\"/>\
             <rect x=\"{last_x}\" y=\"{last_y}\" width=\"1\" height=\"1\" fill=\"black\"/></svg>",
            last_x = width - 1,
            last_y = height - 1,
        )
        .into_bytes()
    }

    /// A crop takes its bite before the resize, so the output is the box that was asked for
    /// and the picture in it is the region that was named.
    #[test]
    fn a_rasterized_svg_can_be_cropped() {
        let input = sniff_artifact(RawArtifact::new(contrasting_svg(), None)).unwrap();
        let result = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                crop: Some(crate::core::CropRegion {
                    x: 0,
                    y: 0,
                    width: 20,
                    height: 10,
                }),
                width: Some(20),
                height: Some(10),
                fit: Some(crate::core::Fit::Fill),
                ..TransformOptions::default()
            },
        ))
        .expect("a crop of a rasterized SVG");

        assert_eq!(result.artifact.metadata.width, Some(20));
        assert_eq!(result.artifact.metadata.height, Some(10));

        // The region that was named is the region that comes back: the top-left quarter of
        // this drawing holds the black bar, and the bottom-right does not.
        let top_left = decoded_pixels(&result.artifact.bytes);
        let bottom_right = decoded_pixels(
            &transform_svg(TransformRequest::new(
                sniff_artifact(RawArtifact::new(contrasting_svg(), None)).unwrap(),
                TransformOptions {
                    format: Some(MediaType::Png),
                    crop: Some(crate::core::CropRegion {
                        x: 20,
                        y: 30,
                        width: 20,
                        height: 10,
                    }),
                    width: Some(20),
                    height: Some(10),
                    fit: Some(crate::core::Fit::Fill),
                    ..TransformOptions::default()
                },
            ))
            .expect("the other corner")
            .artifact
            .bytes,
        );
        assert_ne!(
            top_left, bottom_right,
            "two different regions of the drawing gave the same picture"
        );
    }

    fn decoded_pixels(png: &[u8]) -> Vec<u8> {
        image::load_from_memory(png)
            .expect("decode png")
            .to_rgba8()
            .into_raw()
    }

    /// A crop rectangle is measured against the drawing's own size, which is the size
    /// `truss inspect` reports, and one that runs past it is refused with the raster
    /// pipeline's own message.
    #[test]
    fn a_crop_past_the_drawing_is_refused() {
        let input = sniff_artifact(RawArtifact::new(contrasting_svg(), None)).unwrap();
        let error = transform_svg(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                crop: Some(crate::core::CropRegion {
                    x: 90,
                    y: 90,
                    width: 50,
                    height: 50,
                }),
                ..TransformOptions::default()
            },
        ))
        .expect_err("a crop past the drawing is out of bounds");

        assert!(
            matches!(error, TransformError::InvalidOptions(ref message)
                if message.contains("exceeds image bounds")),
            "expected the pipeline's own out-of-bounds message, got: {error}"
        );
    }

    /// With SVG output there is no pipeline at all, so the four stay refused there.
    #[test]
    fn svg_output_still_refuses_the_pixel_stages() {
        let cases: &[(&str, TransformOptions)] = &[
            (
                "blur",
                TransformOptions {
                    format: Some(MediaType::Svg),
                    blur: Some(2.0),
                    ..TransformOptions::default()
                },
            ),
            (
                "sharpen",
                TransformOptions {
                    format: Some(MediaType::Svg),
                    sharpen: Some(2.0),
                    ..TransformOptions::default()
                },
            ),
            (
                "crop",
                TransformOptions {
                    format: Some(MediaType::Svg),
                    crop: Some(crate::core::CropRegion {
                        x: 0,
                        y: 0,
                        width: 10,
                        height: 10,
                    }),
                    ..TransformOptions::default()
                },
            ),
        ];

        for (name, options) in cases {
            let input = sniff_artifact(RawArtifact::new(simple_svg(), None)).unwrap();
            let error = transform_svg(TransformRequest::new(input, options.clone()))
                .expect_err("SVG output cannot honour a pixel stage");
            assert!(
                matches!(error, TransformError::InvalidOptions(ref message)
                    if message.contains(name)),
                "{name} should be refused for SVG output, got: {error}"
            );
        }
    }

    /// A drawing with an edge in it, since a filter leaves a uniform colour alone.
    fn contrasting_svg() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"40\" height=\"40\">\
          <rect width=\"40\" height=\"40\" fill=\"white\"/>\
          <rect x=\"4\" y=\"4\" width=\"16\" height=\"32\" fill=\"black\"/>\
          <circle cx=\"30\" cy=\"12\" r=\"7\" fill=\"red\"/></svg>"
            .to_vec()
    }

    fn render_svg_to_png(options: TransformOptions) -> Vec<u8> {
        let input = sniff_artifact(RawArtifact::new(contrasting_svg(), None)).unwrap();
        transform_svg(TransformRequest::new(input, options))
            .expect("rasterize svg")
            .artifact
            .bytes
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
