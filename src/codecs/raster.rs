use crate::Rgba8;
use crate::core::{
    Artifact, ArtifactMetadata, CropRegion, Fit, MAX_DECODED_PIXELS, MAX_OUTPUT_PIXELS, MediaType,
    MetadataKind, MetadataPolicy, Position, Rotation, TransformError, TransformRequest,
    TransformResult, TransformWarning, WatermarkInput,
};
use exif::{In, Reader, Tag, Value};
use image::codecs::avif::AvifEncoder;
use image::codecs::jpeg::JpegDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngDecoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPDecoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::{self, FilterType};
use image::metadata::Orientation;
use image::{
    ColorType, DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat, Rgba,
    RgbaImage,
};
use mp4parse::ParseStrictness;
use rav1d_safe::{Decoder, Planes};
use std::io::Cursor;
use std::time::{Duration, Instant};
use yuvutils_rs::{YuvGrayImage, YuvPlanarImage, YuvRange, YuvStandardMatrix};

/// Checks the transform deadline and returns an error if exceeded.
///
/// This macro reduces boilerplate for the repeated deadline-check pattern
/// throughout the transform pipeline.
macro_rules! check_deadline_if_set {
    ($start:expr, $deadline:expr, $stage:expr) => {
        if let (Some(start), Some(limit)) = ($start, $deadline) {
            check_deadline(start.elapsed(), limit, $stage)?;
        }
    };
}

/// Transforms a raster artifact using the current backend implementation.
///
/// The input artifact must already be classified by [`crate::sniff_artifact`]. This backend
/// performs raster-only work for the current implementation phase: optional EXIF auto-orient
/// for JPEG input, explicit rotation, resize handling, and encoding into the requested output
/// format. Metadata stripping remains the default, while `preserve_exif` retains EXIF and
/// `keep-metadata` retains EXIF plus ICC profiles for JPEG, PNG, and WebP output. Metadata types
/// that the current encoders cannot round-trip, such as XMP or IPTC, are silently dropped and
/// reported as [`TransformWarning::MetadataDropped`] warnings in the returned
/// [`TransformResult`].
///
/// # Errors
///
/// Returns [`TransformError::InvalidOptions`] when the request fails Core validation,
/// [`TransformError::DecodeFailed`] or [`TransformError::EncodeFailed`] when image processing
/// fails, and [`TransformError::CapabilityMissing`] for features that are intentionally not
/// implemented yet, such as metadata retention on AVIF output.
///
/// # Examples
///
/// ```
/// use image::codecs::png::PngEncoder;
/// use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
/// use truss::{sniff_artifact, transform_raster, MediaType, RawArtifact, TransformOptions, TransformRequest};
///
/// let image = RgbaImage::from_pixel(2, 2, Rgba([10, 20, 30, 255]));
/// let mut bytes = Vec::new();
/// PngEncoder::new(&mut bytes)
///     .write_image(&image, 2, 2, ColorType::Rgba8.into())
///     .unwrap();
///
/// let input = sniff_artifact(RawArtifact::new(bytes, Some(MediaType::Png))).unwrap();
/// let output = transform_raster(TransformRequest::new(
///     input,
///     TransformOptions {
///         format: Some(MediaType::Jpeg),
///         ..TransformOptions::default()
///     },
/// ))
/// .unwrap();
///
/// assert_eq!(output.artifact.media_type, MediaType::Jpeg);
/// assert_eq!(output.artifact.metadata.width, Some(2));
/// assert_eq!(output.artifact.metadata.height, Some(2));
/// ```
///
/// ```
/// use image::codecs::png::PngEncoder;
/// use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
/// use truss::{sniff_artifact, transform_raster, MediaType, RawArtifact, TransformOptions, TransformRequest};
///
/// let image = RgbaImage::from_pixel(2, 2, Rgba([10, 20, 30, 255]));
/// let mut bytes = Vec::new();
/// PngEncoder::new(&mut bytes)
///     .write_image(&image, 2, 2, ColorType::Rgba8.into())
///     .unwrap();
///
/// let input = sniff_artifact(RawArtifact::new(bytes, Some(MediaType::Png))).unwrap();
/// let output = transform_raster(TransformRequest::new(
///     input,
///     TransformOptions {
///         format: Some(MediaType::Avif),
///         quality: Some(70),
///         ..TransformOptions::default()
///     },
/// ))
/// .unwrap();
/// let sniffed = sniff_artifact(RawArtifact::new(output.artifact.bytes.clone(), None)).unwrap();
///
/// assert_eq!(output.artifact.media_type, MediaType::Avif);
/// assert_eq!(sniffed.media_type, MediaType::Avif);
/// ```
///
/// ```
/// use image::codecs::jpeg::JpegDecoder;
/// use image::codecs::jpeg::JpegEncoder;
/// use image::metadata::Orientation;
/// use image::{ColorType, ImageDecoder, ImageEncoder, Rgb, RgbImage};
/// use std::io::Cursor;
/// use truss::{sniff_artifact, transform_raster, MediaType, RawArtifact, TransformOptions, TransformRequest};
///
/// let image = RgbImage::from_pixel(2, 1, Rgb([10, 20, 30]));
/// let exif = vec![
///     0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00,
///     0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01, 0x00,
///     0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00,
///     0x00, 0x00,
/// ];
/// let mut bytes = Vec::new();
/// let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 80);
/// encoder.set_exif_metadata(exif).unwrap();
/// encoder
///     .write_image(&image, 2, 1, ColorType::Rgb8.into())
///     .unwrap();
///
/// let input = sniff_artifact(RawArtifact::new(bytes, Some(MediaType::Jpeg))).unwrap();
/// let output = transform_raster(TransformRequest::new(
///     input,
///     TransformOptions {
///         format: Some(MediaType::Jpeg),
///         strip_metadata: false,
///         preserve_exif: true,
///         ..TransformOptions::default()
///     },
/// ))
/// .unwrap();
///
/// let mut decoder = JpegDecoder::new(Cursor::new(&output.artifact.bytes)).unwrap();
/// let exif = decoder.exif_metadata().unwrap().unwrap();
///
/// assert_eq!(output.artifact.metadata.width, Some(1));
/// assert_eq!(output.artifact.metadata.height, Some(2));
/// assert_eq!(Orientation::from_exif_chunk(&exif), Some(Orientation::NoTransforms));
/// ```
///
/// ```
/// use image::codecs::jpeg::JpegDecoder;
/// use image::codecs::jpeg::JpegEncoder;
/// use image::{ColorType, ImageDecoder, ImageEncoder, Rgb, RgbImage};
/// use std::io::Cursor;
/// use truss::{sniff_artifact, transform_raster, MediaType, RawArtifact, TransformOptions, TransformRequest};
///
/// let image = RgbImage::from_pixel(2, 1, Rgb([10, 20, 30]));
/// let mut bytes = Vec::new();
/// let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 80);
/// encoder.set_icc_profile(b"demo-icc-profile".to_vec()).unwrap();
/// encoder
///     .write_image(&image, 2, 1, ColorType::Rgb8.into())
///     .unwrap();
///
/// let input = sniff_artifact(RawArtifact::new(bytes, Some(MediaType::Jpeg))).unwrap();
/// let output = transform_raster(TransformRequest::new(
///     input,
///     TransformOptions {
///         format: Some(MediaType::Jpeg),
///         strip_metadata: false,
///         ..TransformOptions::default()
///     },
/// ))
/// .unwrap();
///
/// let mut decoder = JpegDecoder::new(Cursor::new(&output.artifact.bytes)).unwrap();
/// assert_eq!(decoder.icc_profile().unwrap(), Some(b"demo-icc-profile".to_vec()));
/// ```
pub fn transform_raster(request: TransformRequest) -> Result<TransformResult, TransformError> {
    let normalized = request.normalize()?;
    let deadline = normalized.options.deadline;
    let start = deadline.map(|_| Instant::now());

    let (retained_metadata, mut warnings) = extract_retained_metadata(
        &normalized.input,
        normalized.options.metadata_policy,
        normalized.options.auto_orient,
        normalized.options.format,
    )?;

    check_input_pixel_limit(&normalized.input)?;

    let mut image = decode_input(&normalized.input)?;
    check_deadline_if_set!(start, deadline, "decode");

    if normalized.options.auto_orient {
        image = apply_auto_orientation(image, &normalized.input);
    }

    image = apply_rotation(image, normalized.options.rotate);
    check_deadline_if_set!(start, deadline, "rotate");

    if let Some(crop) = normalized.options.crop {
        image = apply_crop(image, crop)?;
        check_deadline_if_set!(start, deadline, "crop");
    }

    check_output_pixel_limit(&image, normalized.options.width, normalized.options.height)?;
    image = apply_resize(
        image,
        normalized.options.width,
        normalized.options.height,
        normalized.options.fit,
        normalized.options.position,
        normalized.options.background,
        normalized.options.format,
    );
    check_deadline_if_set!(start, deadline, "resize");

    if let Some(sigma) = normalized.options.blur {
        image = image.blur(sigma);
        check_deadline_if_set!(start, deadline, "blur");
    }

    if let Some(sigma) = normalized.options.sharpen {
        image = image.unsharpen(sigma, 1);
        check_deadline_if_set!(start, deadline, "sharpen");
    }

    if let Some(ref wm) = normalized.watermark {
        image = apply_watermark(image, wm)?;
        check_deadline_if_set!(start, deadline, "watermark");
    }

    let bytes = encode_output(
        &image,
        normalized.options.format,
        normalized.options.quality,
        retained_metadata.as_ref(),
    )?;
    check_deadline_if_set!(start, deadline, "encode");

    // Lossy WebP uses libwebp which cannot inject EXIF/ICC, so metadata is silently
    // dropped even when keep-metadata was requested. Warn the caller.
    if normalized.options.format == MediaType::Webp
        && normalized.options.quality.is_some()
        && retained_metadata.as_ref().is_some_and(|m| !m.is_empty())
    {
        warnings.push(TransformWarning::MetadataDropped(MetadataKind::Exif));
        warnings.push(TransformWarning::MetadataDropped(MetadataKind::Icc));
    }

    // Post-encode byte-level injection for XMP and IPTC metadata.
    let bytes = if let Some(ref metadata) = retained_metadata {
        inject_metadata(bytes, normalized.options.format, metadata, &mut warnings)
    } else {
        bytes
    };

    let (width, height) = image.dimensions();

    Ok(TransformResult {
        artifact: Artifact::new(
            bytes,
            normalized.options.format,
            ArtifactMetadata {
                width: Some(width),
                height: Some(height),
                frame_count: 1,
                duration: None,
                has_alpha: Some(output_has_alpha(&image, normalized.options.format)),
            },
        ),
        warnings,
    })
}

fn decode_input(input: &Artifact) -> Result<DynamicImage, TransformError> {
    let image_format = match input.media_type {
        MediaType::Jpeg => ImageFormat::Jpeg,
        MediaType::Png => ImageFormat::Png,
        MediaType::Webp => ImageFormat::WebP,
        MediaType::Avif => return decode_avif(&input.bytes),
        MediaType::Bmp => ImageFormat::Bmp,
        MediaType::Tiff => ImageFormat::Tiff,
        MediaType::Svg => {
            return Err(TransformError::UnsupportedInputMediaType(
                "SVG input should be routed to transform_svg, not transform_raster".into(),
            ));
        }
    };

    image::load_from_memory_with_format(&input.bytes, image_format)
        .map_err(|error| TransformError::DecodeFailed(error.to_string()))
}

