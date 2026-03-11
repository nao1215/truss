//! Browser and WebAssembly adapter support.
//!
//! This module keeps the browser-facing contract separate from the core Rust types so the
//! GitHub Pages demo can exchange simple JSON-like objects with JavaScript while still
//! reusing the shared transformation pipeline.

use crate::{
    Artifact, MediaType, Position, RawArtifact, Rgba8, Rotation, TransformError, TransformOptions,
    TransformRequest, TransformResult, WatermarkInput, sniff_artifact, transform_raster,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[cfg(feature = "svg")]
use crate::transform_svg;
#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

/// Browser-facing transform options accepted by the WASM adapter.
///
/// The fields intentionally use strings for enum-like values so JavaScript callers do not
/// need to understand the Rust enum layout. The adapter validates and converts these fields
/// before calling the shared Core transformation pipeline.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmTransformOptions {
    /// The requested output width in pixels.
    pub width: Option<u32>,
    /// The requested output height in pixels.
    pub height: Option<u32>,
    /// The resize fit mode (`contain`, `cover`, `fill`, or `inside`).
    pub fit: Option<String>,
    /// The crop anchor (`center`, `top-left`, and so on).
    pub position: Option<String>,
    /// The requested output format (`jpeg`, `png`, `webp`, `avif`, `bmp`, or `svg`).
    pub format: Option<String>,
    /// The requested lossy quality from 1 to 100.
    pub quality: Option<u8>,
    /// Optional background color as `RRGGBB` or `RRGGBBAA`.
    pub background: Option<String>,
    /// Optional clockwise rotation in degrees. Supported values are `0`, `90`, `180`, `270`.
    pub rotate: Option<u16>,
    /// Whether EXIF auto-orientation should run. Defaults to `true`.
    pub auto_orient: Option<bool>,
    /// Whether all supported metadata should be retained when possible.
    pub keep_metadata: Option<bool>,
    /// Whether only EXIF metadata should be retained.
    pub preserve_exif: Option<bool>,
    /// Gaussian blur sigma (0.1–100.0).
    pub blur: Option<f32>,
}

/// Build-time capabilities exposed by the WASM adapter.
///
/// The GitHub Pages UI uses this to disable controls for features that are intentionally
/// absent from the browser build, such as SVG processing or lossy WebP encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmCapabilities {
    /// Whether SVG input and SVG output processing are available in this build.
    pub svg: bool,
    /// Whether quality-controlled lossy WebP encoding is available in this build.
    pub webp_lossy: bool,
}

/// Serializable metadata about an inspected or transformed artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmArtifactInfo {
    /// Canonical media type name such as `png` or `jpeg`.
    pub media_type: String,
    /// MIME type string such as `image/png`.
    pub mime_type: String,
    /// Rendered width in pixels when known.
    pub width: Option<u32>,
    /// Rendered height in pixels when known.
    pub height: Option<u32>,
    /// Frame count for the artifact.
    pub frame_count: u32,
    /// Whether the artifact contains alpha when known.
    pub has_alpha: Option<bool>,
}

/// Response payload returned by [`inspect_browser_artifact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmInspectResponse {
    /// Inspected metadata for the supplied artifact.
    pub artifact: WasmArtifactInfo,
}

/// Response payload returned by [`transform_browser_artifact`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmTransformResponse {
    /// Transformed output bytes. JavaScript receives this as a `Uint8Array`.
    pub bytes: Vec<u8>,
    /// Metadata describing the transformed artifact.
    pub artifact: WasmArtifactInfo,
    /// Non-fatal warnings emitted by the transform pipeline.
    pub warnings: Vec<String>,
    /// Suggested output extension derived from the output media type.
    pub suggested_extension: String,
}

#[cfg(feature = "wasm")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WasmErrorPayload {
    kind: &'static str,
    message: String,
}

/// Returns the compile-time capabilities of the current WASM build.
pub fn browser_capabilities() -> WasmCapabilities {
    WasmCapabilities {
        svg: cfg!(feature = "svg"),
        webp_lossy: cfg!(feature = "webp-lossy"),
    }
}