/// Decodes an AVIF image using `rav1d` (pure Rust AV1 decoder) and `mp4parse` (ISOBMFF parser).
///
/// The pipeline extracts AV1 OBU data from the AVIF container, decodes it into YUV planes,
/// and converts to RGBA using the color matrix and range signaled in the bitstream.
/// Alpha planes are decoded separately when present in the container.
///
/// Supports 8-bit YUV 4:2:0, 4:2:2, 4:4:4, and 4:0:0 (grayscale) layouts.
/// 10/12-bit images are downscaled to 8-bit with rounding.
fn decode_avif(bytes: &[u8]) -> Result<DynamicImage, TransformError> {
    let mut cursor = Cursor::new(bytes);
    let context = mp4parse::read_avif(&mut cursor, ParseStrictness::Normal)
        .map_err(|e| TransformError::DecodeFailed(format!("AVIF container parse failed: {e}")))?;

    let primary_data = context
        .primary_item_coded_data()
        .ok_or_else(|| TransformError::DecodeFailed("AVIF has no primary item data".into()))?;

    let frame = decode_av1_frame(primary_data)?;
    let width = frame.width();
    let height = frame.height();

    let color = frame.color_info();
    let matrix = map_yuv_matrix(color.matrix_coefficients);
    let range = map_yuv_range(color.color_range);

    let mut rgba = yuv_frame_to_rgba(&frame, width, height, range, matrix)?;

    // Decode alpha plane if present and merge into RGBA.
    if let Some(alpha_data) = context.alpha_item_coded_data() {
        let alpha_frame = decode_av1_frame(alpha_data)
            .map_err(|e| TransformError::DecodeFailed(format!("AVIF alpha decode failed: {e}")))?;
        merge_alpha_plane(&alpha_frame, &mut rgba, width, height);
    }

    RgbaImage::from_raw(width, height, rgba)
        .map(DynamicImage::ImageRgba8)
        .ok_or_else(|| TransformError::DecodeFailed("AVIF decoded buffer size mismatch".into()))
}

/// Feeds AV1 OBU data to a `rav1d` decoder and returns the first decoded frame.
fn decode_av1_frame(obu_data: &[u8]) -> Result<rav1d_safe::Frame, TransformError> {
    let mut decoder = Decoder::new()
        .map_err(|e| TransformError::DecodeFailed(format!("AV1 decoder init failed: {e}")))?;

    if let Some(frame) = decoder
        .decode(obu_data)
        .map_err(|e| TransformError::DecodeFailed(format!("AV1 decode failed: {e}")))?
    {
        return Ok(frame);
    }

    // Flush any buffered frames.
    let frames = decoder
        .flush()
        .map_err(|e| TransformError::DecodeFailed(format!("AV1 flush failed: {e}")))?;

    frames
        .into_iter()
        .next()
        .ok_or_else(|| TransformError::DecodeFailed("AV1 decoder produced no frames".into()))
}

/// Maps rav1d `MatrixCoefficients` to the corresponding `yuvutils_rs` standard matrix.
fn map_yuv_matrix(mc: rav1d_safe::MatrixCoefficients) -> YuvStandardMatrix {
    match mc {
        rav1d_safe::MatrixCoefficients::BT601 => YuvStandardMatrix::Bt601,
        rav1d_safe::MatrixCoefficients::BT470BG => YuvStandardMatrix::Bt601,
        rav1d_safe::MatrixCoefficients::BT2020NCL => YuvStandardMatrix::Bt2020,
        rav1d_safe::MatrixCoefficients::BT2020CL => YuvStandardMatrix::Bt2020,
        rav1d_safe::MatrixCoefficients::SMPTE240 => YuvStandardMatrix::Smpte240,
        // BT.709 is the most common for AVIF and a safe default for unspecified.
        _ => YuvStandardMatrix::Bt709,
    }
}

/// Maps rav1d `ColorRange` to `yuvutils_rs` range.
fn map_yuv_range(cr: rav1d_safe::ColorRange) -> YuvRange {
    match cr {
        rav1d_safe::ColorRange::Full => YuvRange::Full,
        rav1d_safe::ColorRange::Limited => YuvRange::Limited,
    }
}

/// Converts a decoded AV1 frame's YUV planes to RGBA bytes.
///
/// Handles 8-bit and 10/12-bit depth by downscaling higher bit depths to 8-bit.
/// Supports I420, I422, I444, and I400 (grayscale) pixel layouts.
fn yuv_frame_to_rgba(
    frame: &rav1d_safe::Frame,
    width: u32,
    height: u32,
    range: YuvRange,
    matrix: YuvStandardMatrix,
) -> Result<Vec<u8>, TransformError> {
    let rgba_stride = width.checked_mul(4).ok_or_else(|| {
        TransformError::DecodeFailed("AVIF frame dimensions overflow address space".into())
    })?;
    let total_bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| {
            TransformError::DecodeFailed("AVIF frame dimensions overflow address space".into())
        })?;
    let mut rgba = vec![255u8; total_bytes];
    let layout = frame.pixel_layout();

    match frame.planes() {
        Planes::Depth8(planes) => {
            let y = planes.y();
            convert_8bit_yuv_to_rgba(
                layout,
                y.as_slice(),
                y.stride(),
                planes.u().as_ref().map(|p| (p.as_slice(), p.stride())),
                planes.v().as_ref().map(|p| (p.as_slice(), p.stride())),
                width,
                height,
                &mut rgba,
                rgba_stride,
                range,
                matrix,
            )?;
        }
        Planes::Depth16(planes) => {
            let bit_depth = frame.bit_depth();
            let shift = bit_depth - 8;
            let round = 1u16 << (shift - 1);
            let y8: Vec<u8> = planes
                .y()
                .as_slice()
                .iter()
                .map(|&v| ((v.saturating_add(round)) >> shift) as u8)
                .collect();
            let y_stride = planes.y().stride();
            let u8s: Option<(Vec<u8>, usize)> = planes.u().as_ref().map(|p| {
                let data: Vec<u8> = p
                    .as_slice()
                    .iter()
                    .map(|&v| ((v.saturating_add(round)) >> shift) as u8)
                    .collect();
                (data, p.stride())
            });
            let v8s: Option<(Vec<u8>, usize)> = planes.v().as_ref().map(|p| {
                let data: Vec<u8> = p
                    .as_slice()
                    .iter()
                    .map(|&v| ((v.saturating_add(round)) >> shift) as u8)
                    .collect();
                (data, p.stride())
            });
            convert_8bit_yuv_to_rgba(
                layout,
                &y8,
                y_stride,
                u8s.as_ref().map(|(d, s)| (d.as_slice(), *s)),
                v8s.as_ref().map(|(d, s)| (d.as_slice(), *s)),
                width,
                height,
                &mut rgba,
                rgba_stride,
                range,
                matrix,
            )?;
        }
    }

    Ok(rgba)
}

/// Converts 8-bit YUV plane data to RGBA, dispatching by pixel layout.
///
/// U and V planes are `None` for I400 (grayscale). For I420/I422/I444, both must be present.
#[allow(clippy::too_many_arguments)]
fn convert_8bit_yuv_to_rgba(
    layout: rav1d_safe::PixelLayout,
    y_data: &[u8],
    y_stride: usize,
    u_data: Option<(&[u8], usize)>,
    v_data: Option<(&[u8], usize)>,
    width: u32,
    height: u32,
    rgba: &mut [u8],
    rgba_stride: u32,
    range: YuvRange,
    matrix: YuvStandardMatrix,
) -> Result<(), TransformError> {
    match layout {
        rav1d_safe::PixelLayout::I400 => {
            let gray = YuvGrayImage {
                y_plane: y_data,
                y_stride: y_stride as u32,
                width,
                height,
            };
            yuvutils_rs::yuv400_to_rgba(&gray, rgba, rgba_stride, range, matrix)
                .map_err(|e| TransformError::DecodeFailed(format!("YUV400→RGBA failed: {e}")))?;
        }
        _ => {
            let (u_plane, u_stride) = u_data.ok_or_else(|| {
                TransformError::DecodeFailed("missing U plane for non-grayscale AVIF".into())
            })?;
            let (v_plane, v_stride) = v_data.ok_or_else(|| {
                TransformError::DecodeFailed("missing V plane for non-grayscale AVIF".into())
            })?;
            let planar = YuvPlanarImage {
                y_plane: y_data,
                y_stride: y_stride as u32,
                u_plane,
                u_stride: u_stride as u32,
                v_plane,
                v_stride: v_stride as u32,
                width,
                height,
            };
            let convert_fn = match layout {
                rav1d_safe::PixelLayout::I420 => yuvutils_rs::yuv420_to_rgba,
                rav1d_safe::PixelLayout::I422 => yuvutils_rs::yuv422_to_rgba,
                rav1d_safe::PixelLayout::I444 => yuvutils_rs::yuv444_to_rgba,
                rav1d_safe::PixelLayout::I400 => unreachable!(),
            };
            convert_fn(&planar, rgba, rgba_stride, range, matrix)
                .map_err(|e| TransformError::DecodeFailed(format!("YUV→RGBA failed: {e}")))?;
        }
    }
    Ok(())
}

/// Merges a separately decoded alpha plane into an existing RGBA buffer.
///
/// The alpha frame's Y plane is used as the alpha channel. If the alpha frame dimensions
/// do not match the primary frame, the merge is silently skipped.
fn merge_alpha_plane(alpha_frame: &rav1d_safe::Frame, rgba: &mut [u8], width: u32, height: u32) {
    if alpha_frame.width() != width || alpha_frame.height() != height {
        return;
    }

    let w = width as usize;
    let row_stride = w.saturating_mul(4);

    match alpha_frame.planes() {
        Planes::Depth8(planes) => {
            let y = planes.y();
            for row_idx in 0..height as usize {
                let row = y.row(row_idx);
                let row_start = row_idx.saturating_mul(row_stride);
                for (col, &alpha) in row.iter().enumerate().take(w) {
                    let idx = row_start + col * 4 + 3;
                    if idx < rgba.len() {
                        rgba[idx] = alpha;
                    }
                }
            }
        }
        Planes::Depth16(planes) => {
            let shift = alpha_frame.bit_depth() - 8;
            let y = planes.y();
            for row_idx in 0..height as usize {
                let row = y.row(row_idx);
                let row_start = row_idx.saturating_mul(row_stride);
                for (col, &alpha) in row.iter().enumerate().take(w) {
                    let idx = row_start + col * 4 + 3;
                    if idx < rgba.len() {
                        rgba[idx] = (alpha >> shift) as u8;
                    }
                }
            }
        }
    }
}

/// Checks whether the elapsed time exceeds the given deadline.
///
/// Called at pipeline stage boundaries when a deadline is configured. Accepts the elapsed
/// time and limit as separate values so the function can be tested without depending on
/// real wall-clock time.
pub(crate) fn check_deadline(
    elapsed: Duration,
    limit: Duration,
    stage: &str,
) -> Result<(), TransformError> {
    if elapsed > limit {
        return Err(TransformError::LimitExceeded(format!(
            "transform exceeded {:.0}s deadline after {stage} (elapsed: {:.1}s)",
            limit.as_secs_f64(),
            elapsed.as_secs_f64()
        )));
    }
    Ok(())
}

/// Checks the input artifact dimensions against [`MAX_DECODED_PIXELS`] before decoding.
///
/// This uses the dimensions extracted by [`crate::sniff_artifact`] during media-type detection,
/// so the check runs without allocating the full decoded pixel buffer. If the artifact metadata
/// does not contain dimensions (e.g. a truncated header), the check is skipped and the decoder
/// will handle the error downstream.
fn check_input_pixel_limit(input: &Artifact) -> Result<(), TransformError> {
    if let (Some(w), Some(h)) = (input.metadata.width, input.metadata.height) {
        let pixels = u64::from(w) * u64::from(h);
        if pixels > MAX_DECODED_PIXELS {
            return Err(TransformError::LimitExceeded(format!(
                "decoded image has {pixels} pixels, limit is {MAX_DECODED_PIXELS}"
            )));
        }
    }
    Ok(())
}

/// Checks the output dimensions against [`MAX_OUTPUT_PIXELS`] before resize allocation.
///
/// Computes the effective output pixel count from the requested dimensions and the current
/// image size. The check runs before `apply_resize` so that oversized output buffers are
/// never allocated.
fn check_output_pixel_limit(
    image: &DynamicImage,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<(), TransformError> {
    let (current_w, current_h) = image.dimensions();
    let out_w = width.unwrap_or(current_w);
    let out_h = height.unwrap_or(current_h);
    let pixels = u64::from(out_w) * u64::from(out_h);
    if pixels > MAX_OUTPUT_PIXELS {
        return Err(TransformError::LimitExceeded(format!(
            "output image would have {pixels} pixels, limit is {MAX_OUTPUT_PIXELS}"
        )));
    }
    Ok(())
}

fn apply_auto_orientation(image: DynamicImage, input: &Artifact) -> DynamicImage {
    if input.media_type != MediaType::Jpeg {
        return image;
    }

    let mut cursor = Cursor::new(&input.bytes);
    let Ok(exif) = Reader::new().read_from_container(&mut cursor) else {
        return image;
    };
    let Some(field) = exif.get_field(Tag::Orientation, In::PRIMARY) else {
        return image;
    };
    let Some(orientation) = first_orientation_value(&field.value) else {
        return image;
    };

    apply_exif_orientation(image, orientation)
}

fn first_orientation_value(value: &Value) -> Option<u32> {
    match value {
        Value::Short(values) => values.first().map(|value| u32::from(*value)),
        Value::Long(values) => values.first().copied(),
        _ => None,
    }
}

fn apply_exif_orientation(image: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.fliph().rotate90(),
        6 => image.rotate90(),
        7 => image.fliph().rotate270(),
        8 => image.rotate270(),
        _ => image,
    }
}