/// Inspects browser-provided bytes and returns metadata suitable for JavaScript callers.
///
/// `declared_media_type` may be omitted. When present, it is validated against the detected
/// signature in the same way as the CLI and HTTP server adapters.
///
/// # Errors
///
/// Returns [`TransformError::InvalidInput`] when the declared media type conflicts with the
/// detected bytes, [`TransformError::UnsupportedInputMediaType`] when the bytes are not a
/// supported image format, and [`TransformError::DecodeFailed`] when the image structure is
/// malformed.
pub fn inspect_browser_artifact(
    input_bytes: Vec<u8>,
    declared_media_type: Option<&str>,
) -> Result<WasmInspectResponse, TransformError> {
    let artifact = sniff_browser_artifact(input_bytes, declared_media_type)?;

    Ok(WasmInspectResponse {
        artifact: artifact_info(&artifact),
    })
}

/// Transforms browser-provided bytes using JavaScript-friendly transform options.
///
/// This adapter intentionally excludes runtime-specific features such as local filesystem
/// paths, server-side URL fetches, and secret-backed authentication. It only accepts raw
/// input bytes and explicit transform options supplied by the browser application.
///
/// # Errors
///
/// Returns the same validation and execution errors as the shared transformation pipeline,
/// plus [`TransformError::CapabilityMissing`] when a requested browser feature was compiled out
/// of the current build, such as SVG processing or lossy WebP encoding.
pub fn transform_browser_artifact(
    input_bytes: Vec<u8>,
    declared_media_type: Option<&str>,
    options: WasmTransformOptions,
) -> Result<WasmTransformResponse, TransformError> {
    let artifact = sniff_browser_artifact(input_bytes, declared_media_type)?;
    let options = parse_wasm_options(options)?;
    let output = dispatch_browser_transform(artifact, options)?;
    let TransformResult { artifact, warnings } = output;
    let artifact_info = artifact_info(&artifact);
    let suggested_extension = output_extension(artifact.media_type).to_string();

    Ok(WasmTransformResponse {
        bytes: artifact.bytes,
        artifact: artifact_info,
        warnings: warnings
            .into_iter()
            .map(|warning| warning.to_string())
            .collect(),
        suggested_extension,
    })
}

fn sniff_browser_artifact(
    input_bytes: Vec<u8>,
    declared_media_type: Option<&str>,
) -> Result<Artifact, TransformError> {
    let declared_media_type = declared_media_type
        .map(|value| parse_media_type(value, "declaredMediaType"))
        .transpose()?;

    sniff_artifact(RawArtifact::new(input_bytes, declared_media_type))
}

fn parse_wasm_options(options: WasmTransformOptions) -> Result<TransformOptions, TransformError> {
    let (strip_metadata, preserve_exif) =
        crate::core::resolve_metadata_flags(None, options.keep_metadata, options.preserve_exif)?;

    let fit = parse_optional_enum(options.fit, "fit")?;
    let position = parse_optional_enum(options.position, "position")?;
    let format = options
        .format
        .as_deref()
        .map(|value| parse_media_type(value, "format"))
        .transpose()?;
    let background = options
        .background
        .as_deref()
        .map(|value| {
            Rgba8::from_hex(value).map_err(|reason| {
                TransformError::InvalidOptions(format!("background is invalid: {reason}"))
            })
        })
        .transpose()?;

    Ok(TransformOptions {
        width: options.width,
        height: options.height,
        fit,
        position,
        format,
        quality: options.quality,
        background,
        rotate: parse_rotation(options.rotate)?,
        auto_orient: options.auto_orient.unwrap_or(true),
        strip_metadata,
        preserve_exif,
        blur: options.blur,
        deadline: None,
    })
}

fn parse_optional_enum<T>(value: Option<String>, field: &str) -> Result<Option<T>, TransformError>
where
    T: FromStr<Err = String>,
{
    value
        .map(|value| {
            T::from_str(&value).map_err(|reason| {
                TransformError::InvalidOptions(format!("{field} is invalid: {reason}"))
            })
        })
        .transpose()
}

fn parse_media_type(value: &str, field: &str) -> Result<MediaType, TransformError> {
    MediaType::from_str(value)
        .map_err(|reason| TransformError::InvalidOptions(format!("{field} is invalid: {reason}")))
}

fn parse_rotation(value: Option<u16>) -> Result<Rotation, TransformError> {
    match value.unwrap_or(0) {
        0 => Ok(Rotation::Deg0),
        90 => Ok(Rotation::Deg90),
        180 => Ok(Rotation::Deg180),
        270 => Ok(Rotation::Deg270),
        other => Err(TransformError::InvalidOptions(format!(
            "rotate is invalid: unsupported rotation `{other}`"
        ))),
    }
}

fn dispatch_browser_transform(
    artifact: Artifact,
    options: TransformOptions,
) -> Result<TransformResult, TransformError> {
    dispatch_browser_transform_with_watermark(artifact, options, None)
}

fn dispatch_browser_transform_with_watermark(
    artifact: Artifact,
    options: TransformOptions,
    watermark: Option<WatermarkInput>,
) -> Result<TransformResult, TransformError> {
    if artifact.media_type != MediaType::Svg && options.format == Some(MediaType::Svg) {
        return Err(TransformError::UnsupportedOutputMediaType(MediaType::Svg));
    }

    if artifact.media_type == MediaType::Svg {
        #[cfg(feature = "svg")]
        {
            return transform_svg(TransformRequest::new(artifact, options));
        }
        #[cfg(not(feature = "svg"))]
        {
            let _ = (options, watermark);
            return Err(TransformError::CapabilityMissing(
                "SVG processing is not enabled in this build".to_string(),
            ));
        }
    }

    let mut request = TransformRequest::new(artifact, options);
    request.watermark = watermark;
    transform_raster(request)
}

fn artifact_info(artifact: &Artifact) -> WasmArtifactInfo {
    WasmArtifactInfo {
        media_type: artifact.media_type.as_name().to_string(),
        mime_type: artifact.media_type.as_mime().to_string(),
        width: artifact.metadata.width,
        height: artifact.metadata.height,
        frame_count: artifact.metadata.frame_count,
        has_alpha: artifact.metadata.has_alpha,
    }
}

fn output_extension(media_type: MediaType) -> &'static str {
    match media_type {
        MediaType::Jpeg => "jpg",
        MediaType::Png => "png",
        MediaType::Webp => "webp",
        MediaType::Avif => "avif",
        MediaType::Svg => "svg",
        MediaType::Bmp => "bmp",
    }
}

#[cfg(feature = "wasm")]
fn error_kind(error: &TransformError) -> &'static str {
    match error {
        TransformError::InvalidInput(_) => "invalidInput",
        TransformError::InvalidOptions(_) => "invalidOptions",
        TransformError::UnsupportedInputMediaType(_) => "unsupportedInputMediaType",
        TransformError::UnsupportedOutputMediaType(_) => "unsupportedOutputMediaType",
        TransformError::DecodeFailed(_) => "decodeFailed",
        TransformError::EncodeFailed(_) => "encodeFailed",
        TransformError::CapabilityMissing(_) => "capabilityMissing",
        TransformError::LimitExceeded(_) => "limitExceeded",
    }
}

#[cfg(feature = "wasm")]
fn serialize_json<T: Serialize>(value: &T) -> Result<String, JsValue> {
    serde_json::to_string(value)
        .map_err(|error| JsValue::from_str(&format!("failed to serialize WASM response: {error}")))
}

#[cfg(feature = "wasm")]
fn transform_error_to_js(error: TransformError) -> JsValue {
    let payload = WasmErrorPayload {
        kind: error_kind(&error),
        message: error.to_string(),
    };

    serialize_json(&payload)
        .map(JsValue::from)
        .unwrap_or_else(|_| JsValue::from_str(&format!("{}: {}", payload.kind, payload.message)))
}