fn apply_rotation(image: DynamicImage, rotation: Rotation) -> DynamicImage {
    match rotation {
        Rotation::Deg0 => image,
        Rotation::Deg90 => image.rotate90(),
        Rotation::Deg180 => image.rotate180(),
        Rotation::Deg270 => image.rotate270(),
    }
}

fn apply_crop(image: DynamicImage, crop: CropRegion) -> Result<DynamicImage, TransformError> {
    let (iw, ih) = image.dimensions();
    if crop.x.saturating_add(crop.width) > iw || crop.y.saturating_add(crop.height) > ih {
        return Err(TransformError::InvalidOptions(format!(
            "crop region {}x{}+{}+{} exceeds image bounds {}x{}",
            crop.width, crop.height, crop.x, crop.y, iw, ih
        )));
    }
    Ok(image.crop_imm(crop.x, crop.y, crop.width, crop.height))
}

fn apply_resize(
    image: DynamicImage,
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    position: Position,
    background: Option<Rgba8>,
    output_format: MediaType,
) -> DynamicImage {
    let (original_width, original_height) = image.dimensions();

    match (width, height) {
        (None, None) => image,
        (Some(target_width), None) => {
            let target_height = scale_dimension(original_height, target_width, original_width);
            image.resize_exact(target_width, target_height, FilterType::Lanczos3)
        }
        (None, Some(target_height)) => {
            let target_width = scale_dimension(original_width, target_height, original_height);
            image.resize_exact(target_width, target_height, FilterType::Lanczos3)
        }
        (Some(target_width), Some(target_height)) => match fit.unwrap_or(Fit::Contain) {
            Fit::Fill => image.resize_exact(target_width, target_height, FilterType::Lanczos3),
            Fit::Contain => {
                let resized = image.resize(target_width, target_height, FilterType::Lanczos3);
                pad_to_box(
                    resized,
                    target_width,
                    target_height,
                    position,
                    background,
                    output_format,
                )
            }
            Fit::Inside => {
                let resized = if original_width <= target_width && original_height <= target_height
                {
                    image
                } else {
                    image.resize(target_width, target_height, FilterType::Lanczos3)
                };

                pad_to_box(
                    resized,
                    target_width,
                    target_height,
                    position,
                    background,
                    output_format,
                )
            }
            Fit::Cover => cover_to_box(
                image,
                target_width,
                target_height,
                position,
                background,
                output_format,
            ),
        },
    }
}

/// Composites a watermark image onto the main image at the given position,
/// opacity, and margin.
fn apply_watermark(
    image: DynamicImage,
    watermark: &WatermarkInput,
) -> Result<DynamicImage, TransformError> {
    // Early rejection using metadata dimensions (before allocating/decoding).
    let (main_w, main_h) = image.dimensions();
    if let (Some(meta_w), Some(meta_h)) = (
        watermark.image.metadata.width,
        watermark.image.metadata.height,
    ) {
        let margin = watermark.margin;
        let (mx, my) = match watermark.position {
            Position::Center => (0, 0),
            Position::Top | Position::Bottom => (0, margin),
            Position::Left | Position::Right => (margin, 0),
            _ => (margin, margin),
        };
        if u64::from(meta_w) + u64::from(mx) > u64::from(main_w)
            || u64::from(meta_h) + u64::from(my) > u64::from(main_h)
        {
            return Err(TransformError::InvalidOptions(
                "watermark image is too large for the output dimensions".to_string(),
            ));
        }
    }

    if let (Some(w), Some(h)) = (
        watermark.image.metadata.width,
        watermark.image.metadata.height,
    ) {
        let pixels = u64::from(w) * u64::from(h);
        if pixels > crate::MAX_WATERMARK_PIXELS {
            return Err(TransformError::LimitExceeded(format!(
                "watermark image has {pixels} pixels, limit is {}",
                crate::MAX_WATERMARK_PIXELS
            )));
        }
    }
    check_input_pixel_limit(&watermark.image)?;
    let wm_image = decode_input(&watermark.image)?;

    // Cross-check decoded dimensions against header-declared size to detect
    // malformed files that claim small dimensions but decode larger (#104).
    let (decoded_w, decoded_h) = wm_image.dimensions();
    if let (Some(meta_w), Some(meta_h)) = (
        watermark.image.metadata.width,
        watermark.image.metadata.height,
    ) && (decoded_w != meta_w || decoded_h != meta_h)
    {
        return Err(TransformError::InvalidInput(format!(
            "watermark decoded dimensions ({decoded_w}x{decoded_h}) \
             do not match header-declared size ({meta_w}x{meta_h})"
        )));
    }

    let mut wm_rgba = wm_image.to_rgba8();

    // Apply opacity by scaling the alpha channel of the watermark.
    let opacity_scale = f32::from(watermark.opacity) / 100.0;
    for pixel in wm_rgba.pixels_mut() {
        pixel.0[3] = (f32::from(pixel.0[3]) * opacity_scale) as u8;
    }

    let (main_w, main_h) = image.dimensions();
    let (wm_w, wm_h) = wm_rgba.dimensions();
    let margin = watermark.margin;

    // Determine which axes actually use the margin based on position.
    let (margin_x, margin_y) = match watermark.position {
        Position::Center => (0, 0),
        Position::Top | Position::Bottom => (0, margin),
        Position::Left | Position::Right => (margin, 0),
        _ => (margin, margin), // corners: TopLeft, TopRight, BottomLeft, BottomRight
    };

    // If the watermark (plus applicable margin) exceeds the main image, reject it.
    // Use u64 arithmetic to avoid u32 overflow with large margin values.
    if u64::from(wm_w) + u64::from(margin_x) > u64::from(main_w)
        || u64::from(wm_h) + u64::from(margin_y) > u64::from(main_h)
    {
        return Err(TransformError::InvalidOptions(
            "watermark image is too large for the output dimensions".to_string(),
        ));
    }

    let (x, y) = watermark_offset(main_w, main_h, wm_w, wm_h, watermark.position, margin);

    let mut canvas = image.to_rgba8();
    imageops::overlay(&mut canvas, &wm_rgba, i64::from(x), i64::from(y));

    Ok(DynamicImage::ImageRgba8(canvas))
}

/// Calculates the top-left offset for a watermark given the main image dimensions,
/// watermark dimensions, position, and margin.
fn watermark_offset(
    main_w: u32,
    main_h: u32,
    wm_w: u32,
    wm_h: u32,
    position: Position,
    margin: u32,
) -> (u32, u32) {
    match position {
        Position::TopLeft => (margin, margin),
        Position::Top => ((main_w.saturating_sub(wm_w)) / 2, margin),
        Position::TopRight => (main_w.saturating_sub(wm_w).saturating_sub(margin), margin),
        Position::Left => (margin, (main_h.saturating_sub(wm_h)) / 2),
        Position::Center => (
            (main_w.saturating_sub(wm_w)) / 2,
            (main_h.saturating_sub(wm_h)) / 2,
        ),
        Position::Right => (
            main_w.saturating_sub(wm_w).saturating_sub(margin),
            (main_h.saturating_sub(wm_h)) / 2,
        ),
        Position::BottomLeft => (margin, main_h.saturating_sub(wm_h).saturating_sub(margin)),
        Position::Bottom => (
            (main_w.saturating_sub(wm_w)) / 2,
            main_h.saturating_sub(wm_h).saturating_sub(margin),
        ),
        Position::BottomRight => (
            main_w.saturating_sub(wm_w).saturating_sub(margin),
            main_h.saturating_sub(wm_h).saturating_sub(margin),
        ),
    }
}

fn scale_dimension(source: u32, target: u32, reference: u32) -> u32 {
    let scaled = ((f64::from(source) * f64::from(target)) / f64::from(reference)).round();
    scaled.max(1.0) as u32
}

fn pad_to_box(
    image: DynamicImage,
    target_width: u32,
    target_height: u32,
    position: Position,
    background: Option<Rgba8>,
    output_format: MediaType,
) -> DynamicImage {
    let resized = image.to_rgba8();
    let (content_width, content_height) = resized.dimensions();
    let fill = background_pixel(background, output_format);
    let mut canvas = RgbaImage::from_pixel(target_width, target_height, fill);
    let (x, y) = position_offset(
        target_width,
        target_height,
        content_width,
        content_height,
        position,
    );

    imageops::overlay(&mut canvas, &resized, i64::from(x), i64::from(y));
    DynamicImage::ImageRgba8(canvas)
}

fn cover_to_box(
    image: DynamicImage,
    target_width: u32,
    target_height: u32,
    position: Position,
    background: Option<Rgba8>,
    output_format: MediaType,
) -> DynamicImage {
    let (original_width, original_height) = image.dimensions();
    let scale = f64::max(
        f64::from(target_width) / f64::from(original_width),
        f64::from(target_height) / f64::from(original_height),
    );
    let resized_width = (f64::from(original_width) * scale).ceil().max(1.0) as u32;
    let resized_height = (f64::from(original_height) * scale).ceil().max(1.0) as u32;
    let resized = image
        .resize_exact(resized_width, resized_height, FilterType::Lanczos3)
        .to_rgba8();

    if resized_width == target_width && resized_height == target_height {
        return DynamicImage::ImageRgba8(resized);
    }

    let fill = background_pixel(background, output_format);
    let mut canvas = RgbaImage::from_pixel(target_width, target_height, fill);
    let (crop_x, crop_y) = position_offset(
        resized_width,
        resized_height,
        target_width,
        target_height,
        position,
    );
    let cropped =
        imageops::crop_imm(&resized, crop_x, crop_y, target_width, target_height).to_image();

    imageops::overlay(&mut canvas, &cropped, 0, 0);
    DynamicImage::ImageRgba8(canvas)
}

fn position_offset(
    container_width: u32,
    container_height: u32,
    content_width: u32,
    content_height: u32,
    position: Position,
) -> (u32, u32) {
    let horizontal_space = container_width.saturating_sub(content_width);
    let vertical_space = container_height.saturating_sub(content_height);

    let x = match position {
        Position::Center | Position::Top | Position::Bottom => horizontal_space / 2,
        Position::Left | Position::TopLeft | Position::BottomLeft => 0,
        Position::Right | Position::TopRight | Position::BottomRight => horizontal_space,
    };

    let y = match position {
        Position::Center | Position::Left | Position::Right => vertical_space / 2,
        Position::Top | Position::TopLeft | Position::TopRight => 0,
        Position::Bottom | Position::BottomLeft | Position::BottomRight => vertical_space,
    };

    (x, y)
}

fn background_pixel(background: Option<Rgba8>, output_format: MediaType) -> Rgba<u8> {
    match background {
        Some(color) => Rgba([color.r, color.g, color.b, color.a]),
        None if matches!(
            output_format,
            MediaType::Jpeg | MediaType::Avif | MediaType::Bmp
        ) =>
        {
            Rgba([255, 255, 255, 255])
        }
        None => Rgba([0, 0, 0, 0]),
    }
}