/// Browser-facing watermark options accepted by the WASM adapter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmWatermarkOptions {
    /// Watermark placement position (e.g. `bottom-right`, `center`).
    pub position: Option<String>,
    /// Watermark opacity (1–100). Default: 50.
    pub opacity: Option<u8>,
    /// Margin in pixels from the nearest edge. Default: 10.
    pub margin: Option<u32>,
}

fn resolve_wasm_watermark(
    watermark_bytes: Vec<u8>,
    watermark_options: WasmWatermarkOptions,
) -> Result<WatermarkInput, TransformError> {
    let artifact = sniff_artifact(RawArtifact::new(watermark_bytes, None))?;
    if artifact.media_type == MediaType::Svg {
        return Err(TransformError::InvalidOptions(
            "watermark image must be a raster format, not SVG".to_string(),
        ));
    }
    let position = watermark_options
        .position
        .map(|v| {
            Position::from_str(&v).map_err(|reason| {
                TransformError::InvalidOptions(format!("watermark position is invalid: {reason}"))
            })
        })
        .transpose()?
        .unwrap_or(Position::BottomRight);
    let opacity = watermark_options.opacity.unwrap_or(50);
    if opacity == 0 || opacity > 100 {
        return Err(TransformError::InvalidOptions(
            "watermark opacity must be between 1 and 100".to_string(),
        ));
    }
    let margin = watermark_options.margin.unwrap_or(10);

    Ok(WatermarkInput {
        image: artifact,
        position,
        opacity,
        margin,
    })
}

/// Transforms browser-provided bytes with an optional watermark overlay.
pub fn transform_browser_artifact_with_watermark(
    input_bytes: Vec<u8>,
    declared_media_type: Option<&str>,
    options: WasmTransformOptions,
    watermark_bytes: Vec<u8>,
    watermark_options: WasmWatermarkOptions,
) -> Result<WasmTransformResponse, TransformError> {
    let artifact = sniff_browser_artifact(input_bytes, declared_media_type)?;
    let options = parse_wasm_options(options)?;
    let watermark = resolve_wasm_watermark(watermark_bytes, watermark_options)?;
    let output = dispatch_browser_transform_with_watermark(artifact, options, Some(watermark))?;
    let TransformResult { artifact, warnings } = output;
    let artifact_info = artifact_info(&artifact);
    let suggested_extension = output_extension(artifact.media_type).to_string();

    Ok(WasmTransformResponse {
        bytes: artifact.bytes,
        artifact: artifact_info,
        warnings: warnings
            .into_iter()
            .map(|warning| warning.to_string())
            .collect(),
        suggested_extension,
    })
}

/// Browser-facing transform output returned by [`transform_image`].
///
/// JavaScript callers receive the transformed bytes separately from the JSON metadata so the
/// output can be downloaded or previewed without reparsing large byte arrays through JSON.
#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub struct WasmTransformOutput {
    bytes: Vec<u8>,
    response_json: String,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl WasmTransformOutput {
    /// Returns the transformed output bytes.
    #[wasm_bindgen(getter)]
    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.clone()
    }

    /// Returns JSON metadata describing the transformed output.
    #[wasm_bindgen(js_name = responseJson, getter)]
    pub fn response_json(&self) -> String {
        self.response_json.clone()
    }
}

/// Returns build-time capabilities to JavaScript callers as a JSON string.
#[cfg(feature = "wasm")]
#[wasm_bindgen(js_name = getCapabilitiesJson)]
pub fn get_capabilities_json() -> Result<String, JsValue> {
    serialize_json(&browser_capabilities())
}

/// Inspects image bytes supplied by JavaScript and returns structured metadata as JSON.
///
/// The returned object contains the canonical media type, MIME type, dimensions, frame count,
/// and alpha information when available.
#[cfg(feature = "wasm")]
#[wasm_bindgen(js_name = inspectImageJson)]
pub fn inspect_image_json(
    input_bytes: &[u8],
    declared_media_type: Option<String>,
) -> Result<String, JsValue> {
    let response = inspect_browser_artifact(input_bytes.to_vec(), declared_media_type.as_deref())
        .map_err(transform_error_to_js)?;

    serialize_json(&response)
}