fn encode_output(
    image: &DynamicImage,
    media_type: MediaType,
    quality: Option<u8>,
    retained_metadata: Option<&RetainedMetadata>,
) -> Result<Vec<u8>, TransformError> {
    let mut bytes = Vec::new();

    match media_type {
        MediaType::Jpeg => {
            let quality = quality.unwrap_or(80);
            let mut encoder = JpegEncoder::new_with_quality(&mut bytes, quality);
            if let Some(retained_metadata) = retained_metadata {
                if let Some(icc_profile) = &retained_metadata.icc_profile {
                    encoder
                        .set_icc_profile(icc_profile.clone())
                        .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
                }
                if let Some(exif) = &retained_metadata.exif_metadata {
                    encoder
                        .set_exif_metadata(exif.clone())
                        .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
                }
            }
            let rgb = image.to_rgb8();
            encoder
                .write_image(&rgb, rgb.width(), rgb.height(), ColorType::Rgb8.into())
                .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
        }
        MediaType::Png => {
            let mut encoder = PngEncoder::new(&mut bytes);
            if let Some(retained_metadata) = retained_metadata {
                if let Some(icc_profile) = &retained_metadata.icc_profile {
                    encoder
                        .set_icc_profile(icc_profile.clone())
                        .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
                }
                if let Some(exif) = &retained_metadata.exif_metadata {
                    encoder
                        .set_exif_metadata(exif.clone())
                        .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
                }
            }
            let rgba = image.to_rgba8();
            encoder
                .write_image(&rgba, rgba.width(), rgba.height(), ColorType::Rgba8.into())
                .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
        }
        MediaType::Webp => {
            let rgba = image.to_rgba8();
            if let Some(q) = quality {
                #[cfg(feature = "webp-lossy")]
                {
                    // Lossy WebP encoding via libwebp (vendored C, no system install needed).
                    let lossy_encoder =
                        webp::Encoder::from_rgba(rgba.as_ref(), rgba.width(), rgba.height());
                    let encoded = lossy_encoder.encode(q as f32);
                    bytes = encoded.to_vec();

                    // libwebp's encoder does not support injecting EXIF/ICC into the output.
                    // Metadata is silently dropped for lossy WebP when quality is specified.
                }
                #[cfg(not(feature = "webp-lossy"))]
                {
                    let _ = q;
                    return Err(TransformError::CapabilityMissing(
                        "lossy WebP encoding is not enabled in this build".to_string(),
                    ));
                }
            } else {
                // Lossless WebP encoding via the image crate's pure-Rust encoder.
                let mut encoder = WebPEncoder::new_lossless(&mut bytes);
                if let Some(retained_metadata) = retained_metadata {
                    if let Some(icc_profile) = &retained_metadata.icc_profile {
                        encoder
                            .set_icc_profile(icc_profile.clone())
                            .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
                    }
                    if let Some(exif) = &retained_metadata.exif_metadata {
                        encoder
                            .set_exif_metadata(exif.clone())
                            .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
                    }
                }
                encoder
                    .write_image(&rgba, rgba.width(), rgba.height(), ColorType::Rgba8.into())
                    .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
            }
        }
        MediaType::Avif => {
            if retained_metadata.is_some_and(|metadata| !metadata.is_empty()) {
                return Err(TransformError::CapabilityMissing(
                    "metadata retention is not implemented for avif output".to_string(),
                ));
            }
            let quality = quality.unwrap_or(80);
            let encoder = AvifEncoder::new_with_speed_quality(&mut bytes, 4, quality);
            let rgba = image.to_rgba8();
            encoder
                .write_image(&rgba, rgba.width(), rgba.height(), ColorType::Rgba8.into())
                .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
        }
        MediaType::Bmp => {
            let rgba = image.to_rgba8();
            image::codecs::bmp::BmpEncoder::new(&mut bytes)
                .write_image(&rgba, rgba.width(), rgba.height(), ColorType::Rgba8.into())
                .map_err(|e: image::ImageError| TransformError::EncodeFailed(e.to_string()))?;
        }
        MediaType::Tiff => {
            let rgba = image.to_rgba8();
            let mut cursor = Cursor::new(bytes);
            image::codecs::tiff::TiffEncoder::new(&mut cursor)
                .write_image(&rgba, rgba.width(), rgba.height(), ColorType::Rgba8.into())
                .map_err(|e: image::ImageError| TransformError::EncodeFailed(e.to_string()))?;
            bytes = cursor.into_inner();
        }
        MediaType::Svg => {
            return Err(TransformError::EncodeFailed(
                "SVG encoding should be handled by transform_svg".into(),
            ));
        }
    }

    Ok(bytes)
}

/// Injects XMP and IPTC metadata into encoded image bytes for formats that support
/// post-encode byte-level insertion. Returns the (possibly modified) bytes and removes
/// successfully injected metadata kinds from the warning list.
///
/// Injection failures (e.g. oversized payloads) are silently ignored — the original
/// encoded bytes are returned unchanged and any pre-inserted `MetadataDropped` warning
/// remains in place. This is intentional: metadata injection is best-effort.
fn inject_metadata(
    mut encoded: Vec<u8>,
    format: MediaType,
    metadata: &RetainedMetadata,
    warnings: &mut Vec<TransformWarning>,
) -> Vec<u8> {
    match format {
        MediaType::Jpeg => {
            // IPTC APP13 first (inserted after SOI), then XMP APP1 (inserted after SOI).
            // Because each insertion goes right after SOI, the final order is:
            // SOI → XMP APP1 → IPTC APP13 → (EXIF APP1 from encoder) → rest
            if let Some(iptc) = &metadata.iptc_metadata
                && let Ok(result) = inject_jpeg_iptc(&encoded, iptc)
            {
                encoded = result;
                warnings.retain(|w| {
                    !matches!(w, TransformWarning::MetadataDropped(MetadataKind::Iptc))
                });
            }
            if let Some(xmp) = &metadata.xmp_metadata
                && let Ok(result) = inject_jpeg_xmp(&encoded, xmp)
            {
                encoded = result;
                warnings
                    .retain(|w| !matches!(w, TransformWarning::MetadataDropped(MetadataKind::Xmp)));
            }
        }
        MediaType::Png => {
            if let Some(xmp) = &metadata.xmp_metadata
                && let Ok(result) = inject_png_xmp(&encoded, xmp)
            {
                encoded = result;
                warnings
                    .retain(|w| !matches!(w, TransformWarning::MetadataDropped(MetadataKind::Xmp)));
            }
            // IPTC has no standard embedding in PNG; warning remains.
        }
        _ => {
            // WebP/AVIF: no post-encode injection supported.
        }
    }

    encoded
}

/// Inserts an XMP APP1 segment into a JPEG byte stream immediately after the SOI marker.
///
/// The XMP APP1 segment uses the namespace `http://ns.adobe.com/xap/1.0/\0` followed
/// by the raw XMP payload. Extended XMP (payloads exceeding the 64KB APP segment limit)
/// is not supported and returns an error.
fn inject_jpeg_xmp(encoded: &[u8], xmp: &[u8]) -> Result<Vec<u8>, TransformError> {
    const XMP_NAMESPACE: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

    if encoded.len() < 2 || encoded[0] != 0xFF || encoded[1] != 0xD8 {
        return Err(TransformError::EncodeFailed(
            "cannot inject XMP: output is not a valid JPEG".into(),
        ));
    }

    let data_len = XMP_NAMESPACE.len() + xmp.len();
    let segment_len = u16::try_from(data_len + 2).map_err(|_| {
        TransformError::EncodeFailed(
            "XMP payload exceeds the JPEG APP1 segment size limit (64KB)".into(),
        )
    })?;
    let mut result = Vec::with_capacity(encoded.len() + 4 + data_len);
    result.extend_from_slice(&encoded[..2]); // SOI
    result.push(0xFF);
    result.push(0xE1); // APP1 marker
    result.extend_from_slice(&segment_len.to_be_bytes());
    result.extend_from_slice(XMP_NAMESPACE);
    result.extend_from_slice(xmp);
    result.extend_from_slice(&encoded[2..]); // rest of JPEG
    Ok(result)
}

/// Inserts an IPTC APP13 segment into a JPEG byte stream immediately after the SOI marker.
///
/// The IPTC data is wrapped in a Photoshop 3.0 Image Resource Block (8BIM) with
/// resource type 0x0404 (IPTC-NAA record). This structure is required for IPTC readers
/// to correctly parse the embedded data.
fn inject_jpeg_iptc(encoded: &[u8], iptc: &[u8]) -> Result<Vec<u8>, TransformError> {
    const PHOTOSHOP_NAMESPACE: &[u8] = b"Photoshop 3.0\0";
    const BIM_SIGNATURE: &[u8] = b"8BIM";
    const IPTC_RESOURCE_TYPE: u16 = 0x0404;

    if encoded.len() < 2 || encoded[0] != 0xFF || encoded[1] != 0xD8 {
        return Err(TransformError::EncodeFailed(
            "cannot inject IPTC: output is not a valid JPEG".into(),
        ));
    }

    // Build the 8BIM resource block:
    // "8BIM" (4) + resource_type (2) + pascal_string_len (1, value 0) + padding (1)
    // + data_size (4) + iptc_data + optional padding byte
    let resource_header_len = BIM_SIGNATURE.len() + 2 + 1 + 1 + 4; // 12 bytes
    let iptc_padded_len = if iptc.len().is_multiple_of(2) {
        iptc.len()
    } else {
        iptc.len() + 1
    };
    let resource_block_len = resource_header_len + iptc_padded_len;

    let data_len = PHOTOSHOP_NAMESPACE.len() + resource_block_len;
    let segment_len = u16::try_from(data_len + 2).map_err(|_| {
        TransformError::EncodeFailed(
            "IPTC payload exceeds the JPEG APP13 segment size limit (64KB)".into(),
        )
    })?;
    let mut result = Vec::with_capacity(encoded.len() + 4 + data_len);
    result.extend_from_slice(&encoded[..2]); // SOI
    result.push(0xFF);
    result.push(0xED); // APP13 marker
    result.extend_from_slice(&segment_len.to_be_bytes());
    result.extend_from_slice(PHOTOSHOP_NAMESPACE);
    // 8BIM resource block
    result.extend_from_slice(BIM_SIGNATURE);
    result.extend_from_slice(&IPTC_RESOURCE_TYPE.to_be_bytes());
    result.push(0x00); // Pascal string length (empty name)
    result.push(0x00); // Padding to even boundary
    result.extend_from_slice(&(iptc.len() as u32).to_be_bytes());
    result.extend_from_slice(iptc);
    if !iptc.len().is_multiple_of(2) {
        result.push(0x00); // Pad data to even length
    }
    result.extend_from_slice(&encoded[2..]); // rest of JPEG
    Ok(result)
}

/// Inserts an XMP iTXt chunk into a PNG byte stream after the IHDR chunk.
///
/// The iTXt chunk uses the keyword `XML:com.adobe.xmp` as specified by the XMP standard
/// for PNG embedding. The chunk includes a proper CRC-32 computed over the chunk type
/// and data as required by the PNG specification.
///
/// IPTC has no standard PNG embedding mechanism, so only XMP is supported.
fn inject_png_xmp(encoded: &[u8], xmp: &[u8]) -> Result<Vec<u8>, TransformError> {
    const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    const ITXT_TYPE: &[u8] = b"iTXt";
    const XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp";

    if encoded.len() < 8 || &encoded[..8] != PNG_SIGNATURE {
        return Err(TransformError::EncodeFailed(
            "cannot inject XMP: output is not a valid PNG".into(),
        ));
    }

    // Find the end of the IHDR chunk to insert after it.
    // PNG structure: 8-byte signature, then chunks of (4-byte length, 4-byte type, data, 4-byte CRC).
    if encoded.len() < 8 + 4 + 4 {
        return Err(TransformError::EncodeFailed(
            "cannot inject XMP: PNG is too short to contain IHDR".into(),
        ));
    }
    let ihdr_data_len =
        u32::from_be_bytes([encoded[8], encoded[9], encoded[10], encoded[11]]) as usize;
    let ihdr_end = 8 + 4 + 4 + ihdr_data_len + 4; // signature + length + type + data + CRC
    if encoded.len() < ihdr_end {
        return Err(TransformError::EncodeFailed(
            "cannot inject XMP: PNG IHDR chunk is truncated".into(),
        ));
    }

    // Build the iTXt chunk data:
    // keyword (null-terminated) + compression_flag (0) + compression_method (0)
    // + language_tag (empty, null-terminated) + translated_keyword (empty, null-terminated)
    // + text (XMP payload)
    let mut chunk_data = Vec::with_capacity(XMP_KEYWORD.len() + 5 + xmp.len());
    chunk_data.extend_from_slice(XMP_KEYWORD);
    chunk_data.push(0x00); // Null terminator for keyword
    chunk_data.push(0x00); // Compression flag (uncompressed)
    chunk_data.push(0x00); // Compression method
    chunk_data.push(0x00); // Language tag (empty, null-terminated)
    chunk_data.push(0x00); // Translated keyword (empty, null-terminated)
    chunk_data.extend_from_slice(xmp);

    let chunk_data_len = chunk_data.len() as u32;

    // Compute CRC-32 over chunk type + chunk data
    let mut crc_input = Vec::with_capacity(4 + chunk_data.len());
    crc_input.extend_from_slice(ITXT_TYPE);
    crc_input.extend_from_slice(&chunk_data);
    let crc = png_crc32(&crc_input);

    // Assemble the full chunk: length + type + data + CRC
    let chunk_total = 4 + 4 + chunk_data.len() + 4;
    let mut result = Vec::with_capacity(encoded.len() + chunk_total);
    result.extend_from_slice(&encoded[..ihdr_end]); // signature + IHDR
    result.extend_from_slice(&chunk_data_len.to_be_bytes());
    result.extend_from_slice(ITXT_TYPE);
    result.extend_from_slice(&chunk_data);
    result.extend_from_slice(&crc.to_be_bytes());
    result.extend_from_slice(&encoded[ihdr_end..]); // remaining chunks
    Ok(result)
}

/// Computes the CRC-32 used by the PNG specification (ISO 3309 / ITU-T V.42).
fn png_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    crc ^ 0xFFFF_FFFF
}

#[derive(Debug, Default)]
struct RetainedMetadata {
    exif_metadata: Option<Vec<u8>>,
    icc_profile: Option<Vec<u8>>,
    xmp_metadata: Option<Vec<u8>>,
    iptc_metadata: Option<Vec<u8>>,
}

impl RetainedMetadata {
    fn is_empty(&self) -> bool {
        self.exif_metadata.is_none()
            && self.icc_profile.is_none()
            && self.xmp_metadata.is_none()
            && self.iptc_metadata.is_none()
    }

    fn retain_exif_only(mut self) -> Self {
        self.icc_profile = None;
        self.xmp_metadata = None;
        self.iptc_metadata = None;
        self
    }

    /// Retains metadata that can be preserved for the given output format.
    ///
    /// - JPEG: EXIF, ICC, XMP (APP1 injection), IPTC (APP13 injection)
    /// - PNG: EXIF, ICC, XMP (iTXt injection). IPTC has no standard PNG embedding.
    /// - WebP/AVIF: EXIF and ICC only (via encoder API or not at all).
    fn retain_supported_keep_all(mut self, output_format: MediaType) -> Self {
        match output_format {
            MediaType::Jpeg => {
                // All four metadata types are supported for JPEG output.
            }
            MediaType::Png => {
                // XMP via iTXt injection. IPTC has no standard PNG embedding.
                self.iptc_metadata = None;
            }
            _ => {
                // WebP, AVIF: no post-encode injection support.
                self.xmp_metadata = None;
                self.iptc_metadata = None;
            }
        }
        self
    }
}

fn extract_retained_metadata(
    input: &Artifact,
    metadata_policy: MetadataPolicy,
    auto_orient: bool,
    output_format: MediaType,
) -> Result<(Option<RetainedMetadata>, Vec<TransformWarning>), TransformError> {
    let mut warnings = Vec::new();

    if matches!(metadata_policy, MetadataPolicy::StripAll) {
        return Ok((None, warnings));
    }

    let mut metadata = read_input_metadata(input)?;
    if let Some(exif_chunk) = metadata.exif_metadata.as_mut()
        && auto_orient
        && matches!(input.media_type, MediaType::Jpeg)
    {
        let _ = Orientation::remove_from_exif_chunk(exif_chunk);
    }

    let metadata = match metadata_policy {
        MetadataPolicy::StripAll => return Ok((None, warnings)),
        MetadataPolicy::PreserveExif => metadata.retain_exif_only(),
        MetadataPolicy::KeepAll => {
            // Emit warnings for metadata that will be dropped for this output format.
            // Metadata that can be injected post-encode will have its warning removed
            // later by inject_metadata on success.
            if metadata.xmp_metadata.is_some()
                && !matches!(output_format, MediaType::Jpeg | MediaType::Png)
            {
                warnings.push(TransformWarning::MetadataDropped(MetadataKind::Xmp));
            }
            if metadata.iptc_metadata.is_some() && !matches!(output_format, MediaType::Jpeg) {
                warnings.push(TransformWarning::MetadataDropped(MetadataKind::Iptc));
            }
            metadata.retain_supported_keep_all(output_format)
        }
    };

    if matches!(output_format, MediaType::Avif) && !metadata.is_empty() {
        return Err(TransformError::CapabilityMissing(
            "metadata retention is not implemented for avif output".to_string(),
        ));
    }

    if metadata.is_empty() {
        return Ok((None, warnings));
    }

    Ok((Some(metadata), warnings))
}

fn read_input_metadata(input: &Artifact) -> Result<RetainedMetadata, TransformError> {
    match input.media_type {
        MediaType::Jpeg => {
            let mut decoder = JpegDecoder::new(Cursor::new(&input.bytes))
                .map_err(|error| TransformError::DecodeFailed(error.to_string()))?;
            Ok(RetainedMetadata {
                exif_metadata: decoder
                    .exif_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                icc_profile: decoder
                    .icc_profile()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                xmp_metadata: decoder
                    .xmp_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                iptc_metadata: decoder
                    .iptc_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
            })
        }
        MediaType::Png => {
            let mut decoder = PngDecoder::new(Cursor::new(&input.bytes))
                .map_err(|error| TransformError::DecodeFailed(error.to_string()))?;
            Ok(RetainedMetadata {
                exif_metadata: decoder
                    .exif_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                icc_profile: decoder
                    .icc_profile()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                xmp_metadata: decoder
                    .xmp_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                iptc_metadata: decoder
                    .iptc_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
            })
        }
        MediaType::Webp => {
            let mut decoder = WebPDecoder::new(Cursor::new(&input.bytes))
                .map_err(|error| TransformError::DecodeFailed(error.to_string()))?;
            Ok(RetainedMetadata {
                exif_metadata: decoder
                    .exif_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                icc_profile: decoder
                    .icc_profile()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                xmp_metadata: decoder
                    .xmp_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
                iptc_metadata: decoder
                    .iptc_metadata()
                    .map_err(|error| TransformError::DecodeFailed(error.to_string()))?,
            })
        }
        MediaType::Avif | MediaType::Svg | MediaType::Bmp | MediaType::Tiff => {
            Ok(RetainedMetadata::default())
        }
    }
}