/// Transforms image bytes supplied by JavaScript and returns output bytes plus metadata.
///
/// `options_json` must match the JSON shape of [`WasmTransformOptions`]. On success, the
/// returned object contains output bytes plus a JSON metadata payload describing the artifact,
/// warnings, and suggested file extension for download flows.
#[cfg(feature = "wasm")]
#[wasm_bindgen(js_name = transformImage)]
pub fn transform_image(
    input_bytes: &[u8],
    declared_media_type: Option<String>,
    options_json: &str,
) -> Result<WasmTransformOutput, JsValue> {
    let options = serde_json::from_str::<WasmTransformOptions>(options_json).map_err(|error| {
        transform_error_to_js(TransformError::InvalidOptions(format!(
            "failed to parse transform options: {error}"
        )))
    })?;
    let response = transform_browser_artifact(
        input_bytes.to_vec(),
        declared_media_type.as_deref(),
        options,
    )
    .map_err(transform_error_to_js)?;
    let response_json = serialize_json(&response)?;

    Ok(WasmTransformOutput {
        bytes: response.bytes,
        response_json,
    })
}

/// Transforms image bytes with a watermark overlay supplied by JavaScript.
///
/// `watermark_bytes` must contain valid raster image bytes (not SVG).
/// `watermark_options_json` must match the JSON shape of [`WasmWatermarkOptions`].
#[cfg(feature = "wasm")]
#[wasm_bindgen(js_name = transformImageWithWatermark)]
pub fn transform_image_with_watermark(
    input_bytes: &[u8],
    declared_media_type: Option<String>,
    options_json: &str,
    watermark_bytes: &[u8],
    watermark_options_json: &str,
) -> Result<WasmTransformOutput, JsValue> {
    let options = serde_json::from_str::<WasmTransformOptions>(options_json).map_err(|error| {
        transform_error_to_js(TransformError::InvalidOptions(format!(
            "failed to parse transform options: {error}"
        )))
    })?;
    let watermark_options = serde_json::from_str::<WasmWatermarkOptions>(watermark_options_json)
        .map_err(|error| {
            transform_error_to_js(TransformError::InvalidOptions(format!(
                "failed to parse watermark options: {error}"
            )))
        })?;
    let response = transform_browser_artifact_with_watermark(
        input_bytes.to_vec(),
        declared_media_type.as_deref(),
        options,
        watermark_bytes.to_vec(),
        watermark_options,
    )
    .map_err(transform_error_to_js)?;
    let response_json = serialize_json(&response)?;

    Ok(WasmTransformOutput {
        bytes: response.bytes,
        response_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, width, height, ColorType::Rgba8.into())
            .expect("encode png");
        bytes
    }

    #[test]
    fn browser_capabilities_reflect_compile_time_features() {
        let capabilities = browser_capabilities();

        assert_eq!(capabilities.svg, cfg!(feature = "svg"));
        assert_eq!(capabilities.webp_lossy, cfg!(feature = "webp-lossy"));
    }

    #[test]
    fn inspect_browser_artifact_reports_png_metadata() {
        let response =
            inspect_browser_artifact(png_bytes(4, 3), Some("png")).expect("inspect png artifact");

        assert_eq!(response.artifact.media_type, "png");
        assert_eq!(response.artifact.mime_type, "image/png");
        assert_eq!(response.artifact.width, Some(4));
        assert_eq!(response.artifact.height, Some(3));
        assert_eq!(response.artifact.has_alpha, Some(true));
    }

    #[test]
    fn transform_browser_artifact_converts_png_to_jpeg() {
        let response = transform_browser_artifact(
            png_bytes(4, 3),
            Some("png"),
            WasmTransformOptions {
                format: Some("jpeg".to_string()),
                width: Some(2),
                ..WasmTransformOptions::default()
            },
        )
        .expect("transform png to jpeg");

        assert_eq!(response.artifact.media_type, "jpeg");
        assert_eq!(response.artifact.mime_type, "image/jpeg");
        assert_eq!(response.artifact.width, Some(2));
        assert_eq!(response.artifact.height, Some(2));
        assert_eq!(response.suggested_extension, "jpg");
        assert!(response.bytes.starts_with(&[0xFF, 0xD8]));
    }

    #[test]
    fn parse_wasm_options_rejects_conflicting_metadata_flags() {
        let error = parse_wasm_options(WasmTransformOptions {
            keep_metadata: Some(true),
            preserve_exif: Some(true),
            ..WasmTransformOptions::default()
        })
        .expect_err("conflicting metadata flags should fail");

        assert_eq!(
            error,
            TransformError::InvalidOptions(
                "keepMetadata and preserveExif cannot both be true".to_string()
            )
        );
    }

    #[test]
    fn raster_input_cannot_request_svg_output() {
        let error = transform_browser_artifact(
            png_bytes(4, 3),
            Some("png"),
            WasmTransformOptions {
                format: Some("svg".to_string()),
                ..WasmTransformOptions::default()
            },
        )
        .expect_err("raster input should not produce svg output");

        assert_eq!(
            error,
            TransformError::UnsupportedOutputMediaType(MediaType::Svg)
        );
    }

    #[test]
    fn test_resolve_wasm_watermark_rejects_svg() {
        // Minimal valid SVG
        let svg_bytes = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>".to_vec();
        let error = resolve_wasm_watermark(svg_bytes, WasmWatermarkOptions::default())
            .expect_err("SVG watermark should be rejected");

        assert_eq!(
            error,
            TransformError::InvalidOptions(
                "watermark image must be a raster format, not SVG".to_string()
            )
        );
    }

    #[test]
    fn test_resolve_wasm_watermark_rejects_opacity_zero() {
        let error = resolve_wasm_watermark(
            png_bytes(2, 2),
            WasmWatermarkOptions {
                opacity: Some(0),
                ..WasmWatermarkOptions::default()
            },
        )
        .expect_err("opacity 0 should be rejected");

        assert_eq!(
            error,
            TransformError::InvalidOptions(
                "watermark opacity must be between 1 and 100".to_string()
            )
        );
    }

    #[test]
    fn test_resolve_wasm_watermark_rejects_opacity_over_100() {
        let error = resolve_wasm_watermark(
            png_bytes(2, 2),
            WasmWatermarkOptions {
                opacity: Some(101),
                ..WasmWatermarkOptions::default()
            },
        )
        .expect_err("opacity 101 should be rejected");

        assert_eq!(
            error,
            TransformError::InvalidOptions(
                "watermark opacity must be between 1 and 100".to_string()
            )
        );
    }

    #[test]
    fn test_resolve_wasm_watermark_defaults() {
        let wm = resolve_wasm_watermark(png_bytes(2, 2), WasmWatermarkOptions::default())
            .expect("valid watermark with defaults");

        assert_eq!(wm.position, Position::BottomRight);
        assert_eq!(wm.opacity, 50);
        assert_eq!(wm.margin, 10);
    }

    #[test]
    fn test_transform_with_watermark_basic() {
        let response = transform_browser_artifact_with_watermark(
            png_bytes(16, 16),
            None,
            WasmTransformOptions::default(),
            png_bytes(4, 4),
            WasmWatermarkOptions {
                position: Some("center".to_string()),
                opacity: Some(80),
                margin: Some(0),
            },
        )
        .expect("transform with watermark should succeed");

        assert_eq!(response.artifact.media_type, "png");
        assert_eq!(response.artifact.width, Some(16));
        assert_eq!(response.artifact.height, Some(16));
        assert!(!response.bytes.is_empty());
        // Verify the output is a valid PNG (magic bytes)
        assert!(response.bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    }
}