fn output_has_alpha(image: &DynamicImage, media_type: MediaType) -> bool {
    match media_type {
        MediaType::Jpeg => false,
        MediaType::Png
        | MediaType::Webp
        | MediaType::Avif
        | MediaType::Svg
        | MediaType::Bmp
        | MediaType::Tiff => image.color().has_alpha(),
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_exif_orientation, transform_raster};
    use crate::core::{
        Artifact, ArtifactMetadata, Fit, MediaType, MetadataPolicy, Position, Rotation,
        TransformOptions, TransformRequest, WatermarkInput,
    };
    use crate::{RawArtifact, Rgba8, TransformError, sniff_artifact};
    use image::codecs::jpeg::JpegDecoder;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngDecoder;
    use image::codecs::png::PngEncoder;
    use image::codecs::webp::WebPDecoder;
    use image::codecs::webp::WebPEncoder;
    use image::metadata::Orientation;
    use image::{
        ColorType, DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat, Rgba,
        RgbaImage,
    };
    use std::io::Cursor;

    fn png_artifact(width: u32, height: u32, fill: Rgba<u8>) -> Artifact {
        let image = RgbaImage::from_pixel(width, height, fill);
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, width, height, ColorType::Rgba8.into())
            .expect("encode png");

        Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(width),
                height: Some(height),
                frame_count: 1,
                duration: None,
                has_alpha: Some(fill[3] < u8::MAX),
            },
        )
    }

    fn jpeg_artifact_with_metadata(
        width: u32,
        height: u32,
        orientation: Option<u16>,
        icc_profile: Option<&[u8]>,
    ) -> Artifact {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([10, 20, 30]));
        let mut bytes = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 80);
        if let Some(orientation) = orientation {
            let exif = vec![
                0x49,
                0x49,
                0x2A,
                0x00,
                0x08,
                0x00,
                0x00,
                0x00,
                0x01,
                0x00,
                0x12,
                0x01,
                0x03,
                0x00,
                0x01,
                0x00,
                0x00,
                0x00,
                orientation as u8,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
            ];
            encoder
                .set_exif_metadata(exif)
                .expect("set jpeg exif metadata");
        }
        if let Some(icc_profile) = icc_profile {
            encoder
                .set_icc_profile(icc_profile.to_vec())
                .expect("set jpeg icc profile");
        }
        encoder
            .write_image(&image, width, height, ColorType::Rgb8.into())
            .expect("encode jpeg");

        Artifact::new(
            bytes,
            MediaType::Jpeg,
            ArtifactMetadata {
                width: Some(width),
                height: Some(height),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
            },
        )
    }

    fn png_artifact_with_metadata(
        width: u32,
        height: u32,
        orientation: Option<u16>,
        icc_profile: Option<&[u8]>,
    ) -> Artifact {
        let image = RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        let mut encoder = PngEncoder::new(&mut bytes);
        if let Some(orientation) = orientation {
            let exif = vec![
                0x49,
                0x49,
                0x2A,
                0x00,
                0x08,
                0x00,
                0x00,
                0x00,
                0x01,
                0x00,
                0x12,
                0x01,
                0x03,
                0x00,
                0x01,
                0x00,
                0x00,
                0x00,
                orientation as u8,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
            ];
            encoder
                .set_exif_metadata(exif)
                .expect("set png exif metadata");
        }
        if let Some(icc_profile) = icc_profile {
            encoder
                .set_icc_profile(icc_profile.to_vec())
                .expect("set png icc profile");
        }
        encoder
            .write_image(&image, width, height, ColorType::Rgba8.into())
            .expect("encode png");

        Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(width),
                height: Some(height),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
            },
        )
    }

    fn webp_artifact_with_metadata(
        width: u32,
        height: u32,
        orientation: Option<u16>,
        icc_profile: Option<&[u8]>,
    ) -> Artifact {
        let image = RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        let mut encoder = WebPEncoder::new_lossless(&mut bytes);
        if let Some(orientation) = orientation {
            let exif = vec![
                0x49,
                0x49,
                0x2A,
                0x00,
                0x08,
                0x00,
                0x00,
                0x00,
                0x01,
                0x00,
                0x12,
                0x01,
                0x03,
                0x00,
                0x01,
                0x00,
                0x00,
                0x00,
                orientation as u8,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
            ];
            encoder
                .set_exif_metadata(exif)
                .expect("set webp exif metadata");
        }
        if let Some(icc_profile) = icc_profile {
            encoder
                .set_icc_profile(icc_profile.to_vec())
                .expect("set webp icc profile");
        }
        encoder
            .write_image(&image, width, height, ColorType::Rgba8.into())
            .expect("encode webp");

        Artifact::new(
            bytes,
            MediaType::Webp,
            ArtifactMetadata {
                width: Some(width),
                height: Some(height),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
            },
        )
    }

    /// Creates a JPEG artifact with XMP and IPTC segments manually injected.
    fn jpeg_with_xmp_iptc() -> Artifact {
        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
        let mut base_bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut base_bytes, 80)
            .write_image(&image, 2, 2, ColorType::Rgb8.into())
            .expect("encode jpeg");

        let xmp_ns = b"http://ns.adobe.com/xap/1.0/\0";
        let xmp_payload = b"<x:xmpmeta>test</x:xmpmeta>";
        let xmp_data_len = xmp_ns.len() + xmp_payload.len();
        let xmp_segment_len = (xmp_data_len + 2) as u16;
        let mut xmp_segment = vec![0xFF, 0xE1];
        xmp_segment.extend_from_slice(&xmp_segment_len.to_be_bytes());
        xmp_segment.extend_from_slice(xmp_ns);
        xmp_segment.extend_from_slice(xmp_payload);

        let iptc_ns = b"Photoshop 3.0\0";
        let iptc_payload = b"\x1c\x02\x00\x00\x02OK";
        let iptc_data_len = iptc_ns.len() + iptc_payload.len();
        let iptc_segment_len = (iptc_data_len + 2) as u16;
        let mut iptc_segment = vec![0xFF, 0xED];
        iptc_segment.extend_from_slice(&iptc_segment_len.to_be_bytes());
        iptc_segment.extend_from_slice(iptc_ns);
        iptc_segment.extend_from_slice(iptc_payload);

        let mut jpeg_with_metadata = Vec::new();
        jpeg_with_metadata.extend_from_slice(&base_bytes[..2]); // SOI
        jpeg_with_metadata.extend_from_slice(&xmp_segment);
        jpeg_with_metadata.extend_from_slice(&iptc_segment);
        jpeg_with_metadata.extend_from_slice(&base_bytes[2..]); // rest of JPEG
        Artifact::new(
            jpeg_with_metadata,
            MediaType::Jpeg,
            ArtifactMetadata {
                width: Some(2),
                height: Some(2),
                ..ArtifactMetadata::default()
            },
        )
    }

    fn top_left_pixel(bytes: &[u8], format: ImageFormat) -> [u8; 4] {
        image::load_from_memory_with_format(bytes, format)
            .expect("decode image")
            .to_rgba8()
            .get_pixel(0, 0)
            .0
    }

    #[test]
    fn transform_raster_can_convert_png_to_jpeg() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                ..TransformOptions::default()
            },
        ))
        .expect("convert png to jpeg");

        assert_eq!(result.artifact.media_type, MediaType::Jpeg);
        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(3));
        assert_eq!(result.artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn transform_raster_resizes_with_single_dimension() {
        let artifact = png_artifact(4, 2, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                width: Some(8),
                ..TransformOptions::default()
            },
        ))
        .expect("resize with width");

        assert_eq!(result.artifact.metadata.width, Some(8));
        assert_eq!(result.artifact.metadata.height, Some(4));
    }

    #[test]
    fn transform_raster_can_pad_with_background_for_contain() {
        let artifact = png_artifact(4, 2, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                width: Some(8),
                height: Some(8),
                fit: Some(Fit::Contain),
                position: Some(Position::TopLeft),
                background: Some(Rgba8 {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
                ..TransformOptions::default()
            },
        ))
        .expect("contain with background");

        assert_eq!(result.artifact.metadata.width, Some(8));
        assert_eq!(result.artifact.metadata.height, Some(8));
        assert_eq!(
            top_left_pixel(&result.artifact.bytes, ImageFormat::Png),
            [10, 20, 30, 255]
        );
    }

    #[test]
    fn transform_raster_can_cover_the_target_box() {
        let artifact = png_artifact(4, 2, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                width: Some(2),
                height: Some(2),
                fit: Some(Fit::Cover),
                ..TransformOptions::default()
            },
        ))
        .expect("cover resize");

        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(2));
    }

    #[test]
    fn transform_raster_can_rotate_output() {
        let artifact = png_artifact(4, 2, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                rotate: Rotation::Deg90,
                ..TransformOptions::default()
            },
        ))
        .expect("rotate image");

        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(4));
    }

    #[test]
    fn transform_raster_preserves_exif_and_normalizes_orientation() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(6), None);
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                strip_metadata: false,
                preserve_exif: true,
                ..TransformOptions::default()
            },
        ))
        .expect("preserve exif");

        let mut decoder =
            JpegDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode jpeg");
        let exif = decoder
            .exif_metadata()
            .expect("read jpeg exif")
            .expect("retained exif");

        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(4));
        assert_eq!(
            Orientation::from_exif_chunk(&exif),
            Some(Orientation::NoTransforms)
        );
    }

    #[test]
    fn transform_raster_preserve_exif_drops_icc_profile() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(6), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                strip_metadata: false,
                preserve_exif: true,
                ..TransformOptions::default()
            },
        ))
        .expect("preserve exif only");

        let mut decoder =
            JpegDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode jpeg");

        assert_eq!(decoder.icc_profile().expect("read jpeg icc profile"), None);
    }

    #[test]
    fn transform_raster_preserve_exif_keeps_png_orientation_when_pixels_are_not_auto_oriented() {
        let artifact = png_artifact_with_metadata(4, 2, Some(6), None);
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                strip_metadata: false,
                preserve_exif: true,
                ..TransformOptions::default()
            },
        ))
        .expect("preserve png exif");

        let mut decoder = PngDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode png");
        let exif = decoder
            .exif_metadata()
            .expect("read png exif")
            .expect("retained png exif");

        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(2));
        assert_eq!(
            Orientation::from_exif_chunk(&exif),
            Some(Orientation::Rotate90)
        );
    }

    #[test]
    fn transform_raster_keeps_supported_metadata_when_requested() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(6), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("keep metadata");

        let mut decoder =
            JpegDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode jpeg");
        let exif = decoder
            .exif_metadata()
            .expect("read jpeg exif")
            .expect("retained exif");
        let icc_profile = decoder
            .icc_profile()
            .expect("read jpeg icc")
            .expect("retained icc");

        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(4));
        assert_eq!(
            Orientation::from_exif_chunk(&exif),
            Some(Orientation::NoTransforms)
        );
        assert_eq!(icc_profile, b"demo-icc-profile".to_vec());
    }

    #[test]
    fn transform_raster_keeps_metadata_in_png_output() {
        let artifact = jpeg_artifact_with_metadata(4, 2, None, Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("keep metadata in png output");

        let mut decoder = PngDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode png");
        let icc_profile = decoder
            .icc_profile()
            .expect("read png icc")
            .expect("retained png icc");

        assert_eq!(icc_profile, b"demo-icc-profile".to_vec());
    }

    #[test]
    fn transform_raster_keeps_metadata_from_webp_input() {
        let artifact = webp_artifact_with_metadata(4, 2, None, Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("keep metadata from webp input");

        let mut decoder =
            WebPDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode webp");
        let icc_profile = decoder
            .icc_profile()
            .expect("read webp icc")
            .expect("retained webp icc");

        assert_eq!(icc_profile, b"demo-icc-profile".to_vec());
    }

    #[test]
    fn transform_raster_keep_metadata_succeeds_when_input_has_no_metadata() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("keep metadata should succeed when nothing is present");

        assert_eq!(result.artifact.media_type, MediaType::Png);
        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(3));
    }

    #[test]
    fn transform_raster_rejects_preserved_metadata_for_avif_output() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(6), Some(b"demo-icc-profile"));
        let err = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Avif),
                strip_metadata: false,
                preserve_exif: true,
                ..TransformOptions::default()
            },
        ))
        .expect_err("avif output should reject preserved exif");

        assert_eq!(
            err,
            TransformError::CapabilityMissing(
                "metadata retention is not implemented for avif output".to_string()
            )
        );
    }

    #[cfg(feature = "webp-lossy")]
    #[test]
    fn transform_raster_encodes_lossy_webp_with_quality() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(80),
                ..TransformOptions::default()
            },
        ))
        .expect("lossy webp encode should succeed");

        assert_eq!(result.artifact.media_type, MediaType::Webp);
        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(3));
        // Lossy output should be smaller than lossless for non-trivial images.
        assert!(!result.artifact.bytes.is_empty());
    }

    #[cfg(feature = "webp-lossy")]
    #[test]
    fn transform_raster_lossy_webp_smaller_at_lower_quality() {
        let artifact = png_artifact(16, 16, Rgba([128, 64, 32, 255]));
        let high_q = transform_raster(TransformRequest::new(
            artifact.clone(),
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(95),
                ..TransformOptions::default()
            },
        ))
        .expect("high quality webp");

        let low_q = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(10),
                ..TransformOptions::default()
            },
        ))
        .expect("low quality webp");

        // Lower quality should generally produce smaller output.
        assert!(
            low_q.artifact.bytes.len() <= high_q.artifact.bytes.len(),
            "low quality ({}) should be <= high quality ({})",
            low_q.artifact.bytes.len(),
            high_q.artifact.bytes.len()
        );
    }

    #[test]
    fn transform_raster_can_convert_png_to_avif() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Avif),
                quality: Some(72),
                ..TransformOptions::default()
            },
        ))
        .expect("avif encode should succeed");
        let sniffed = sniff_artifact(RawArtifact::new(result.artifact.bytes.clone(), None))
            .expect("sniff avif output");

        assert_eq!(result.artifact.media_type, MediaType::Avif);
        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(3));
        assert_eq!(sniffed.media_type, MediaType::Avif);
    }

    #[test]
    fn transform_raster_round_trips_avif_decode() {
        // Encode a known PNG to AVIF, then decode the AVIF back to PNG.
        let source = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let avif_result = transform_raster(TransformRequest::new(
            source,
            TransformOptions {
                format: Some(MediaType::Avif),
                ..TransformOptions::default()
            },
        ))
        .expect("avif encode should succeed");

        let avif_artifact = avif_result.artifact;
        assert_eq!(avif_artifact.media_type, MediaType::Avif);

        // Now decode the AVIF back to PNG.
        let png_result = transform_raster(TransformRequest::new(
            avif_artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("avif decode should succeed");

        assert_eq!(png_result.artifact.media_type, MediaType::Png);
        assert_eq!(png_result.artifact.metadata.width, Some(4));
        assert_eq!(png_result.artifact.metadata.height, Some(3));
    }

    #[test]
    fn transform_raster_decodes_avif_with_resize() {
        let source = png_artifact(8, 6, Rgba([100, 150, 200, 255]));
        let avif_result = transform_raster(TransformRequest::new(
            source,
            TransformOptions {
                format: Some(MediaType::Avif),
                ..TransformOptions::default()
            },
        ))
        .expect("avif encode should succeed");

        let result = transform_raster(TransformRequest::new(
            avif_result.artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(4),
                height: Some(3),
                ..TransformOptions::default()
            },
        ))
        .expect("avif decode with resize should succeed");

        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(3));
    }

    #[test]
    fn transform_raster_rejects_invalid_avif_data() {
        let artifact = Artifact::new(
            vec![0, 1, 2, 3],
            MediaType::Avif,
            ArtifactMetadata {
                width: Some(1),
                height: Some(1),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
            },
        );
        let err = transform_raster(TransformRequest::new(artifact, TransformOptions::default()))
            .expect_err("invalid avif should fail");

        assert!(
            matches!(err, TransformError::DecodeFailed(_)),
            "expected DecodeFailed, got {err:?}"
        );
    }

    #[test]
    fn apply_exif_orientation_rotates_dimensions() {
        let image =
            image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([10, 20, 30, 255])));
        let rotated = apply_exif_orientation(image, 6);

        assert_eq!(rotated.dimensions(), (2, 4));
    }

    #[test]
    fn input_pixel_limit_accepts_boundary() {
        use super::check_input_pixel_limit;
        // 10000 * 10000 = 100_000_000 == MAX_DECODED_PIXELS
        let input = Artifact::new(
            vec![],
            MediaType::Png,
            ArtifactMetadata {
                width: Some(10000),
                height: Some(10000),
                ..ArtifactMetadata::default()
            },
        );
        check_input_pixel_limit(&input).unwrap();
    }

    #[test]
    fn input_pixel_limit_rejects_oversized() {
        use super::check_input_pixel_limit;
        // 10001 * 10000 = 100_010_000 > MAX_DECODED_PIXELS
        let input = Artifact::new(
            vec![],
            MediaType::Png,
            ArtifactMetadata {
                width: Some(10001),
                height: Some(10000),
                ..ArtifactMetadata::default()
            },
        );
        let err = check_input_pixel_limit(&input).unwrap_err();
        assert!(matches!(err, TransformError::LimitExceeded(_)));
    }

    #[test]
    fn output_pixel_limit_accepts_boundary() {
        use super::check_output_pixel_limit;
        // 8192 * 8192 = 67_108_864 == MAX_OUTPUT_PIXELS
        let image = image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(
            8192,
            8192,
            Rgba([0, 0, 0, 255]),
        ));
        check_output_pixel_limit(&image, Some(8192), Some(8192)).unwrap();
    }

    #[test]
    fn output_pixel_limit_rejects_oversized() {
        use super::check_output_pixel_limit;
        // 8193 * 8192 = 67_116_032 > MAX_OUTPUT_PIXELS (67_108_864)
        let image =
            image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 255])));
        let err = check_output_pixel_limit(&image, Some(8193), Some(8192)).unwrap_err();
        assert!(matches!(err, TransformError::LimitExceeded(_)));
    }

    #[test]
    fn transform_rejects_oversized_output() {
        let input = png_artifact(100, 100, Rgba([10, 20, 30, 255]));
        let err = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                width: Some(8193),
                height: Some(8192),
                ..TransformOptions::default()
            },
        ))
        .unwrap_err();
        assert!(matches!(err, TransformError::LimitExceeded(_)));
        assert!(err.to_string().contains("output image"));
    }

    #[test]
    fn keep_metadata_retains_xmp_iptc_for_jpeg_output() {
        use super::extract_retained_metadata;

        let artifact = jpeg_with_xmp_iptc();

        let (retained, warnings) =
            extract_retained_metadata(&artifact, MetadataPolicy::KeepAll, false, MediaType::Jpeg)
                .expect("should not error");

        // For JPEG output, XMP and IPTC are retained (injected post-encode), no warnings.
        assert!(
            warnings.is_empty(),
            "expected no warnings, got: {warnings:?}"
        );

        let metadata = retained.expect("metadata should be retained");
        assert!(metadata.xmp_metadata.is_some(), "XMP should be retained");
        assert!(metadata.iptc_metadata.is_some(), "IPTC should be retained");
    }

    #[test]
    fn keep_metadata_drops_iptc_for_png_output_with_warning() {
        use super::extract_retained_metadata;
        use crate::core::{MetadataKind, TransformWarning};

        let artifact = jpeg_with_xmp_iptc();

        let (retained, warnings) =
            extract_retained_metadata(&artifact, MetadataPolicy::KeepAll, false, MediaType::Png)
                .expect("should not error");

        // PNG supports XMP via iTXt but not IPTC.
        assert_eq!(warnings.len(), 1);
        assert_eq!(
            warnings[0],
            TransformWarning::MetadataDropped(MetadataKind::Iptc)
        );

        let metadata = retained.expect("metadata should be retained");
        assert!(
            metadata.xmp_metadata.is_some(),
            "XMP should be retained for PNG"
        );
        assert!(
            metadata.iptc_metadata.is_none(),
            "IPTC should be dropped for PNG"
        );
    }

    #[test]
    fn keep_metadata_drops_xmp_iptc_for_webp_output_with_warnings() {
        use super::extract_retained_metadata;
        use crate::core::{MetadataKind, TransformWarning};

        let artifact = jpeg_with_xmp_iptc();

        let (_, warnings) =
            extract_retained_metadata(&artifact, MetadataPolicy::KeepAll, false, MediaType::Webp)
                .expect("should not error");

        // WebP does not support XMP/IPTC injection.
        assert_eq!(warnings.len(), 2);
        assert_eq!(
            warnings[0],
            TransformWarning::MetadataDropped(MetadataKind::Xmp)
        );
        assert_eq!(
            warnings[1],
            TransformWarning::MetadataDropped(MetadataKind::Iptc)
        );
    }

    #[test]
    fn keep_metadata_no_warnings_when_no_xmp_iptc() {
        use super::extract_retained_metadata;

        let artifact = jpeg_artifact_with_metadata(4, 3, Some(6), None);
        let (_, warnings) =
            extract_retained_metadata(&artifact, MetadataPolicy::KeepAll, false, MediaType::Jpeg)
                .expect("should succeed");

        assert!(warnings.is_empty());
    }

    #[test]
    fn strip_metadata_produces_no_warnings() {
        use super::extract_retained_metadata;

        let artifact = jpeg_artifact_with_metadata(4, 3, Some(6), None);
        let (retained, warnings) =
            extract_retained_metadata(&artifact, MetadataPolicy::StripAll, false, MediaType::Jpeg)
                .expect("should succeed");

        assert!(retained.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn check_deadline_accepts_within_limit() {
        use super::check_deadline;
        use std::time::Duration;

        check_deadline(Duration::from_secs(29), Duration::from_secs(30), "decode").unwrap();
    }

    #[test]
    fn check_deadline_rejects_exceeded() {
        use super::check_deadline;
        use std::time::Duration;

        let err =
            check_deadline(Duration::from_secs(31), Duration::from_secs(30), "decode").unwrap_err();
        assert!(matches!(err, TransformError::LimitExceeded(_)));
        assert!(err.to_string().contains("decode"));
        assert!(err.to_string().contains("30s"));
    }

    #[test]
    fn transform_with_deadline_succeeds_for_small_image() {
        use std::time::Duration;

        let input = png_artifact(2, 2, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                deadline: Some(Duration::from_secs(30)),
                ..TransformOptions::default()
            },
        ))
        .unwrap();
        assert_eq!(result.artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn inject_jpeg_xmp_inserts_app1_segment() {
        use super::inject_jpeg_xmp;

        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
        let mut jpeg_bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg_bytes, 80)
            .write_image(&image, 2, 2, ColorType::Rgb8.into())
            .expect("encode jpeg");

        let xmp_payload = b"<x:xmpmeta>hello</x:xmpmeta>";
        let result = inject_jpeg_xmp(&jpeg_bytes, xmp_payload).expect("inject XMP");

        // Verify the output starts with SOI + APP1 marker
        assert_eq!(&result[..2], &[0xFF, 0xD8]);
        assert_eq!(&result[2..4], &[0xFF, 0xE1]);

        // Verify the XMP namespace is present
        let xmp_ns = b"http://ns.adobe.com/xap/1.0/\0";
        assert!(result.windows(xmp_ns.len()).any(|w| w == xmp_ns));

        // Verify the output is still a valid JPEG
        image::load_from_memory_with_format(&result, ImageFormat::Jpeg)
            .expect("injected JPEG should still decode");
    }

    #[test]
    fn inject_jpeg_iptc_inserts_app13_segment() {
        use super::inject_jpeg_iptc;

        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
        let mut jpeg_bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg_bytes, 80)
            .write_image(&image, 2, 2, ColorType::Rgb8.into())
            .expect("encode jpeg");

        let iptc_payload = b"\x1c\x02\x00\x00\x02OK";
        let result = inject_jpeg_iptc(&jpeg_bytes, iptc_payload).expect("inject IPTC");

        // Verify SOI + APP13 marker
        assert_eq!(&result[..2], &[0xFF, 0xD8]);
        assert_eq!(&result[2..4], &[0xFF, 0xED]);

        // Verify Photoshop namespace and 8BIM marker
        assert!(
            result
                .windows(b"Photoshop 3.0\0".len())
                .any(|w| w == b"Photoshop 3.0\0")
        );
        assert!(result.windows(b"8BIM".len()).any(|w| w == b"8BIM"));

        // Verify the output is still a valid JPEG
        image::load_from_memory_with_format(&result, ImageFormat::Jpeg)
            .expect("injected JPEG should still decode");
    }

    #[test]
    fn inject_png_xmp_inserts_itxt_chunk() {
        use super::inject_png_xmp;

        let image = RgbaImage::from_pixel(2, 2, Rgba([10, 20, 30, 255]));
        let mut png_bytes = Vec::new();
        PngEncoder::new(&mut png_bytes)
            .write_image(&image, 2, 2, ColorType::Rgba8.into())
            .expect("encode png");

        let xmp_payload = b"<x:xmpmeta>hello</x:xmpmeta>";
        let result = inject_png_xmp(&png_bytes, xmp_payload).expect("inject XMP into PNG");

        // Verify the XMP keyword is present
        assert!(
            result
                .windows(b"XML:com.adobe.xmp".len())
                .any(|w| w == b"XML:com.adobe.xmp")
        );

        // Verify the iTXt chunk type is present
        assert!(result.windows(b"iTXt".len()).any(|w| w == b"iTXt"));

        // Verify the output is still a valid PNG
        image::load_from_memory_with_format(&result, ImageFormat::Png)
            .expect("injected PNG should still decode");
    }

    #[test]
    fn inject_jpeg_xmp_rejects_non_jpeg() {
        use super::inject_jpeg_xmp;

        let result = inject_jpeg_xmp(b"not a jpeg", b"<xmp/>");
        assert!(result.is_err());
    }

    #[test]
    fn inject_png_xmp_rejects_non_png() {
        use super::inject_png_xmp;

        let result = inject_png_xmp(b"not a png", b"<xmp/>");
        assert!(result.is_err());
    }

    #[test]
    fn inject_jpeg_xmp_rejects_oversized_payload() {
        use super::inject_jpeg_xmp;

        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
        let mut jpeg_bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg_bytes, 80)
            .write_image(&image, 2, 2, ColorType::Rgb8.into())
            .expect("encode jpeg");

        // Create a payload that exceeds the 64KB APP segment limit
        let oversized = vec![0u8; 70_000];
        let result = inject_jpeg_xmp(&jpeg_bytes, &oversized);
        assert!(result.is_err());
    }

    #[test]
    fn transform_raster_round_trips_xmp_in_jpeg() {
        use crate::core::{MetadataKind, TransformWarning};
        let artifact = jpeg_with_xmp_iptc();
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                strip_metadata: false,
                format: Some(MediaType::Jpeg),
                ..TransformOptions::default()
            },
        ))
        .expect("keep-metadata transform");

        // XMP should be present in the output JPEG
        let xmp_ns = b"http://ns.adobe.com/xap/1.0/\0";
        assert!(
            result
                .artifact
                .bytes
                .windows(xmp_ns.len())
                .any(|w| w == xmp_ns),
            "XMP namespace should be present in output JPEG"
        );

        // No XMP/IPTC dropped warnings should be present
        assert!(
            !result
                .warnings
                .iter()
                .any(|w| matches!(w, TransformWarning::MetadataDropped(MetadataKind::Xmp))),
            "should not have XMP dropped warning"
        );
    }

    #[test]
    fn transform_raster_round_trips_xmp_in_png() {
        use crate::core::{MetadataKind, TransformWarning};
        let artifact = jpeg_with_xmp_iptc();
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                strip_metadata: false,
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("keep-metadata transform to PNG");

        // XMP should be present in the output PNG via iTXt chunk
        assert!(
            result
                .artifact
                .bytes
                .windows(b"XML:com.adobe.xmp".len())
                .any(|w| w == b"XML:com.adobe.xmp"),
            "XMP keyword should be present in output PNG"
        );

        // IPTC should be dropped (no PNG embedding) — warning present
        assert!(
            result
                .warnings
                .iter()
                .any(|w| matches!(w, TransformWarning::MetadataDropped(MetadataKind::Iptc))),
            "should have IPTC dropped warning for PNG output"
        );
    }

    #[test]
    fn transform_raster_can_convert_png_to_bmp() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Bmp),
                ..TransformOptions::default()
            },
        ))
        .expect("convert png to bmp");

        assert_eq!(result.artifact.media_type, MediaType::Bmp);
        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(3));
        // BMP output starts with "BM" signature
        assert_eq!(&result.artifact.bytes[0..2], b"BM");
    }

    #[test]
    fn transform_raster_can_convert_bmp_to_png() {
        // First create a BMP artifact from a PNG
        let png = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let bmp_result = transform_raster(TransformRequest::new(
            png,
            TransformOptions {
                format: Some(MediaType::Bmp),
                ..TransformOptions::default()
            },
        ))
        .expect("create bmp");

        // Now convert BMP back to PNG
        let bmp_artifact =
            crate::sniff_artifact(crate::RawArtifact::new(bmp_result.artifact.bytes, None))
                .expect("sniff bmp");
        assert_eq!(bmp_artifact.media_type, MediaType::Bmp);

        let result = transform_raster(TransformRequest::new(
            bmp_artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("convert bmp to png");

        assert_eq!(result.artifact.media_type, MediaType::Png);
        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(3));
    }

    #[test]
    fn transform_raster_can_resize_bmp() {
        let png = png_artifact(8, 4, Rgba([10, 20, 30, 255]));
        let bmp_result = transform_raster(TransformRequest::new(
            png,
            TransformOptions {
                format: Some(MediaType::Bmp),
                ..TransformOptions::default()
            },
        ))
        .expect("create bmp");

        let bmp_artifact =
            crate::sniff_artifact(crate::RawArtifact::new(bmp_result.artifact.bytes, None))
                .expect("sniff bmp");

        let result = transform_raster(TransformRequest::new(
            bmp_artifact,
            TransformOptions {
                width: Some(4),
                format: Some(MediaType::Bmp),
                ..TransformOptions::default()
            },
        ))
        .expect("resize bmp");

        assert_eq!(result.artifact.metadata.width, Some(4));
        assert_eq!(result.artifact.metadata.height, Some(2));
    }

    #[test]
    fn transform_raster_applies_blur() {
        // Use a non-uniform image so blur actually changes pixel values.
        let mut image = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255]));
        for y in 0..4 {
            for x in 0..4 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 8, 8, ColorType::Rgba8.into())
            .expect("encode png");
        let artifact = Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(8),
                height: Some(8),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                blur: Some(2.0),
                ..TransformOptions::default()
            },
        ))
        .expect("blur transform");

        assert_eq!(result.artifact.metadata.width, Some(8));
        assert_eq!(result.artifact.metadata.height, Some(8));

        // After blur, the sharp edge should be smoothed: a pixel near the
        // boundary is neither pure black nor pure white.
        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output");
        let edge_pixel = output.get_pixel(4, 4);
        assert!(
            edge_pixel[0] > 0 && edge_pixel[0] < 255,
            "expected blurred edge pixel to be a mid-tone, got r={}",
            edge_pixel[0]
        );
    }

    #[test]
    fn transform_raster_applies_sharpen() {
        // Create a blurry image by first blurring a sharp edge.
        let mut image = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255]));
        for y in 0..4 {
            for x in 0..4 {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
        }
        let blurred = DynamicImage::ImageRgba8(image).blur(2.0);

        // Measure pre-sharpen contrast across the edge.
        let pre_dark = blurred.get_pixel(3, 3)[0] as i32;
        let pre_light = blurred.get_pixel(4, 3)[0] as i32;
        let pre_contrast = (pre_light - pre_dark).abs();

        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(blurred.as_bytes(), 8, 8, ColorType::Rgba8.into())
            .expect("encode png");
        let artifact = Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(8),
                height: Some(8),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                sharpen: Some(5.0),
                ..TransformOptions::default()
            },
        ))
        .expect("sharpen transform");

        assert_eq!(result.artifact.metadata.width, Some(8));
        assert_eq!(result.artifact.metadata.height, Some(8));

        // After sharpening, the contrast across the edge should increase.
        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output");
        let post_dark = output.get_pixel(3, 3)[0] as i32;
        let post_light = output.get_pixel(4, 3)[0] as i32;
        let post_contrast = (post_light - post_dark).abs();

        assert!(
            post_contrast > pre_contrast,
            "expected sharpening to increase edge contrast: pre={pre_contrast}, post={post_contrast}"
        );
    }

    #[test]
    fn transform_raster_applies_watermark() {
        let main = png_artifact(10, 10, Rgba([255, 255, 255, 255]));
        let wm = png_artifact(3, 3, Rgba([0, 0, 0, 128]));

        let mut request = TransformRequest::new(main, TransformOptions::default());
        request.watermark = Some(WatermarkInput {
            image: wm,
            position: Position::BottomRight,
            opacity: 100,
            margin: 0,
        });

        let result = transform_raster(request).expect("watermark transform");
        assert_eq!(result.artifact.metadata.width, Some(10));
        assert_eq!(result.artifact.metadata.height, Some(10));

        // Verify watermark composited by checking a pixel in the watermark region.
        let output_image =
            image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
                .expect("decode output");
        // Bottom-right corner (9,9) should be affected by the black watermark.
        let pixel = output_image.get_pixel(9, 9);
        assert!(
            pixel[0] < 255,
            "expected watermark to darken the pixel, got r={}",
            pixel[0]
        );
    }

    #[test]
    fn transform_raster_rejects_oversized_watermark() {
        let main = png_artifact(4, 4, Rgba([255, 255, 255, 255]));
        let wm = png_artifact(5, 5, Rgba([0, 0, 0, 128]));

        let mut request = TransformRequest::new(main, TransformOptions::default());
        request.watermark = Some(WatermarkInput {
            image: wm,
            position: Position::Center,
            opacity: 50,
            margin: 0,
        });

        let err = transform_raster(request).expect_err("oversized watermark should fail");
        assert_eq!(
            err,
            TransformError::InvalidOptions(
                "watermark image is too large for the output dimensions".to_string()
            )
        );
    }

    #[test]
    fn watermark_full_width_at_top_with_margin_succeeds() {
        // A watermark as wide as the main image should be accepted at Top
        // because Top only applies margin on the Y axis.
        let main = png_artifact(10, 10, Rgba([255, 255, 255, 255]));
        let wm = png_artifact(10, 3, Rgba([0, 0, 0, 128]));

        let mut request = TransformRequest::new(main, TransformOptions::default());
        request.watermark = Some(WatermarkInput {
            image: wm,
            position: Position::Top,
            opacity: 50,
            margin: 2,
        });

        let result = transform_raster(request).expect("full-width watermark at Top should succeed");
        assert_eq!(result.artifact.metadata.width, Some(10));
    }

    #[test]
    fn watermark_full_height_at_left_with_margin_succeeds() {
        // A watermark as tall as the main image should be accepted at Left
        // because Left only applies margin on the X axis.
        let main = png_artifact(10, 10, Rgba([255, 255, 255, 255]));
        let wm = png_artifact(3, 10, Rgba([0, 0, 0, 128]));

        let mut request = TransformRequest::new(main, TransformOptions::default());
        request.watermark = Some(WatermarkInput {
            image: wm,
            position: Position::Left,
            opacity: 50,
            margin: 2,
        });

        let result =
            transform_raster(request).expect("full-height watermark at Left should succeed");
        assert_eq!(result.artifact.metadata.height, Some(10));
    }

    #[test]
    fn watermark_pixel_limit_enforced() {
        // Create a watermark artifact with fake dimensions exceeding MAX_DECODED_PIXELS.
        // We use a valid but tiny PNG, then override the metadata to claim huge dimensions.
        let main = png_artifact(4, 4, Rgba([255, 255, 255, 255]));
        let mut wm = png_artifact(2, 2, Rgba([0, 0, 0, 128]));
        // Override metadata to simulate a decompression bomb watermark.
        wm.metadata.width = Some(100_000);
        wm.metadata.height = Some(100_000);

        let mut request = TransformRequest::new(main, TransformOptions::default());
        request.watermark = Some(WatermarkInput {
            image: wm,
            position: Position::Center,
            opacity: 50,
            margin: 0,
        });

        let err = transform_raster(request).expect_err("huge watermark should be rejected");
        // The early metadata size check rejects the watermark before decode,
        // so we may get InvalidOptions (too large) instead of LimitExceeded.
        assert!(
            matches!(err, TransformError::InvalidOptions(ref msg) if msg.contains("too large"))
                || matches!(err, TransformError::LimitExceeded(ref msg) if msg.contains("pixels")),
            "expected InvalidOptions or LimitExceeded, got: {err}"
        );
    }

    #[test]
    fn transform_raster_applies_crop() {
        use crate::core::CropRegion;
        let artifact = png_artifact(4, 4, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                crop: Some(CropRegion {
                    x: 1,
                    y: 1,
                    width: 2,
                    height: 2,
                }),
                ..TransformOptions::default()
            },
        ))
        .expect("crop should succeed");

        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(2));
    }

    #[test]
    fn transform_raster_rejects_crop_exceeding_bounds() {
        use crate::core::CropRegion;
        let artifact = png_artifact(4, 4, Rgba([10, 20, 30, 255]));
        let err = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                crop: Some(CropRegion {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                }),
                ..TransformOptions::default()
            },
        ))
        .expect_err("crop exceeding bounds should fail");

        assert!(
            matches!(err, TransformError::InvalidOptions(ref msg) if msg.contains("exceeds image bounds")),
            "unexpected error: {err}"
        );
    }

    // ── Exhaustive EXIF orientation tests (issue #106) ──────────────────

    /// Orientation 1: no transform — dimensions remain unchanged.
    #[test]
    fn apply_exif_orientation_1_identity() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 1);
        assert_eq!(result.dimensions(), (4, 2));
    }

    /// Orientation 2: horizontal flip — dimensions remain unchanged.
    #[test]
    fn apply_exif_orientation_2_fliph() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 2);
        assert_eq!(result.dimensions(), (4, 2));
    }

    /// Orientation 3: rotate 180 — dimensions remain unchanged.
    #[test]
    fn apply_exif_orientation_3_rotate180() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 3);
        assert_eq!(result.dimensions(), (4, 2));
    }

    /// Orientation 4: vertical flip — dimensions remain unchanged.
    #[test]
    fn apply_exif_orientation_4_flipv() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 4);
        assert_eq!(result.dimensions(), (4, 2));
    }

    /// Orientation 5: transpose (fliph + rotate90) — dimensions are swapped.
    #[test]
    fn apply_exif_orientation_5_transpose() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 5);
        assert_eq!(result.dimensions(), (2, 4));
    }

    /// Orientation 6: rotate 90 CW — dimensions are swapped.
    #[test]
    fn apply_exif_orientation_6_rotate90() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 6);
        assert_eq!(result.dimensions(), (2, 4));
    }

    /// Orientation 7: transverse (fliph + rotate270) — dimensions are swapped.
    #[test]
    fn apply_exif_orientation_7_transverse() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 7);
        assert_eq!(result.dimensions(), (2, 4));
    }

    /// Orientation 8: rotate 270 CW — dimensions are swapped.
    #[test]
    fn apply_exif_orientation_8_rotate270() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 8);
        assert_eq!(result.dimensions(), (2, 4));
    }

    /// Invalid orientation value (0) should leave the image unchanged.
    #[test]
    fn apply_exif_orientation_0_passthrough() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 0);
        assert_eq!(result.dimensions(), (4, 2));
    }

    /// Out-of-range orientation value (9) should leave the image unchanged.
    #[test]
    fn apply_exif_orientation_9_passthrough() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
        let result = apply_exif_orientation(image, 9);
        assert_eq!(result.dimensions(), (4, 2));
    }

    /// Verify pixel placement for orientation 2 (horizontal flip).
    /// Place a distinct pixel at (0,0) and check it moves to (width-1, 0).
    #[test]
    fn apply_exif_orientation_2_pixel_placement() {
        let mut img = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let result = apply_exif_orientation(DynamicImage::ImageRgba8(img), 2);
        assert_eq!(*result.to_rgba8().get_pixel(3, 0), Rgba([255, 0, 0, 255]));
    }

    /// Verify pixel placement for orientation 3 (rotate 180).
    /// Pixel at (0,0) should move to (width-1, height-1).
    #[test]
    fn apply_exif_orientation_3_pixel_placement() {
        let mut img = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let result = apply_exif_orientation(DynamicImage::ImageRgba8(img), 3);
        assert_eq!(*result.to_rgba8().get_pixel(3, 1), Rgba([255, 0, 0, 255]));
    }

    /// Verify pixel placement for orientation 4 (vertical flip).
    /// Pixel at (0,0) should move to (0, height-1).
    #[test]
    fn apply_exif_orientation_4_pixel_placement() {
        let mut img = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let result = apply_exif_orientation(DynamicImage::ImageRgba8(img), 4);
        assert_eq!(*result.to_rgba8().get_pixel(0, 1), Rgba([255, 0, 0, 255]));
    }

    /// Verify pixel placement for orientation 6 (rotate 90 CW).
    /// Pixel at (0,0) should move to (height-1, 0) in the rotated image.
    #[test]
    fn apply_exif_orientation_6_pixel_placement() {
        let mut img = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let result = apply_exif_orientation(DynamicImage::ImageRgba8(img), 6);
        let rgba = result.to_rgba8();
        assert_eq!(rgba.dimensions(), (2, 4));
        assert_eq!(*rgba.get_pixel(1, 0), Rgba([255, 0, 0, 255]));
    }

    /// Verify pixel placement for orientation 8 (rotate 270 CW).
    /// Pixel at (0,0) should move to (0, width-1) in the rotated image.
    #[test]
    fn apply_exif_orientation_8_pixel_placement() {
        let mut img = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let result = apply_exif_orientation(DynamicImage::ImageRgba8(img), 8);
        let rgba = result.to_rgba8();
        assert_eq!(rgba.dimensions(), (2, 4));
        assert_eq!(*rgba.get_pixel(0, 3), Rgba([255, 0, 0, 255]));
    }

    /// End-to-end test: JPEG with each EXIF orientation value is auto-corrected
    /// during transform. Orientations 5-8 swap dimensions.
    #[test]
    fn transform_raster_auto_orients_all_jpeg_orientations() {
        for orientation in 1..=8u16 {
            let artifact = jpeg_artifact_with_metadata(4, 2, Some(orientation), None);
            let result = transform_raster(TransformRequest::new(
                artifact,
                TransformOptions {
                    format: Some(MediaType::Jpeg),
                    ..TransformOptions::default()
                },
            ))
            .unwrap_or_else(|e| panic!("orientation {orientation} should succeed: {e}"));

            let (expected_w, expected_h) = if orientation >= 5 {
                (2, 4)
            } else {
                (4, 2)
            };
            assert_eq!(
                result.artifact.metadata.width,
                Some(expected_w),
                "orientation {orientation}: expected width {expected_w}"
            );
            assert_eq!(
                result.artifact.metadata.height,
                Some(expected_h),
                "orientation {orientation}: expected height {expected_h}"
            );
        }
    }
}
