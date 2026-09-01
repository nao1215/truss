use crate::core::{
    Artifact, ArtifactMetadata, CropRegion, Fit, MAX_DECODED_PIXELS, MAX_OUTPUT_PIXELS, MediaType,
    MetadataKind, MetadataPolicy, NormalizedTransformOptions, NormalizedTransformRequest,
    OptimizeMode, Position, QualityMetric, Rotation, TargetQuality, TransformError,
    TransformRequest, TransformResult, TransformWarning, WatermarkInput,
    default_lossy_target_quality,
};
use crate::{RawArtifact, Rgba8, sniff_artifact};
#[cfg(feature = "avif")]
use image::codecs::avif::AvifEncoder;
use image::codecs::jpeg::JpegDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{
    CompressionType as PngCompressionType, FilterType as PngFilterType, PngDecoder, PngEncoder,
};
use image::codecs::webp::WebPDecoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::{self, FilterType};
use image::metadata::Orientation;
use image::{
    ColorType, DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat, Pixel,
    Rgba, RgbaImage,
};
#[cfg(feature = "avif")]
use mp4parse::ParseStrictness;
#[cfg(feature = "avif")]
use rav1d_safe::{Decoder, Planes};
use std::io::Cursor;
use std::time::{Duration, Instant};
#[cfg(feature = "avif")]
use yuvutils_rs::{YuvGrayImage, YuvPlanarImage, YuvRange, YuvStandardMatrix};

/// Transforms a raster artifact using the current backend implementation.
///
/// The input artifact must already be classified by [`crate::sniff_artifact`]. This backend
/// performs raster-only work for the current implementation phase: optional EXIF auto-orient
/// for JPEG input, explicit rotation, resize handling, and encoding into the requested output
/// format. Metadata stripping remains the default, while `preserve_exif` retains EXIF and
/// `keep-metadata` retains EXIF plus ICC profiles for JPEG, PNG, and WebP output. XMP is carried
/// for JPEG, PNG, and WebP; IPTC only for JPEG. Metadata the target format cannot round-trip is
/// silently dropped and reported as [`TransformWarning::MetadataDropped`] warnings in the
/// returned [`TransformResult`].
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
#[must_use = "this function returns the transform result without side effects"]
pub fn transform_raster(request: TransformRequest) -> Result<TransformResult, TransformError> {
    let normalized = request.normalize()?;
    if let Some(result) = try_passthrough_lossless_optimization(&normalized)? {
        return Ok(result);
    }
    let deadline = normalized.options.deadline;
    let budget = EncodeDeadline {
        start: deadline.map(|_| Instant::now()),
        deadline,
    };

    let (retained_metadata, mut warnings) = extract_retained_metadata(
        &normalized.input,
        normalized.options.metadata_policy,
        normalized.options.auto_orient,
        normalized.options.format,
    )?;

    check_input_pixel_limit(&normalized.input)?;

    let mut image = decode_input(&normalized.input)?;
    budget.check("decode")?;

    image = apply_pixel_stages(image, &normalized, budget)?;

    // Formats without an alpha channel need the transparency resolved before the encoder
    // sees it, or it is truncated away rather than composited.
    image = flatten_for_opaque_output(
        image,
        normalized.options.background,
        normalized.options.format,
    );

    // Kept apart from `warnings` until the passthrough below has decided: a warning about
    // the encode is void when the encode is not what gets returned.
    let mut encode_warnings = Vec::new();
    let encoded = encode_output(
        &image,
        normalized.options.format,
        &normalized.options,
        retained_metadata.as_ref(),
        budget,
        &mut encode_warnings,
    )?;
    budget.check("encode")?;

    // Post-encode byte-level injection for metadata the encoders cannot embed themselves:
    // XMP/IPTC for JPEG and PNG, and the whole WebP metadata set for lossy output, which
    // libwebp writes without any container chunks.
    let bytes = if let Some(ref metadata) = retained_metadata {
        inject_metadata(
            encoded.bytes,
            normalized.options.format,
            metadata,
            encoded.used_lossy_webp,
            &mut warnings,
        )
    } else {
        encoded.bytes
    };

    // `auto` and `lossless` both mean "no bigger than what you were handed". The input's
    // own bytes are a candidate whenever no pixel transform was asked for, and for an image
    // the encoder compresses worse than whatever produced the input they are the smallest.
    let bytes = match smaller_passthrough(&normalized, &bytes) {
        Some(input) => input,
        None => {
            warnings.append(&mut encode_warnings);
            bytes
        }
    };

    let (width, height) = image.dimensions();
    // Read the tag back out of the encoded bytes rather than predicting it: with
    // auto-orientation off and metadata retained, the output carries the input's tag, and
    // this is what `inspect` reports for the same file. Asked of the output's own format,
    // because every container that can carry the tag can carry it into the output too.
    let orientation = crate::core::exif_orientation(normalized.options.format, &bytes);
    if let Some(warning) = dropped_orientation_warning(
        &normalized.input,
        normalized.options.auto_orient,
        orientation,
    ) {
        warnings.push(warning);
    }

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
                orientation,
            },
        ),
        warnings,
    })
}

/// Runs the stages that touch pixels, in the order `docs/pipeline.md` fixes: auto-orient,
/// rotate, crop, resize, blur, sharpen, grayscale, watermark.
///
/// The order is the contract, not the order the caller wrote the options in. Each stage
/// works on the one before it, and each checks the deadline after it, so a pipeline that has
/// spent its budget stops at the next boundary rather than carrying on. A stage already under
/// way is not interrupted, so the transform returns after the deadline by however long that
/// stage takes. The output pixel limit is checked before the resize rather than after, from
/// the dimensions alone, so an outsized request is refused without allocating the buffer it
/// asked for.
fn apply_pixel_stages(
    mut image: DynamicImage,
    normalized: &NormalizedTransformRequest,
    budget: EncodeDeadline,
) -> Result<DynamicImage, TransformError> {
    let options = &normalized.options;

    if options.auto_orient {
        image = apply_auto_orientation(image, &normalized.input);
    }

    image = apply_rotation(image, options.rotate, options.background, options.format)?;
    budget.check("rotate")?;

    if let Some(crop) = options.crop {
        image = apply_crop(image, crop)?;
        budget.check("crop")?;
    }

    check_output_pixel_limit(
        &image,
        options.width,
        options.height,
        options.fit,
        options.without_enlargement,
    )?;
    image = apply_resize(
        image,
        options.width,
        options.height,
        options.fit,
        options.position,
        options.background,
        options.format,
        options.without_enlargement,
    );
    budget.check("resize")?;

    if let Some(sigma) = options.blur {
        image = image.blur(sigma);
        budget.check("blur")?;
    }

    if let Some(sigma) = options.sharpen {
        image = image.unsharpen(sigma, 1);
        budget.check("sharpen")?;
    }

    if options.grayscale {
        image = apply_grayscale(image);
        budget.check("grayscale")?;
    }

    if let Some(ref watermark) = normalized.watermark {
        image = apply_watermark(image, watermark)?;
        budget.check("watermark")?;
    }

    Ok(image)
}

fn decode_input(input: &Artifact) -> Result<DynamicImage, TransformError> {
    let image_format = match input.media_type {
        MediaType::Jpeg => ImageFormat::Jpeg,
        MediaType::Png => ImageFormat::Png,
        MediaType::Webp => ImageFormat::WebP,
        MediaType::Avif => {
            #[cfg(feature = "avif")]
            {
                return decode_avif(&input.bytes);
            }
            #[cfg(not(feature = "avif"))]
            {
                return Err(TransformError::CapabilityMissing(
                    "AVIF decoding is not enabled in this build".to_string(),
                ));
            }
        }
        MediaType::Bmp => ImageFormat::Bmp,
        MediaType::Tiff => ImageFormat::Tiff,
        MediaType::Gif => ImageFormat::Gif,
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
#[cfg(feature = "avif")]
fn decode_avif(bytes: &[u8]) -> Result<DynamicImage, TransformError> {
    let aperture = crate::core::avif_clean_aperture(bytes)?;
    let mut cursor = Cursor::new(bytes);
    let parse = |cursor: &mut Cursor<&[u8]>, strictness| {
        cursor.set_position(0);
        mp4parse::read_avif(cursor, strictness)
            .map_err(|e| TransformError::DecodeFailed(format!("AVIF container parse failed: {e}")))
    };
    let mut context = parse(&mut cursor, ParseStrictness::Normal)?;
    // mp4parse does not read `clap` and, since MIAF marks it essential, forbids the item
    // that carries one. The aperture is read and applied here, so for such a file the parse
    // is repeated without that check; every other file keeps the stricter parse.
    if aperture.is_some() && !context.primary_item_is_present() {
        context = parse(&mut cursor, ParseStrictness::Permissive)?;
    }

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

    let image = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| TransformError::DecodeFailed("AVIF decoded buffer size mismatch".into()))?;

    // The clean aperture comes first among the transformative properties, so it is cut
    // here, before the orientation the pipeline applies to what this returns.
    let image = match aperture {
        Some(aperture) => {
            let (x, y, aperture_width, aperture_height) = aperture.rectangle(width, height)?;
            if (x, y, aperture_width, aperture_height) == (0, 0, width, height) {
                image
            } else {
                image::imageops::crop_imm(&image, x, y, aperture_width, aperture_height).to_image()
            }
        }
        None => image,
    };

    Ok(DynamicImage::ImageRgba8(image))
}

/// Feeds AV1 OBU data to a `rav1d` decoder and returns the first decoded frame.
#[cfg(feature = "avif")]
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
#[cfg(feature = "avif")]
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
#[cfg(feature = "avif")]
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
#[cfg(feature = "avif")]
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
            let shift = frame.bit_depth() - 8;
            let y8: Vec<u8> = planes
                .y()
                .as_slice()
                .iter()
                .map(|&v| narrow_sample(v, shift))
                .collect();
            let y_stride = planes.y().stride();
            let u8s: Option<(Vec<u8>, usize)> = planes.u().as_ref().map(|p| {
                let data: Vec<u8> = p
                    .as_slice()
                    .iter()
                    .map(|&v| narrow_sample(v, shift))
                    .collect();
                (data, p.stride())
            });
            let v8s: Option<(Vec<u8>, usize)> = planes.v().as_ref().map(|p| {
                let data: Vec<u8> = p
                    .as_slice()
                    .iter()
                    .map(|&v| narrow_sample(v, shift))
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

/// Narrows a 10- or 12-bit sample to 8 bits, rounding to nearest.
///
/// `shift` is the depth minus eight. The rounding is clamped: a sample at the top of its
/// range rounds past 255 otherwise, and a cast would wrap it to 0, which turned white into
/// black and a saturated primary into green in every deep AVIF.
#[cfg(feature = "avif")]
fn narrow_sample(value: u16, shift: u8) -> u8 {
    let round = 1u16 << shift.saturating_sub(1);
    u8::try_from((u32::from(value) + u32::from(round)) >> shift).unwrap_or(u8::MAX)
}

/// Converts 8-bit YUV plane data to RGBA, dispatching by pixel layout.
///
/// U and V planes are `None` for I400 (grayscale). For I420/I422/I444, both must be present.
#[cfg(feature = "avif")]
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
#[cfg(feature = "avif")]
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
                        rgba[idx] = narrow_sample(alpha, shift);
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

/// Resolves the dimensions `apply_resize` will produce for the given request.
///
/// When only one axis is requested the other is derived from the source aspect ratio, exactly
/// as `apply_resize` does, so callers see the real output size rather than the source size.
/// The uniform scale a fit mode applies to the source before any cropping or padding.
///
/// `contain` and `inside` shrink until both axes are within the box, so they take the
/// smaller ratio. `cover` has to overflow the box on one axis to fill it on the other, so
/// it takes the larger. `fill` scales the axes independently and has no single factor,
/// which is why it never reaches here.
fn fit_scale(source: (u32, u32), target: (u32, u32), fit: Fit) -> f64 {
    let (source_w, source_h) = (f64::from(source.0), f64::from(source.1));
    let (target_w, target_h) = (f64::from(target.0), f64::from(target.1));
    match fit {
        Fit::Contain | Fit::Inside => f64::min(target_w / source_w, target_h / source_h),
        Fit::Cover => f64::max(target_w / source_w, target_h / source_h),
        Fit::Fill => 1.0,
    }
}

/// Applies a scale factor to both axes, never producing a zero dimension.
fn scale_both(source: (u32, u32), scale: f64) -> (u32, u32) {
    let scaled_w = (f64::from(source.0) * scale).round().max(1.0);
    let scaled_h = (f64::from(source.1) * scale).round().max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (scaled_w as u32, scaled_h as u32)
}

/// The size the source is scaled to for a fit mode, before padding or cropping.
///
/// `without_enlargement` clamps the scale at 1.0 rather than being folded into any one fit
/// mode: whether a request may upscale is a separate question from how it fits the box, and
/// every mode has a meaningful answer to it.
fn fit_content_size(
    source: (u32, u32),
    target: (u32, u32),
    fit: Fit,
    without_enlargement: bool,
) -> (u32, u32) {
    if fit == Fit::Fill {
        // Fill stretches each axis on its own, so the clamp applies per axis.
        return if without_enlargement {
            (target.0.min(source.0), target.1.min(source.1))
        } else {
            target
        };
    }

    let scale = fit_scale(source, target, fit);
    let scale = if without_enlargement {
        scale.min(1.0)
    } else {
        scale
    };
    scale_both(source, scale)
}

/// The size the content is scaled to, before any padding or cropping the fit mode adds.
///
/// This is the buffer a resize materializes, which is not the size it returns: `contain`
/// pads the content out to the box afterwards and `cover` crops it back to the box, so for
/// those two the content and the output differ. [`resolved_output_dimensions`] answers the
/// other half. The SVG codec rasterizes at this size so a vector source is drawn once, at
/// one uniform scale, and then goes through the same padding and cropping as a raster one.
pub(crate) fn resize_content_size(
    source: (u32, u32),
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    without_enlargement: bool,
) -> (u32, u32) {
    match (width, height) {
        (None, None) => source,
        // A single axis is a scale request, so the content is the whole output.
        (Some(_), None) | (None, Some(_)) => {
            resolved_output_dimensions(source, width, height, fit, without_enlargement)
        }
        (Some(target_width), Some(target_height)) => fit_content_size(
            source,
            (target_width, target_height),
            fit.unwrap_or(Fit::Contain),
            without_enlargement,
        ),
    }
}

/// The final output size of a resize request, canvas included.
///
/// This is what [`check_output_pixel_limit`] measures and what [`apply_resize`] produces, so
/// the limit is always applied to the size that actually gets allocated. `contain` reports
/// the requested box because it pads out to it; `inside` reports the scaled content, which is
/// the whole difference between the two modes.
pub(crate) fn resolved_output_dimensions(
    current: (u32, u32),
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    without_enlargement: bool,
) -> (u32, u32) {
    let (current_w, current_h) = current;
    match (width, height) {
        (None, None) => current,
        // A single axis is a scale request: the other axis follows from the aspect ratio.
        (Some(target_width), None) => {
            let target_width = if without_enlargement {
                target_width.min(current_w)
            } else {
                target_width
            };
            (
                target_width,
                scale_dimension(current_h, target_width, current_w),
            )
        }
        (None, Some(target_height)) => {
            let target_height = if without_enlargement {
                target_height.min(current_h)
            } else {
                target_height
            };
            (
                scale_dimension(current_w, target_height, current_h),
                target_height,
            )
        }
        (Some(target_width), Some(target_height)) => {
            let target = (target_width, target_height);
            let fit = fit.unwrap_or(Fit::Contain);
            let content = fit_content_size(current, target, fit, without_enlargement);
            match fit {
                // Contain pads out to the requested box, so that is the output size.
                Fit::Contain => target,
                // Inside returns the scaled content itself, with no padding.
                Fit::Inside => content,
                // Fill's content size is already the target, clamped when asked.
                Fit::Fill => content,
                // Cover crops back to the box. It can only fall short of it when
                // `without_enlargement` stopped the scale from reaching it.
                Fit::Cover => (target.0.min(content.0), target.1.min(content.1)),
            }
        }
    }
}

/// Checks the output dimensions against [`MAX_OUTPUT_PIXELS`] before resize allocation.
///
/// Computes the effective output pixel count from the requested dimensions and the current
/// image size, deriving the omitted axis from the aspect ratio the same way `apply_resize`
/// does. The check runs before `apply_resize` so that oversized output buffers are never
/// allocated.
fn check_output_pixel_limit(
    image: &DynamicImage,
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    without_enlargement: bool,
) -> Result<(), TransformError> {
    let source = image.dimensions();

    // `cover` scales the source until it covers the box on both axes and crops afterwards,
    // so the buffer it materializes is larger than the size it returns, without bound as the
    // two aspect ratios diverge. Check that buffer here, from dimensions alone, the way
    // `check_rotated_pixel_limit` does: nothing between this point and the allocation looks
    // at it, and the allocation is what aborts the process.
    if let (Some(target_w), Some(target_h)) = (width, height)
        && fit.unwrap_or(Fit::Contain) == Fit::Cover
    {
        let (content_w, content_h) = fit_content_size(
            source,
            (target_w, target_h),
            Fit::Cover,
            without_enlargement,
        );
        let pixels = u64::from(content_w) * u64::from(content_h);
        if pixels > MAX_OUTPUT_PIXELS {
            return Err(TransformError::LimitExceeded(format!(
                "fit=cover scales {}x{} to {content_w}x{content_h} ({pixels} pixels) before cropping to {target_w}x{target_h}, limit is {MAX_OUTPUT_PIXELS}",
                source.0, source.1
            )));
        }
    }

    let (out_w, out_h) =
        resolved_output_dimensions(source, width, height, fit, without_enlargement);
    let pixels = u64::from(out_w) * u64::from(out_h);
    if pixels > MAX_OUTPUT_PIXELS {
        return Err(TransformError::LimitExceeded(format!(
            "output image would have {pixels} pixels, limit is {MAX_OUTPUT_PIXELS}"
        )));
    }
    Ok(())
}

/// Applies the input's EXIF orientation, reading the tag the way `inspect` reports it.
///
/// Both go through [`crate::core::exif_orientation`], so what `inspect` says a file is
/// tagged with and what `convert` does about it cannot disagree, in any of the containers
/// that can carry the tag.
fn apply_auto_orientation(image: DynamicImage, input: &Artifact) -> DynamicImage {
    match crate::core::exif_orientation(input.media_type, &input.bytes) {
        Some(orientation) => apply_exif_orientation(image, orientation),
        None => image,
    }
}

fn apply_exif_orientation(image: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.fliph().rotate270(),
        6 => image.rotate90(),
        7 => image.fliph().rotate90(),
        8 => image.rotate270(),
        _ => image,
    }
}

/// Rotates an image clockwise by any whole number of degrees.
///
/// A multiple of 90 keeps the exact path: those only permute pixels, so resampling them
/// would lose sharpness for nothing. Any other angle goes through [`rotate_arbitrary`].
///
/// The SVG codec calls this too, once it has rasterized: a rotated SVG needs the same
/// quarter-turn shortcut, bounding box, background rule, and pixel limit as any other
/// raster input, and one function is what keeps the two from drifting.
///
/// # Errors
///
/// Returns [`TransformError::LimitExceeded`] when the rotated bounding box would exceed
/// [`MAX_OUTPUT_PIXELS`].
pub(crate) fn apply_rotation(
    image: DynamicImage,
    rotation: Rotation,
    background: Option<Rgba8>,
    output_format: MediaType,
) -> Result<DynamicImage, TransformError> {
    match rotation.quarter_turns() {
        Some(0) => Ok(image),
        Some(1) => Ok(image.rotate90()),
        Some(2) => Ok(image.rotate180()),
        Some(3) => Ok(image.rotate270()),
        _ => rotate_arbitrary(image, rotation, background, output_format),
    }
}

/// The output size that holds a rotated image whole.
///
/// The corners of the source sweep out a larger axis-aligned box, so the canvas grows: a
/// 45-degree turn of a square needs about 1.41x each side. Cropping back to the original
/// size instead would silently cut the corners off, so the box expands and the exposed
/// area is filled with the background color.
pub(crate) fn rotated_bounding_box(width: u32, height: u32, degrees: u16) -> (u32, u32) {
    // Quarter turns are exact, and must be computed exactly: `cos(90f64.to_radians())` is
    // 6.1e-17 rather than 0, so the general formula rounds 8.0 up to 9 and reports a canvas
    // one pixel too large.
    match degrees {
        0 | 180 => return (width, height),
        90 | 270 => return (height, width),
        _ => {}
    }

    let radians = f64::from(degrees).to_radians();
    let (sin, cos) = radians.sin_cos();
    let (w, h) = (f64::from(width), f64::from(height));
    let out_w = (w * cos.abs() + h * sin.abs()).ceil().max(1.0);
    let out_h = (w * sin.abs() + h * cos.abs()).ceil().max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    (out_w as u32, out_h as u32)
}

/// Checks the rotated canvas against [`MAX_OUTPUT_PIXELS`], returning its size.
///
/// A 45-degree turn roughly doubles the pixel count, and the input budget
/// ([`MAX_DECODED_PIXELS`]) is larger than the output one, so a legal input can rotate into
/// an illegal output. This runs on dimensions alone, before the canvas is allocated.
///
/// # Errors
///
/// Returns [`TransformError::LimitExceeded`] when the rotated canvas would be too large.
fn check_rotated_pixel_limit(
    width: u32,
    height: u32,
    degrees: u16,
) -> Result<(u32, u32), TransformError> {
    let (out_w, out_h) = rotated_bounding_box(width, height, degrees);
    let pixels = u64::from(out_w) * u64::from(out_h);
    if pixels > MAX_OUTPUT_PIXELS {
        return Err(TransformError::LimitExceeded(format!(
            "rotating {width}x{height} by {degrees} degrees needs a {out_w}x{out_h} canvas ({pixels} pixels), limit is {MAX_OUTPUT_PIXELS}"
        )));
    }
    Ok((out_w, out_h))
}

/// Rotates by an angle that is not a quarter turn, sampling with bilinear interpolation.
///
/// Works backwards from the destination: every output pixel is mapped through the inverse
/// rotation to a source coordinate and sampled there, which is what leaves no unwritten
/// gaps between pixels the way a forward mapping would.
///
/// Sampling happens in premultiplied alpha. Interpolating straight RGBA next to a
/// transparent pixel pulls that pixel's meaningless color into the result, which shows up
/// as a dark or white fringe along the rotated edge. Off-image samples read as the
/// background color, so the boundary anti-aliases into the fill instead of against it.
fn rotate_arbitrary(
    image: DynamicImage,
    rotation: Rotation,
    background: Option<Rgba8>,
    output_format: MediaType,
) -> Result<DynamicImage, TransformError> {
    let source = image.to_rgba8();
    let (src_w, src_h) = source.dimensions();
    let degrees = rotation.as_degrees();
    let (out_w, out_h) = check_rotated_pixel_limit(src_w, src_h, degrees)?;

    let fill = background_pixel(background, output_format);
    let fill_premultiplied = premultiply(fill);

    let radians = f64::from(degrees).to_radians();
    let (sin, cos) = radians.sin_cos();
    let (src_cx, src_cy) = (f64::from(src_w) / 2.0, f64::from(src_h) / 2.0);
    let (out_cx, out_cy) = (f64::from(out_w) / 2.0, f64::from(out_h) / 2.0);

    let mut canvas = RgbaImage::from_pixel(out_w, out_h, fill);
    for y in 0..out_h {
        let dy = f64::from(y) + 0.5 - out_cy;
        for x in 0..out_w {
            let dx = f64::from(x) + 0.5 - out_cx;
            // Inverse of a clockwise rotation in image coordinates, where y grows downward.
            let sx = dx.mul_add(cos, dy * sin) + src_cx - 0.5;
            let sy = dx.mul_add(-sin, dy * cos) + src_cy - 0.5;
            canvas.put_pixel(x, y, sample_bilinear(&source, sx, sy, fill_premultiplied));
        }
    }

    Ok(DynamicImage::ImageRgba8(canvas))
}

/// Converts a pixel to premultiplied alpha as `f64` channels.
fn premultiply(pixel: Rgba<u8>) -> [f64; 4] {
    let alpha = f64::from(pixel[3]) / 255.0;
    [
        f64::from(pixel[0]) * alpha,
        f64::from(pixel[1]) * alpha,
        f64::from(pixel[2]) * alpha,
        f64::from(pixel[3]),
    ]
}

/// Samples a source image at fractional coordinates, blending the four nearest pixels.
///
/// Coordinates outside the image read as `outside`, which is the background in
/// premultiplied form. Sampling and blending both happen premultiplied; the result is
/// divided back out so the caller gets straight alpha again.
fn sample_bilinear(source: &RgbaImage, x: f64, y: f64, outside: [f64; 4]) -> Rgba<u8> {
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;

    #[allow(clippy::cast_possible_truncation)]
    let (x0, y0) = (x0 as i64, y0 as i64);

    let at = |px: i64, py: i64| -> [f64; 4] {
        if px < 0 || py < 0 {
            return outside;
        }
        #[allow(clippy::cast_sign_loss)]
        let (px, py) = (px as u32, py as u32);
        if px >= source.width() || py >= source.height() {
            return outside;
        }
        premultiply(*source.get_pixel(px, py))
    };

    let top_left = at(x0, y0);
    let top_right = at(x0 + 1, y0);
    let bottom_left = at(x0, y0 + 1);
    let bottom_right = at(x0 + 1, y0 + 1);

    let mut blended = [0.0_f64; 4];
    for channel in 0..4 {
        let top = top_left[channel] * (1.0 - fx) + top_right[channel] * fx;
        let bottom = bottom_left[channel] * (1.0 - fx) + bottom_right[channel] * fx;
        blended[channel] = top * (1.0 - fy) + bottom * fy;
    }

    let alpha = blended[3];
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let to_u8 = |value: f64| value.round().clamp(0.0, 255.0) as u8;

    if alpha <= 0.0 {
        return Rgba([0, 0, 0, 0]);
    }
    let scale = 255.0 / alpha;
    Rgba([
        to_u8(blended[0] * scale),
        to_u8(blended[1] * scale),
        to_u8(blended[2] * scale),
        to_u8(alpha),
    ])
}

/// Desaturates an image to grayscale while preserving its alpha channel.
///
/// Luminance uses the Rec. 601 weights applied by [`DynamicImage::grayscale`], so an
/// opaque input becomes `Luma8` and an input with alpha becomes `LumaA8`. The encoder
/// layer widens either back to `Rgb8`/`Rgba8`, so callers do not need to care which of
/// the two comes out here.
fn apply_grayscale(image: DynamicImage) -> DynamicImage {
    image.grayscale()
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

/// Resizes according to the fit mode, the enlargement policy, and the requested box.
///
/// The argument list is long because a resize genuinely depends on all of it, and bundling
/// the parameters into a struct would only move the same fields somewhere else while adding
/// a type that nothing outside this function would use.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_resize(
    image: DynamicImage,
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    position: Position,
    background: Option<Rgba8>,
    output_format: MediaType,
    without_enlargement: bool,
) -> DynamicImage {
    let source = image.dimensions();

    match (width, height) {
        (None, None) => image,
        // Single-axis resize derives the other axis from the aspect ratio. The same helper
        // backs `check_output_pixel_limit`, so the limit is applied to the real output size.
        (Some(_), None) | (None, Some(_)) => {
            let (target_width, target_height) =
                resolved_output_dimensions(source, width, height, fit, without_enlargement);
            if (target_width, target_height) == source {
                return image;
            }
            image.resize_exact(target_width, target_height, FilterType::Lanczos3)
        }
        (Some(target_width), Some(target_height)) => {
            let fit = fit.unwrap_or(Fit::Contain);
            let (content_width, content_height) =
                resize_content_size(source, width, height, Some(fit), without_enlargement);

            match fit {
                Fit::Fill | Fit::Inside => {
                    // Both write the content size straight out. Fill got there by scaling the
                    // axes independently, Inside by one shared factor, and neither pads.
                    if (content_width, content_height) == source {
                        return image;
                    }
                    image.resize_exact(content_width, content_height, FilterType::Lanczos3)
                }
                Fit::Contain => {
                    let resized = if (content_width, content_height) == source {
                        image
                    } else {
                        image.resize_exact(content_width, content_height, FilterType::Lanczos3)
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
                    content_width,
                    content_height,
                    target_width,
                    target_height,
                    position,
                    background,
                    output_format,
                ),
            }
        }
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
        check_watermark_fits(meta_w, meta_h, main_w, main_h, watermark)?;
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

    check_watermark_fits(wm_w, wm_h, main_w, main_h, watermark)?;

    let (x, y) = watermark_offset(main_w, main_h, wm_w, wm_h, watermark.position, margin);

    let mut canvas = image.to_rgba8();
    imageops::overlay(&mut canvas, &wm_rgba, i64::from(x), i64::from(y));

    Ok(DynamicImage::ImageRgba8(canvas))
}

/// Returns the margin the given position actually consumes on each axis.
///
/// A centered watermark is not pushed away from any edge, and an edge-centered one is
/// only pushed away from the edge it sits on.
fn watermark_margins(position: Position, margin: u32) -> (u32, u32) {
    match position {
        Position::Center => (0, 0),
        Position::Top | Position::Bottom => (0, margin),
        Position::Left | Position::Right => (margin, 0),
        // Corners: TopLeft, TopRight, BottomLeft, BottomRight.
        _ => (margin, margin),
    }
}

/// Rejects a watermark that cannot be placed inside the output at the requested margin.
///
/// The message reports both sizes and the margin because either one can be what does not
/// fit. Saying only "watermark image is too large" sent readers off to shrink an image
/// that was fine, when a margin wider than the output was the real cause. u64 arithmetic
/// keeps a large margin from wrapping a u32 into a size that appears to fit.
fn check_watermark_fits(
    wm_w: u32,
    wm_h: u32,
    main_w: u32,
    main_h: u32,
    watermark: &WatermarkInput,
) -> Result<(), TransformError> {
    let (margin_x, margin_y) = watermark_margins(watermark.position, watermark.margin);
    if u64::from(wm_w) + u64::from(margin_x) <= u64::from(main_w)
        && u64::from(wm_h) + u64::from(margin_y) <= u64::from(main_h)
    {
        return Ok(());
    }
    Err(TransformError::InvalidOptions(format!(
        "watermark {wm_w}x{wm_h} with a {}px margin does not fit a {main_w}x{main_h} output",
        watermark.margin
    )))
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

/// Scales to `resized_*` and crops back to the box, anchored by `position`.
///
/// The crop box is clamped to the scaled image. Normally the scaled image covers the target
/// on both axes and the clamp is a no-op; it only bites when `without_enlargement` stopped
/// the scale from reaching the box, in which case the output is legitimately smaller than
/// requested rather than padded out to it.
#[allow(clippy::too_many_arguments)]
fn cover_to_box(
    image: DynamicImage,
    resized_width: u32,
    resized_height: u32,
    target_width: u32,
    target_height: u32,
    position: Position,
    background: Option<Rgba8>,
    output_format: MediaType,
) -> DynamicImage {
    let resized = image
        .resize_exact(resized_width, resized_height, FilterType::Lanczos3)
        .to_rgba8();

    let crop_width = target_width.min(resized_width);
    let crop_height = target_height.min(resized_height);

    if resized_width == crop_width && resized_height == crop_height {
        return DynamicImage::ImageRgba8(resized);
    }

    let fill = background_pixel(background, output_format);
    let mut canvas = RgbaImage::from_pixel(crop_width, crop_height, fill);
    let (crop_x, crop_y) = position_offset(
        resized_width,
        resized_height,
        crop_width,
        crop_height,
        position,
    );
    let cropped = imageops::crop_imm(&resized, crop_x, crop_y, crop_width, crop_height).to_image();

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

fn try_passthrough_lossless_optimization(
    normalized: &NormalizedTransformRequest,
) -> Result<Option<TransformResult>, TransformError> {
    if normalized.options.optimize != OptimizeMode::Lossless {
        return Ok(None);
    }

    match normalized.options.format {
        MediaType::Jpeg => {
            if !is_passthrough_lossless_request(normalized) {
                return Err(TransformError::CapabilityMissing(lossless_jpeg_refusal(
                    normalized,
                )));
            }

            let bytes = optimize_jpeg_bytes_losslessly(
                &normalized.input.bytes,
                normalized.options.metadata_policy,
            )?;

            // Read back rather than copied from the input: the policy may have dropped the
            // segment that carried it, and this is what `inspect` reports for the output.
            let orientation = crate::core::exif_orientation(MediaType::Jpeg, &bytes);
            let mut warnings = Vec::new();
            if let Some(warning) = dropped_orientation_warning(
                &normalized.input,
                normalized.options.auto_orient,
                orientation,
            ) {
                warnings.push(warning);
            }

            Ok(Some(TransformResult {
                artifact: Artifact::new(
                    bytes,
                    normalized.options.format,
                    ArtifactMetadata {
                        orientation,
                        ..normalized.input.metadata.clone()
                    },
                ),
                warnings,
            }))
        }
        MediaType::Avif => Err(TransformError::CapabilityMissing(
            "lossless optimization is not implemented for avif output".to_string(),
        )),
        _ => Ok(None),
    }
}

fn is_passthrough_lossless_request(normalized: &NormalizedTransformRequest) -> bool {
    normalized.input.media_type == normalized.options.format
        && normalized.options.width.is_none()
        && normalized.options.height.is_none()
        && normalized.options.quality.is_none()
        && normalized.options.background.is_none()
        && normalized.options.rotate.is_identity()
        && normalized.options.crop.is_none()
        && normalized.options.blur.is_none()
        && normalized.options.sharpen.is_none()
        && !normalized.options.grayscale
        && normalized.watermark.is_none()
        // Auto-orientation is satisfied either by there being nothing to apply, or by the
        // tag surviving into the output: the stored pixels and the retained tag then
        // describe the same picture they described in the input, and rotating them —
        // which would need a decode and a re-encode, and so would not be lossless — is
        // not what makes the result correct.
        && (!normalized.options.auto_orient
            || auto_orientation_is_noop(&normalized.input)
            || metadata_policy_retains_exif(normalized.options.metadata_policy))
}

/// Reports whether this policy copies a JPEG's Exif APP1 segment through.
///
/// Asked of the same function that does the copying, so the two cannot disagree.
fn metadata_policy_retains_exif(metadata_policy: MetadataPolicy) -> bool {
    should_keep_jpeg_segment(0xE1, b"Exif\0\0", metadata_policy)
}

/// The input's own bytes, when the request may return them and they are smaller.
///
/// `optimize` exists to make a file smaller, so handing back more bytes than it was given is
/// a failure of the command whatever the encoder decided. It happens: a flat-colour JPEG
/// re-encodes larger than whatever wrote it, and an indexed-colour PNG has no encoder here
/// at all, so it comes back as truecolour and grows by half again.
///
/// The input is a legal answer only when nothing about the pixels was asked to change and
/// the metadata policy is already satisfied by the file as it stands, which is what
/// `is_passthrough_lossless_request` and the per-format checks below decide.
///
/// This holds for `lossy` as much as for the other two. Asking for a lossy optimization is
/// asking for the smallest acceptable file, not for a re-encode at any price, and a caller
/// who does want a particular encode names a `quality` — which `is_passthrough_lossless_request`
/// already treats as disqualifying. A `targetQuality` does not disqualify it: the input
/// scores perfectly against itself and is smaller than any re-encode that also meets the
/// target, so it is the best answer to the question that was asked.
///
/// `none` is the one mode that never passes through, because it is the mode that says no
/// optimization was requested at all.
fn smaller_passthrough(normalized: &NormalizedTransformRequest, encoded: &[u8]) -> Option<Vec<u8>> {
    if matches!(normalized.options.optimize, OptimizeMode::None)
        || !is_passthrough_lossless_request(normalized)
    {
        return None;
    }

    let candidate = match normalized.options.format {
        MediaType::Jpeg => optimize_jpeg_bytes_losslessly(
            &normalized.input.bytes,
            normalized.options.metadata_policy,
        )
        .ok()?,
        MediaType::Png => png_bytes_satisfying_metadata_policy(
            &normalized.input.bytes,
            normalized.options.metadata_policy,
        )?
        .to_vec(),
        MediaType::Webp => webp_bytes_satisfying_metadata_policy(
            &normalized.input.bytes,
            normalized.options.metadata_policy,
        )?,
        _ => return None,
    };

    (candidate.len() < encoded.len()).then_some(candidate)
}

/// Returns the PNG unchanged when it carries no metadata the policy would remove.
///
/// This never rewrites the container. Either the file already satisfies the policy and can
/// be handed back byte for byte, or it does not and the re-encode stands: filtering chunks
/// would mean deciding what every ancillary chunk means, and that is a larger contract than
/// "do not make the file bigger" needs.
fn png_bytes_satisfying_metadata_policy(
    bytes: &[u8],
    metadata_policy: MetadataPolicy,
) -> Option<&[u8]> {
    if metadata_policy == MetadataPolicy::KeepAll {
        return Some(bytes);
    }

    // The chunks truss treats as metadata: an ICC profile, EXIF, and the three text forms
    // that carry XMP and comments. Everything else is either critical or a rendering hint
    // the policy says nothing about.
    const METADATA_CHUNKS: [&[u8; 4]; 5] = [b"iCCP", b"eXIf", b"tEXt", b"zTXt", b"iTXt"];

    let kept: &[u8; 4] = match metadata_policy {
        MetadataPolicy::PreserveIcc => b"iCCP",
        MetadataPolicy::PreserveExif => b"eXIf",
        MetadataPolicy::StripAll | MetadataPolicy::KeepAll => b"\0\0\0\0",
    };

    // Past the 8-byte PNG signature; a shorter input never reaches here, because it would
    // not have sniffed as a PNG.
    let mut offset = 8;
    while offset + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?) as usize;
        let chunk_type: &[u8; 4] = bytes.get(offset + 4..offset + 8)?.try_into().ok()?;
        if chunk_type != kept && METADATA_CHUNKS.contains(&chunk_type) {
            return None;
        }
        if chunk_type == b"IEND" {
            return Some(bytes);
        }
        offset = offset.checked_add(12)?.checked_add(length)?;
    }

    None
}

/// The WebP container with the metadata chunks the policy names removed, or `None` when
/// the file is not a WebP truss can rewrite.
///
/// Unlike PNG, whose ancillary chunk space is open-ended, a WebP carries metadata in three
/// chunks the container specification names: `ICCP`, `EXIF`, and `XMP `. Dropping the ones
/// the policy removes is therefore a decision about a closed set rather than about every
/// chunk an encoder might have written, which is why this rewrites the container where
/// `png_bytes_satisfying_metadata_policy` declines to.
///
/// The result is the input's own pixels, so it is far smaller than a re-encode: the lossless
/// WebP encoder reached through the `image` crate is a plain one, and a picture libwebp
/// compressed well comes back two and a half times its size.
fn webp_bytes_satisfying_metadata_policy(
    bytes: &[u8],
    metadata_policy: MetadataPolicy,
) -> Option<Vec<u8>> {
    const ICC_FLAG: u8 = 0x20;
    const EXIF_FLAG: u8 = 0x08;
    const XMP_FLAG: u8 = 0x04;

    if metadata_policy == MetadataPolicy::KeepAll {
        return Some(bytes.to_vec());
    }

    // Each entry is a chunk the policy may remove, paired with the VP8X flag that
    // advertises it, so the flags and the chunks cannot fall out of step.
    let removed: &[(&[u8; 4], u8)] = match metadata_policy {
        MetadataPolicy::PreserveIcc => &[(b"EXIF", EXIF_FLAG), (b"XMP ", XMP_FLAG)],
        MetadataPolicy::PreserveExif => &[(b"ICCP", ICC_FLAG), (b"XMP ", XMP_FLAG)],
        MetadataPolicy::StripAll => &[
            (b"ICCP", ICC_FLAG),
            (b"EXIF", EXIF_FLAG),
            (b"XMP ", XMP_FLAG),
        ],
        MetadataPolicy::KeepAll => &[],
    };

    let chunks = parse_webp_chunks(bytes).ok()?;
    let is_removed = |fourcc: &[u8; 4]| removed.iter().any(|(name, _)| *name == fourcc);
    if !chunks.iter().any(|chunk| is_removed(&chunk.fourcc)) {
        // Nothing to drop, so the file already satisfies the policy byte for byte.
        return Some(bytes.to_vec());
    }

    let mut body = Vec::with_capacity(bytes.len());
    for chunk in &chunks {
        if is_removed(&chunk.fourcc) {
            continue;
        }
        let payload = bytes.get(chunk.start..chunk.end)?;
        if &chunk.fourcc == b"VP8X" {
            // A VP8X that still advertises a chunk that is gone makes the file invalid.
            let mut vp8x = payload.to_vec();
            let flags = vp8x.first_mut()?;
            for (_, flag) in removed {
                *flags &= !flag;
            }
            push_webp_chunk(&mut body, b"VP8X", &vp8x);
            continue;
        }
        push_webp_chunk(&mut body, &chunk.fourcc, payload);
    }

    let riff_size = u32::try_from(body.len() + 4).ok()?;
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    Some(out)
}

/// The message for a lossless JPEG request that cannot be served as a passthrough.
///
/// An EXIF orientation gets its own wording because it is the common reason — a phone
/// photo carries one — and because the generic message blames "pixel transforms" the
/// caller did not ask for, which sends readers looking for a flag they never passed.
fn lossless_jpeg_refusal(normalized: &NormalizedTransformRequest) -> String {
    if normalized.options.auto_orient
        && !auto_orientation_is_noop(&normalized.input)
        && let Some(orientation) =
            crate::core::exif_orientation(normalized.input.media_type, &normalized.input.bytes)
    {
        return format!(
            "lossless JPEG optimization cannot apply the EXIF orientation ({orientation}) this file carries; keep the metadata to preserve the file's own orientation, or use a re-encoding optimize mode"
        );
    }

    "lossless JPEG optimization is only supported when no pixel transforms are applied".to_string()
}

/// The warning for an input whose EXIF orientation the output records nowhere.
///
/// With auto-orientation off the pixels stay as stored, and when the tag that said how to
/// display them is gone too, the output displays rotated. Each flag on its own is honest;
/// the combination is a rotation, and nothing else says so.
///
/// Whether the tag is gone is asked of the output bytes, not of the metadata policy. A
/// policy that keeps Exif keeps nothing from a container whose metadata is never read, and
/// keeps it nowhere in an output format that cannot carry it, and both used to pass for
/// retained; what the output actually says is the one answer that covers every pair.
fn dropped_orientation_warning(
    input: &Artifact,
    auto_orient: bool,
    output_orientation: Option<u16>,
) -> Option<TransformWarning> {
    if auto_orient || matches!(output_orientation, Some(2..=8)) {
        return None;
    }

    match crate::core::exif_orientation(input.media_type, &input.bytes) {
        Some(orientation @ 2..=8) => Some(TransformWarning::OrientationDropped { orientation }),
        _ => None,
    }
}

/// Reports whether auto-orientation would leave the pixels as they are stored.
///
/// A passthrough hands the input's own bytes back, which is only the same picture when
/// there is no orientation to apply.
fn auto_orientation_is_noop(input: &Artifact) -> bool {
    matches!(
        crate::core::exif_orientation(input.media_type, &input.bytes),
        None | Some(0 | 1)
    )
}

fn optimize_jpeg_bytes_losslessly(
    bytes: &[u8],
    metadata_policy: MetadataPolicy,
) -> Result<Vec<u8>, TransformError> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return Err(TransformError::InvalidInput(
            "input is not a valid JPEG bitstream".to_string(),
        ));
    }

    let mut output = Vec::with_capacity(bytes.len());
    output.extend_from_slice(&bytes[..2]);

    let mut index = 2usize;
    while index + 1 < bytes.len() {
        if bytes[index] != 0xFF {
            return Err(TransformError::InvalidInput(
                "input is not a valid JPEG bitstream".to_string(),
            ));
        }

        let mut marker_index = index + 1;
        while marker_index < bytes.len() && bytes[marker_index] == 0xFF {
            marker_index += 1;
        }
        if marker_index >= bytes.len() {
            return Err(TransformError::InvalidInput(
                "input is not a valid JPEG bitstream".to_string(),
            ));
        }

        let marker = bytes[marker_index];
        index = marker_index + 1;

        match marker {
            0xD9 => {
                output.extend_from_slice(&[0xFF, marker]);
                return Ok(output);
            }
            0xDA => {
                let segment_end = jpeg_segment_end(bytes, index)?;
                if !bytes[segment_end..]
                    .windows(2)
                    .any(|window| window == [0xFF, 0xD9])
                {
                    return Err(TransformError::InvalidInput(
                        "input is not a valid JPEG bitstream".to_string(),
                    ));
                }
                output.extend_from_slice(&[0xFF, marker]);
                output.extend_from_slice(&bytes[index..segment_end]);
                output.extend_from_slice(&bytes[segment_end..]);
                return Ok(output);
            }
            0x01 | 0xD0..=0xD7 => {
                output.extend_from_slice(&[0xFF, marker]);
            }
            _ => {
                let segment_end = jpeg_segment_end(bytes, index)?;
                let payload = &bytes[index + 2..segment_end];
                if should_keep_jpeg_segment(marker, payload, metadata_policy) {
                    output.extend_from_slice(&[0xFF, marker]);
                    output.extend_from_slice(&bytes[index..segment_end]);
                }
                index = segment_end;
            }
        }
    }

    Err(TransformError::InvalidInput(
        "input is not a valid JPEG bitstream".to_string(),
    ))
}

fn jpeg_segment_end(bytes: &[u8], index: usize) -> Result<usize, TransformError> {
    if index + 2 > bytes.len() {
        return Err(TransformError::InvalidInput(
            "input is not a valid JPEG bitstream".to_string(),
        ));
    }
    let length = u16::from_be_bytes([bytes[index], bytes[index + 1]]) as usize;
    if length < 2 {
        return Err(TransformError::InvalidInput(
            "input is not a valid JPEG bitstream".to_string(),
        ));
    }
    let end = index + length;
    if end > bytes.len() {
        return Err(TransformError::InvalidInput(
            "input is not a valid JPEG bitstream".to_string(),
        ));
    }
    Ok(end)
}

fn should_keep_jpeg_segment(marker: u8, payload: &[u8], metadata_policy: MetadataPolicy) -> bool {
    match marker {
        0xE0 | 0xEE => true,
        0xE1..=0xEF | 0xFE => match metadata_policy {
            MetadataPolicy::KeepAll => true,
            MetadataPolicy::PreserveIcc => marker == 0xE2 && payload.starts_with(b"ICC_PROFILE\0"),
            MetadataPolicy::PreserveExif => marker == 0xE1 && payload.starts_with(b"Exif\0\0"),
            MetadataPolicy::StripAll => false,
        },
        _ => true,
    }
}

struct EncodedOutput {
    bytes: Vec<u8>,
    used_lossy_webp: bool,
}

#[derive(Clone, Copy)]
struct EncodeDeadline {
    start: Option<Instant>,
    deadline: Option<Duration>,
}

impl EncodeDeadline {
    fn check(self, stage: &'static str) -> Result<(), TransformError> {
        if let (Some(start), Some(limit)) = (self.start, self.deadline) {
            check_deadline(start.elapsed(), limit, stage)?;
        }
        Ok(())
    }
}

fn encode_output(
    image: &DynamicImage,
    media_type: MediaType,
    options: &NormalizedTransformOptions,
    retained_metadata: Option<&RetainedMetadata>,
    deadline: EncodeDeadline,
    warnings: &mut Vec<TransformWarning>,
) -> Result<EncodedOutput, TransformError> {
    match options.optimize {
        OptimizeMode::None => {
            encode_baseline_output(image, media_type, options.quality, retained_metadata)
        }
        OptimizeMode::Auto => encode_auto_output(
            image,
            media_type,
            options,
            retained_metadata,
            deadline,
            warnings,
        ),
        OptimizeMode::Lossless => {
            encode_lossless_optimized_output(image, media_type, retained_metadata, deadline)
        }
        OptimizeMode::Lossy => encode_lossy_optimized_output(
            image,
            media_type,
            options,
            retained_metadata,
            deadline,
            warnings,
        ),
    }
}

fn encode_auto_output(
    image: &DynamicImage,
    media_type: MediaType,
    options: &NormalizedTransformOptions,
    retained_metadata: Option<&RetainedMetadata>,
    deadline: EncodeDeadline,
    warnings: &mut Vec<TransformWarning>,
) -> Result<EncodedOutput, TransformError> {
    let baseline = encode_baseline_output(image, media_type, options.quality, retained_metadata)?;
    deadline.check("encode auto baseline")?;

    // The attempt's warnings are about the attempt, so they are kept only if it is chosen.
    let mut attempt_warnings = Vec::new();
    let optimized = match media_type {
        MediaType::Png => encode_png_optimized(image, retained_metadata, deadline)?,
        MediaType::Jpeg | MediaType::Webp | MediaType::Avif => {
            match encode_lossy_optimized_output(
                image,
                media_type,
                options,
                retained_metadata,
                deadline,
                &mut attempt_warnings,
            ) {
                Ok(output) => output,
                Err(TransformError::CapabilityMissing(_)) if media_type == MediaType::Webp => {
                    return Ok(baseline);
                }
                Err(error) => return Err(error),
            }
        }
        _ => return Ok(baseline),
    };

    // A target the caller named is a requirement, and the baseline never looked at it, so
    // the size comparison is only for the default target `auto` picks on its own. When the
    // baseline would have met the named target the search lands at or below its quality
    // and is no larger, so the comparison only ever changed the answer when the baseline
    // missed the target, which is exactly when it must not win.
    if options.target_quality.is_some() || optimized.bytes.len() < baseline.bytes.len() {
        warnings.append(&mut attempt_warnings);
        Ok(optimized)
    } else {
        Ok(baseline)
    }
}

fn encode_lossless_optimized_output(
    image: &DynamicImage,
    media_type: MediaType,
    retained_metadata: Option<&RetainedMetadata>,
    deadline: EncodeDeadline,
) -> Result<EncodedOutput, TransformError> {
    match media_type {
        MediaType::Png => encode_png_optimized(image, retained_metadata, deadline),
        MediaType::Webp => {
            deadline.check("encode lossless webp")?;
            encode_webp_lossless(image, retained_metadata)
        }
        MediaType::Jpeg => Err(TransformError::CapabilityMissing(
            "lossless JPEG optimization is only supported when no pixel transforms are applied"
                .to_string(),
        )),
        MediaType::Avif => Err(TransformError::CapabilityMissing(
            "lossless optimization is not implemented for avif output".to_string(),
        )),
        _ => Err(TransformError::InvalidOptions(format!(
            "optimization is not supported for {} output",
            media_type.as_name()
        ))),
    }
}

fn encode_lossy_optimized_output(
    image: &DynamicImage,
    media_type: MediaType,
    options: &NormalizedTransformOptions,
    retained_metadata: Option<&RetainedMetadata>,
    deadline: EncodeDeadline,
    warnings: &mut Vec<TransformWarning>,
) -> Result<EncodedOutput, TransformError> {
    let target = options.target_quality.or_else(|| {
        if options.quality.is_none() {
            default_lossy_target_quality(media_type)
        } else {
            None
        }
    });

    let max_quality = options.quality.unwrap_or(100);
    if let Some(target) = target {
        // A shortfall is worth a word only for a target the caller named: the default
        // `auto` aims at is a preference, not a promise the caller was given.
        let mut shortfall = Vec::new();
        let encoded = encode_lossy_with_target(
            image,
            media_type,
            target,
            max_quality,
            retained_metadata,
            deadline,
            &mut shortfall,
        )?;
        if options.target_quality.is_some() {
            warnings.append(&mut shortfall);
        }
        Ok(encoded)
    } else {
        let quality = options
            .quality
            .unwrap_or_else(|| default_lossy_quality(media_type));
        encode_lossy_with_quality(
            image,
            media_type,
            quality,
            retained_metadata,
            true,
            deadline,
        )
    }
}

fn default_lossy_quality(media_type: MediaType) -> u8 {
    match media_type {
        MediaType::Jpeg => 76,
        MediaType::Webp => 75,
        MediaType::Avif => 68,
        _ => 80,
    }
}

fn encode_lossy_with_target(
    image: &DynamicImage,
    media_type: MediaType,
    target: TargetQuality,
    max_quality: u8,
    retained_metadata: Option<&RetainedMetadata>,
    deadline: EncodeDeadline,
    warnings: &mut Vec<TransformWarning>,
) -> Result<EncodedOutput, TransformError> {
    let mut low = 1u8;
    let mut high = max_quality.max(1);
    let mut best: Option<EncodedOutput> = None;

    while low <= high {
        let mid = low + (high - low) / 2;
        let candidate =
            encode_lossy_with_quality(image, media_type, mid, retained_metadata, true, deadline)?;
        deadline.check("encode lossy optimization candidate")?;
        let score =
            measure_quality_metric(image, &candidate.bytes, media_type, target.metric, deadline)?;
        deadline.check("measure lossy optimization quality")?;

        if score >= target.value {
            best = Some(candidate);
            if mid == 1 {
                break;
            }
            high = mid - 1;
        } else {
            low = mid.saturating_add(1);
        }
    }

    if let Some(best) = best {
        return Ok(best);
    }

    // No quality the search was allowed reaches the target. The highest one is the closest
    // there is, and returning it silently would pass a shortfall off as an answer, so the
    // score it did reach is reported alongside it.
    let quality = max_quality.max(1);
    let fallback = encode_lossy_with_quality(
        image,
        media_type,
        quality,
        retained_metadata,
        true,
        deadline,
    )?;
    let achieved =
        measure_quality_metric(image, &fallback.bytes, media_type, target.metric, deadline)?;
    warnings.push(TransformWarning::TargetQualityNotReached {
        target,
        achieved,
        quality,
    });
    Ok(fallback)
}

fn measure_quality_metric(
    reference: &DynamicImage,
    encoded_bytes: &[u8],
    media_type: MediaType,
    metric: QualityMetric,
    deadline: EncodeDeadline,
) -> Result<f32, TransformError> {
    let decoded = decode_encoded_output(encoded_bytes, media_type)?;
    deadline.check("decode lossy optimization candidate")?;
    match metric {
        QualityMetric::Ssim => compute_ssim(reference, &decoded, deadline),
        QualityMetric::Psnr => compute_psnr(reference, &decoded, deadline),
    }
}

fn decode_encoded_output(
    bytes: &[u8],
    media_type: MediaType,
) -> Result<DynamicImage, TransformError> {
    let artifact = sniff_artifact(RawArtifact::new(bytes.to_vec(), Some(media_type)))?;
    decode_input(&artifact)
}

fn compute_psnr(
    reference: &DynamicImage,
    candidate: &DynamicImage,
    deadline: EncodeDeadline,
) -> Result<f32, TransformError> {
    let lhs = reference.to_rgba8();
    let rhs = candidate.to_rgba8();
    if lhs.dimensions() != rhs.dimensions() {
        return Err(TransformError::EncodeFailed(
            "quality metric comparison produced mismatched dimensions".to_string(),
        ));
    }

    let mut squared_error = 0f64;
    for (index, (left, right)) in lhs.pixels().zip(rhs.pixels()).enumerate() {
        for (a, b) in left.0.iter().zip(right.0.iter()) {
            let delta = f64::from(*a) - f64::from(*b);
            squared_error += delta * delta;
        }
        if index.is_multiple_of(16_384) {
            deadline.check("measure lossy optimization psnr")?;
        }
    }

    let sample_count = f64::from(lhs.width()) * f64::from(lhs.height()) * 4.0;
    let mse = squared_error / sample_count.max(1.0);
    if mse == 0.0 {
        return Ok(f32::INFINITY);
    }

    Ok((10.0 * ((255.0f64 * 255.0) / mse).log10()) as f32)
}

fn compute_ssim(
    reference: &DynamicImage,
    candidate: &DynamicImage,
    deadline: EncodeDeadline,
) -> Result<f32, TransformError> {
    let lhs = reference.to_luma8();
    let rhs = candidate.to_luma8();
    if lhs.dimensions() != rhs.dimensions() {
        return Err(TransformError::EncodeFailed(
            "quality metric comparison produced mismatched dimensions".to_string(),
        ));
    }

    let sample_count = f64::from(lhs.width()) * f64::from(lhs.height());
    if sample_count <= 0.0 {
        return Ok(1.0);
    }

    let mut mean_x = 0f64;
    let mut mean_y = 0f64;
    for (index, (left, right)) in lhs.pixels().zip(rhs.pixels()).enumerate() {
        mean_x += f64::from(left.0[0]);
        mean_y += f64::from(right.0[0]);
        if index.is_multiple_of(16_384) {
            deadline.check("measure lossy optimization ssim mean")?;
        }
    }
    mean_x /= sample_count;
    mean_y /= sample_count;

    let mut variance_x = 0f64;
    let mut variance_y = 0f64;
    let mut covariance = 0f64;
    for (index, (left, right)) in lhs.pixels().zip(rhs.pixels()).enumerate() {
        let x = f64::from(left.0[0]) - mean_x;
        let y = f64::from(right.0[0]) - mean_y;
        variance_x += x * x;
        variance_y += y * y;
        covariance += x * y;
        if index.is_multiple_of(16_384) {
            deadline.check("measure lossy optimization ssim variance")?;
        }
    }
    variance_x /= sample_count;
    variance_y /= sample_count;
    covariance /= sample_count;

    let c1 = (0.01 * 255.0f64).powi(2);
    let c2 = (0.03 * 255.0f64).powi(2);
    let numerator = (2.0 * mean_x * mean_y + c1) * (2.0 * covariance + c2);
    let denominator = (mean_x.powi(2) + mean_y.powi(2) + c1) * (variance_x + variance_y + c2);

    if denominator == 0.0 {
        return Ok(1.0);
    }

    Ok((numerator / denominator).clamp(0.0, 1.0) as f32)
}

fn encode_baseline_output(
    image: &DynamicImage,
    media_type: MediaType,
    quality: Option<u8>,
    retained_metadata: Option<&RetainedMetadata>,
) -> Result<EncodedOutput, TransformError> {
    match media_type {
        MediaType::Jpeg => Ok(EncodedOutput {
            bytes: encode_jpeg(image, quality.unwrap_or(80), retained_metadata)?,
            used_lossy_webp: false,
        }),
        MediaType::Png => Ok(EncodedOutput {
            bytes: encode_png(
                image,
                retained_metadata,
                PngCompressionType::Default,
                PngFilterType::Adaptive,
            )?,
            used_lossy_webp: false,
        }),
        MediaType::Webp => {
            if let Some(quality) = quality {
                Ok(EncodedOutput {
                    bytes: encode_webp_lossy_bytes(image, quality)?,
                    used_lossy_webp: true,
                })
            } else {
                encode_webp_lossless(image, retained_metadata)
            }
        }
        MediaType::Avif => Ok(EncodedOutput {
            bytes: encode_avif(
                image,
                quality.unwrap_or(80),
                avif_speed(output_pixels(image), false),
                retained_metadata,
            )?,
            used_lossy_webp: false,
        }),
        MediaType::Bmp => Ok(EncodedOutput {
            bytes: encode_bmp(image)?,
            used_lossy_webp: false,
        }),
        MediaType::Tiff => Ok(EncodedOutput {
            bytes: encode_tiff(image)?,
            used_lossy_webp: false,
        }),
        MediaType::Svg => Err(TransformError::EncodeFailed(
            "SVG encoding should be handled by transform_svg".into(),
        )),
        MediaType::Gif => Err(TransformError::EncodeFailed(
            "GIF output should be rejected before encoding".into(),
        )),
    }
}

fn encode_png_optimized(
    image: &DynamicImage,
    retained_metadata: Option<&RetainedMetadata>,
    deadline: EncodeDeadline,
) -> Result<EncodedOutput, TransformError> {
    let strategies = [
        (PngCompressionType::Best, PngFilterType::Adaptive),
        (PngCompressionType::Best, PngFilterType::Paeth),
        (PngCompressionType::Best, PngFilterType::Sub),
        (PngCompressionType::Level(9), PngFilterType::Adaptive),
        (PngCompressionType::Level(9), PngFilterType::Paeth),
    ];

    let mut best = encode_png(
        image,
        retained_metadata,
        PngCompressionType::Default,
        PngFilterType::Adaptive,
    )?;
    deadline.check("encode png optimization baseline")?;
    for (compression, filter) in strategies {
        let candidate = encode_png(image, retained_metadata, compression, filter)?;
        deadline.check("encode png optimization candidate")?;
        if candidate.len() < best.len() {
            best = candidate;
        }
    }

    Ok(EncodedOutput {
        bytes: best,
        used_lossy_webp: false,
    })
}

fn encode_lossy_with_quality(
    image: &DynamicImage,
    media_type: MediaType,
    quality: u8,
    retained_metadata: Option<&RetainedMetadata>,
    optimized: bool,
    deadline: EncodeDeadline,
) -> Result<EncodedOutput, TransformError> {
    match media_type {
        MediaType::Jpeg => {
            let bytes = encode_jpeg(image, quality, retained_metadata)?;
            deadline.check("encode lossy jpeg")?;
            Ok(EncodedOutput {
                bytes,
                used_lossy_webp: false,
            })
        }
        MediaType::Webp => {
            let bytes = encode_webp_lossy_bytes(image, quality)?;
            deadline.check("encode lossy webp")?;
            Ok(EncodedOutput {
                bytes,
                used_lossy_webp: true,
            })
        }
        MediaType::Avif => {
            let bytes = encode_avif(
                image,
                quality,
                avif_speed(output_pixels(image), optimized),
                retained_metadata,
            )?;
            deadline.check("encode lossy avif")?;
            Ok(EncodedOutput {
                bytes,
                used_lossy_webp: false,
            })
        }
        _ => Err(TransformError::InvalidOptions(format!(
            "lossy optimization is not supported for {} output",
            media_type.as_name()
        ))),
    }
}

fn encode_jpeg(
    image: &DynamicImage,
    quality: u8,
    retained_metadata: Option<&RetainedMetadata>,
) -> Result<Vec<u8>, TransformError> {
    let mut bytes = Vec::new();
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
    Ok(bytes)
}

/// Returns `true` when any pixel of `image` is non-opaque.
///
/// A color model that carries an alpha channel does not imply the image uses it: decoders and
/// the padding helpers routinely widen opaque images to RGBA. Scanning the samples is what lets
/// the encoders drop an alpha channel that holds no information.
fn image_has_transparency(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageLumaA8(buffer) => buffer.pixels().any(|pixel| pixel[1] != u8::MAX),
        DynamicImage::ImageLumaA16(buffer) => buffer.pixels().any(|pixel| pixel[1] != u16::MAX),
        DynamicImage::ImageRgba8(buffer) => buffer.pixels().any(|pixel| pixel[3] != u8::MAX),
        DynamicImage::ImageRgba16(buffer) => buffer.pixels().any(|pixel| pixel[3] != u16::MAX),
        DynamicImage::ImageRgba32F(buffer) => buffer.pixels().any(|pixel| pixel[3] < 1.0),
        // Every other variant is alpha-less by construction.
        _ => false,
    }
}

/// The 8-bit sample buffer an encoder writes, narrowed to RGB when the image is fully opaque.
///
/// Encoding an opaque image as RGBA adds an alpha channel the input never had, which inflates
/// the output and breaks the expectation that a same-format pass preserves the color model
/// (<https://github.com/nao1215/truss/issues/253>). Dropping an all-opaque alpha channel is
/// pixel-lossless, so the narrower form is always safe.
enum EncodeSamples {
    Rgb(image::RgbImage),
    Rgba(RgbaImage),
}

impl EncodeSamples {
    fn from_image(image: &DynamicImage) -> Self {
        if image_has_transparency(image) {
            Self::Rgba(image.to_rgba8())
        } else {
            Self::Rgb(image.to_rgb8())
        }
    }

    fn color_type(&self) -> ColorType {
        match self {
            Self::Rgb(_) => ColorType::Rgb8,
            Self::Rgba(_) => ColorType::Rgba8,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Rgb(buffer) => buffer.as_raw(),
            Self::Rgba(buffer) => buffer.as_raw(),
        }
    }

    fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Rgb(buffer) => buffer.dimensions(),
            Self::Rgba(buffer) => buffer.dimensions(),
        }
    }
}

fn encode_png(
    image: &DynamicImage,
    retained_metadata: Option<&RetainedMetadata>,
    compression: PngCompressionType,
    filter: PngFilterType,
) -> Result<Vec<u8>, TransformError> {
    let mut bytes = Vec::new();
    let mut encoder = PngEncoder::new_with_quality(&mut bytes, compression, filter);
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
    let samples = EncodeSamples::from_image(image);
    let (width, height) = samples.dimensions();
    encoder
        .write_image(
            samples.as_bytes(),
            width,
            height,
            samples.color_type().into(),
        )
        .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
    Ok(bytes)
}

fn encode_webp_lossless(
    image: &DynamicImage,
    retained_metadata: Option<&RetainedMetadata>,
) -> Result<EncodedOutput, TransformError> {
    let mut bytes = Vec::new();
    let samples = EncodeSamples::from_image(image);
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
    let (width, height) = samples.dimensions();
    encoder
        .write_image(
            samples.as_bytes(),
            width,
            height,
            samples.color_type().into(),
        )
        .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;

    Ok(EncodedOutput {
        bytes,
        used_lossy_webp: false,
    })
}

fn encode_webp_lossy_bytes(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, TransformError> {
    #[cfg(feature = "webp-lossy")]
    {
        // Feeding libwebp an RGB buffer for an opaque image keeps the ALPH chunk out of the
        // container, so the output does not gain an alpha channel the input never had.
        let samples = EncodeSamples::from_image(image);
        let (width, height) = samples.dimensions();
        let lossy_encoder = match samples {
            EncodeSamples::Rgb(ref buffer) => {
                webp::Encoder::from_rgb(buffer.as_raw(), width, height)
            }
            EncodeSamples::Rgba(ref buffer) => {
                webp::Encoder::from_rgba(buffer.as_raw(), width, height)
            }
        };
        Ok(lossy_encoder.encode(f32::from(quality)).to_vec())
    }
    #[cfg(not(feature = "webp-lossy"))]
    {
        let _ = (image, quality);
        Err(TransformError::CapabilityMissing(
            "lossy WebP encoding is not enabled in this build".to_string(),
        ))
    }
}

/// The number of pixels an encoder is about to be handed.
fn output_pixels(image: &DynamicImage) -> u64 {
    let (width, height) = image.dimensions();
    u64::from(width) * u64::from(height)
}

/// The rav1e speed setting to encode an AVIF of this many pixels with.
///
/// rav1e's scale runs from 1, the slowest and smallest, to 10. truss asked for 4 at every
/// size, which is a fine setting for an image small enough that the time does not matter,
/// and [`MAX_OUTPUT_PIXELS`] allows outputs where it matters a great deal: 8192x8192 is
/// exactly that ceiling, and speed 4 takes 55 seconds on two cores against a 30 second
/// default deadline, or 218 seconds for a source the encoder finds hard.
///
/// Below the first step nothing changes. A small output is quick at speed 4 and a faster
/// setting does not reliably make it smaller: measured on two cores, speed 6 came out 1.3
/// percent larger than speed 4 on a 1.7MP gradient and 10 percent larger on a 0.3MP image
/// of noise. There is no deadline to save there, so there is no reason to spend the bytes.
///
/// Above it the trade turns over, because the alternative is a request that does not finish.
/// On two cores, with the source the encoder finds hardest:
///
/// | output | speed 4 | this ladder |
/// |--------|---------|-------------|
/// | 12MP | 58.4s | 15.5s at speed 8, 3.5 percent more bytes |
/// | 67MP | 218.2s | 19.2s at speed 10 |
///
/// The steps are placed so that the worst of those finishes inside the default deadline on
/// two cores, which neither did before. On ordinary content the larger sizes come out
/// smaller as well as faster: a 12MP gradient is 23,925 bytes at speed 8 against 25,885 at
/// speed 4.
///
/// `optimize` asks for the smallest file the encoder can produce and accepts the time, so it
/// runs two steps slower than the same size would otherwise. Below the first step that is
/// speed 2, which is what the optimizing path already used.
fn avif_speed(pixels: u64, optimized: bool) -> u8 {
    let speed = match pixels {
        0..=2_000_000 => 4,
        2_000_001..=16_000_000 => 8,
        _ => 10,
    };
    if optimized { speed - 2 } else { speed }
}

fn encode_avif(
    image: &DynamicImage,
    quality: u8,
    speed: u8,
    retained_metadata: Option<&RetainedMetadata>,
) -> Result<Vec<u8>, TransformError> {
    #[cfg(feature = "avif")]
    {
        if retained_metadata.is_some_and(|metadata| !metadata.is_empty()) {
            return Err(TransformError::CapabilityMissing(
                "metadata retention is not implemented for avif output".to_string(),
            ));
        }
        let mut bytes = Vec::new();
        let samples = EncodeSamples::from_image(image);
        let (width, height) = samples.dimensions();
        let encoder = AvifEncoder::new_with_speed_quality(&mut bytes, speed, quality);
        encoder
            .write_image(
                samples.as_bytes(),
                width,
                height,
                samples.color_type().into(),
            )
            .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
        Ok(bytes)
    }
    #[cfg(not(feature = "avif"))]
    {
        let _ = (image, quality, speed, retained_metadata);
        Err(TransformError::CapabilityMissing(
            "AVIF encoding is not enabled in this build".to_string(),
        ))
    }
}

fn encode_bmp(image: &DynamicImage) -> Result<Vec<u8>, TransformError> {
    let mut bytes = Vec::new();
    let samples = EncodeSamples::from_image(image);
    let (width, height) = samples.dimensions();
    image::codecs::bmp::BmpEncoder::new(&mut bytes)
        .write_image(
            samples.as_bytes(),
            width,
            height,
            samples.color_type().into(),
        )
        .map_err(|error: image::ImageError| TransformError::EncodeFailed(error.to_string()))?;
    Ok(bytes)
}

fn encode_tiff(image: &DynamicImage) -> Result<Vec<u8>, TransformError> {
    let samples = EncodeSamples::from_image(image);
    let (width, height) = samples.dimensions();
    let mut cursor = Cursor::new(Vec::new());
    image::codecs::tiff::TiffEncoder::new(&mut cursor)
        .write_image(
            samples.as_bytes(),
            width,
            height,
            samples.color_type().into(),
        )
        .map_err(|error: image::ImageError| TransformError::EncodeFailed(error.to_string()))?;
    Ok(cursor.into_inner())
}

/// A parsed RIFF chunk of a WebP container: its four-character code and payload range.
struct WebpChunk {
    fourcc: [u8; 4],
    start: usize,
    end: usize,
}

/// Splits a WebP container into its RIFF chunks.
fn parse_webp_chunks(encoded: &[u8]) -> Result<Vec<WebpChunk>, TransformError> {
    if encoded.len() < 12 || &encoded[0..4] != b"RIFF" || &encoded[8..12] != b"WEBP" {
        return Err(TransformError::EncodeFailed(
            "cannot inject metadata: output is not a valid WebP container".into(),
        ));
    }

    let mut chunks = Vec::new();
    let mut offset = 12;
    while offset + 8 <= encoded.len() {
        let mut fourcc = [0u8; 4];
        fourcc.copy_from_slice(&encoded[offset..offset + 4]);
        let size = u32::from_le_bytes([
            encoded[offset + 4],
            encoded[offset + 5],
            encoded[offset + 6],
            encoded[offset + 7],
        ]) as usize;
        let start = offset + 8;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= encoded.len())
            .ok_or_else(|| {
                TransformError::EncodeFailed(
                    "cannot inject metadata: WebP chunk exceeds file".into(),
                )
            })?;
        chunks.push(WebpChunk { fourcc, start, end });
        // Chunk payloads are padded to an even length.
        offset = end + (size % 2);
    }

    Ok(chunks)
}

/// Reads the canvas size and alpha flag a `VP8X` chunk must advertise.
///
/// A simple-format container carries them in the bitstream header instead, so they are read
/// from the `VP8 ` (lossy) or `VP8L` (lossless) chunk when no `VP8X` is present.
fn webp_canvas_info(
    encoded: &[u8],
    chunks: &[WebpChunk],
) -> Result<(u32, u32, bool), TransformError> {
    for chunk in chunks {
        let data = &encoded[chunk.start..chunk.end];
        match &chunk.fourcc {
            b"VP8 " if data.len() >= 10 && data[3..6] == [0x9D, 0x01, 0x2A] => {
                let width = u32::from(u16::from_le_bytes([data[6], data[7]]) & 0x3FFF);
                let height = u32::from(u16::from_le_bytes([data[8], data[9]]) & 0x3FFF);
                return Ok((width, height, false));
            }
            b"VP8L" if data.len() >= 5 && data[0] == 0x2F => {
                let bits = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                let width = (bits & 0x3FFF) + 1;
                let height = ((bits >> 14) & 0x3FFF) + 1;
                return Ok((width, height, (bits >> 28) & 1 != 0));
            }
            _ => {}
        }
    }

    Err(TransformError::EncodeFailed(
        "cannot inject metadata: WebP container has no image chunk".into(),
    ))
}

/// Appends one RIFF chunk, padding the payload to an even length.
fn push_webp_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if !payload.len().is_multiple_of(2) {
        out.push(0);
    }
}

/// Rewrites a WebP container so it carries the given ICC profile, EXIF, and XMP payloads.
///
/// libwebp emits a bare `RIFF....WEBPVP8 ` container for lossy output and has no API for
/// embedding metadata, which is why lossy WebP used to reject any retained metadata outright
/// (<https://github.com/nao1215/truss/issues/279>). The WebP container spec allows `ICCP`,
/// `EXIF`, and `XMP ` chunks alongside a lossy `VP8 ` bitstream as long as the file is in the
/// extended format, so a `VP8X` chunk is prepended when one is not already present and the
/// chunks are written in the order the spec mandates:
///
/// ```text
/// VP8X [ICCP] [ALPH] VP8 |VP8L [EXIF] [XMP ]
/// ```
///
/// Payloads that are already present in the container are left untouched, so calling this on
/// output from the `image` crate's lossless encoder (which writes `ICCP`/`EXIF` itself) only
/// adds what is missing.
fn inject_webp_metadata(
    encoded: &[u8],
    icc: Option<&[u8]>,
    exif: Option<&[u8]>,
    xmp: Option<&[u8]>,
) -> Result<Vec<u8>, TransformError> {
    const ICC_FLAG: u8 = 0x20;
    const ALPHA_FLAG: u8 = 0x10;
    const EXIF_FLAG: u8 = 0x08;
    const XMP_FLAG: u8 = 0x04;

    if icc.is_none() && exif.is_none() && xmp.is_none() {
        return Ok(encoded.to_vec());
    }

    let chunks = parse_webp_chunks(encoded)?;
    let existing = |fourcc: &[u8; 4]| chunks.iter().any(|chunk| &chunk.fourcc == fourcc);

    // Only add what the container does not already carry.
    let icc = icc.filter(|_| !existing(b"ICCP"));
    let exif = exif.filter(|_| !existing(b"EXIF"));
    let xmp = xmp.filter(|_| !existing(b"XMP "));
    if icc.is_none() && exif.is_none() && xmp.is_none() {
        return Ok(encoded.to_vec());
    }

    let mut vp8x = match chunks.iter().find(|chunk| &chunk.fourcc == b"VP8X") {
        Some(chunk) if chunk.end - chunk.start >= 10 => {
            encoded[chunk.start..chunk.start + 10].to_vec()
        }
        _ => {
            let (width, height, has_alpha) = webp_canvas_info(encoded, &chunks)?;
            if width == 0 || height == 0 || width > 1 << 24 || height > 1 << 24 {
                return Err(TransformError::EncodeFailed(
                    "cannot inject metadata: WebP canvas size is out of range".into(),
                ));
            }
            let mut payload = vec![0u8; 10];
            if has_alpha || existing(b"ALPH") {
                payload[0] |= ALPHA_FLAG;
            }
            payload[4..7].copy_from_slice(&(width - 1).to_le_bytes()[..3]);
            payload[7..10].copy_from_slice(&(height - 1).to_le_bytes()[..3]);
            payload
        }
    };

    if icc.is_some() {
        vp8x[0] |= ICC_FLAG;
    }
    if exif.is_some() {
        vp8x[0] |= EXIF_FLAG;
    }
    if xmp.is_some() {
        vp8x[0] |= XMP_FLAG;
    }

    // Re-emits a metadata chunk the container already had, since the new payloads were
    // filtered out for exactly those four-character codes.
    let push_existing = |body: &mut Vec<u8>, fourcc: &[u8; 4]| {
        if let Some(chunk) = chunks.iter().find(|chunk| &chunk.fourcc == fourcc) {
            push_webp_chunk(body, fourcc, &encoded[chunk.start..chunk.end]);
        }
    };

    let mut body = Vec::with_capacity(encoded.len() + 64);
    push_webp_chunk(&mut body, b"VP8X", &vp8x);
    match icc {
        Some(icc) => push_webp_chunk(&mut body, b"ICCP", icc),
        None => push_existing(&mut body, b"ICCP"),
    }
    for chunk in &chunks {
        // VP8X was rewritten above; metadata chunks are placed around the bitstream.
        if matches!(&chunk.fourcc, b"VP8X" | b"ICCP" | b"EXIF" | b"XMP ") {
            continue;
        }
        push_webp_chunk(&mut body, &chunk.fourcc, &encoded[chunk.start..chunk.end]);
    }
    match exif {
        Some(exif) => push_webp_chunk(&mut body, b"EXIF", exif),
        None => push_existing(&mut body, b"EXIF"),
    }
    match xmp {
        Some(xmp) => push_webp_chunk(&mut body, b"XMP ", xmp),
        None => push_existing(&mut body, b"XMP "),
    }

    let riff_size = u32::try_from(body.len() + 4).map_err(|_| {
        TransformError::EncodeFailed("cannot inject metadata: WebP output exceeds 4GB".into())
    })?;
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&body);
    Ok(out)
}

/// Injects metadata into encoded image bytes for formats that support post-encode
/// byte-level insertion. Returns the (possibly modified) bytes and removes successfully
/// injected metadata kinds from the warning list.
///
/// `lossy_webp` says whether the bytes came out of libwebp, which embeds nothing at all —
/// the lossless WebP encoder writes ICCP and EXIF itself, so only XMP is added there.
///
/// Injection failures (e.g. oversized payloads) are silently ignored — the original
/// encoded bytes are returned unchanged and any pre-inserted `MetadataDropped` warning
/// remains in place. This is intentional: metadata injection is best-effort.
fn inject_metadata(
    mut encoded: Vec<u8>,
    format: MediaType,
    metadata: &RetainedMetadata,
    lossy_webp: bool,
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
        MediaType::Webp => {
            // The lossless encoder writes ICCP/EXIF itself, so only XMP is missing there.
            // Lossy output comes straight out of libwebp with no metadata chunks at all.
            let (icc, exif) = if lossy_webp {
                (
                    metadata.icc_profile.as_deref(),
                    metadata.exif_metadata.as_deref(),
                )
            } else {
                (None, None)
            };
            if let Ok(result) =
                inject_webp_metadata(&encoded, icc, exif, metadata.xmp_metadata.as_deref())
            {
                encoded = result;
                warnings
                    .retain(|w| !matches!(w, TransformWarning::MetadataDropped(MetadataKind::Xmp)));
            } else {
                // The container could not be rewritten: report what did not survive.
                if icc.is_some() {
                    warnings.push(TransformWarning::MetadataDropped(MetadataKind::Icc));
                }
                if exif.is_some() {
                    warnings.push(TransformWarning::MetadataDropped(MetadataKind::Exif));
                }
            }
            // IPTC has no standard embedding in WebP; warning remains.
        }
        _ => {
            // AVIF: no post-encode injection supported.
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

    fn retain_icc_only(mut self) -> Self {
        self.exif_metadata = None;
        self.xmp_metadata = None;
        self.iptc_metadata = None;
        self
    }

    /// Retains metadata the output format can carry, and names what it could not.
    ///
    /// - JPEG: EXIF, ICC, XMP (APP1 injection), IPTC (APP13 injection)
    /// - PNG: EXIF, ICC, XMP (iTXt injection). IPTC has no standard PNG embedding.
    /// - WebP: EXIF, ICC, XMP in RIFF chunks. IPTC has no WebP container chunk.
    /// - AVIF: no injection at all, and EXIF and ICC are left in place here so that the
    ///   caller refuses the request outright rather than reporting a loss.
    /// - Everything else, TIFF and BMP: no injection path for any of the four.
    ///
    /// A kind that was there and cannot travel is returned rather than dropped in silence:
    /// the caller turns each into a `MetadataDropped` warning, which is what the caller of
    /// truss reads to learn that the file it asked for is not quite what it got.
    fn retain_supported(mut self, output_format: MediaType) -> (Self, Vec<MetadataKind>) {
        let (exif, icc, xmp, iptc) = match output_format {
            MediaType::Jpeg => (true, true, true, true),
            MediaType::Png | MediaType::Webp => (true, true, true, false),
            MediaType::Avif => (true, true, false, false),
            _ => (false, false, false, false),
        };

        let mut dropped = Vec::new();
        if !exif && self.exif_metadata.take().is_some() {
            dropped.push(MetadataKind::Exif);
        }
        if !icc && self.icc_profile.take().is_some() {
            dropped.push(MetadataKind::Icc);
        }
        if !xmp && self.xmp_metadata.take().is_some() {
            dropped.push(MetadataKind::Xmp);
        }
        if !iptc && self.iptc_metadata.take().is_some() {
            dropped.push(MetadataKind::Iptc);
        }
        (self, dropped)
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
    // The pixels have been turned, so a retained tag would turn them again in the viewer.
    if let Some(exif_chunk) = metadata.exif_metadata.as_mut()
        && auto_orient
    {
        let _ = Orientation::remove_from_exif_chunk(exif_chunk);
    }

    // The policy narrows the metadata to what the caller asked to keep; the format then
    // narrows it to what it can carry. Only the second step loses something the caller
    // wanted, so only it reports. Metadata that can be injected post-encode has its
    // warning removed later by inject_metadata on success.
    let metadata = match metadata_policy {
        MetadataPolicy::StripAll => return Ok((None, warnings)),
        MetadataPolicy::PreserveIcc => metadata.retain_icc_only(),
        MetadataPolicy::PreserveExif => metadata.retain_exif_only(),
        MetadataPolicy::KeepAll => metadata,
    };
    let (metadata, dropped) = metadata.retain_supported(output_format);
    warnings.extend(dropped.into_iter().map(TransformWarning::MetadataDropped));

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

/// Collects the metadata a decoder carries: EXIF, ICC, XMP, and IPTC.
///
/// The four reads are the same for every container, so the format only decides which decoder
/// to open. A failure here is a decode failure, not a metadata failure: the bytes claimed to
/// be this format and were not.
fn retained_metadata<D: ImageDecoder>(mut decoder: D) -> Result<RetainedMetadata, TransformError> {
    let decode_failed = |error: image::ImageError| TransformError::DecodeFailed(error.to_string());
    Ok(RetainedMetadata {
        exif_metadata: decoder.exif_metadata().map_err(decode_failed)?,
        icc_profile: decoder.icc_profile().map_err(decode_failed)?,
        xmp_metadata: decoder.xmp_metadata().map_err(decode_failed)?,
        iptc_metadata: decoder.iptc_metadata().map_err(decode_failed)?,
    })
}

/// Reads the metadata truss can carry from one format to another.
///
/// The formats not listed carry none through this path: AVIF and TIFF metadata is handled
/// where those are decoded, SVG has no such containers, and BMP and GIF have nowhere to put
/// them.
fn read_input_metadata(input: &Artifact) -> Result<RetainedMetadata, TransformError> {
    let bytes = Cursor::new(&input.bytes);
    let open_failed = |error: image::ImageError| TransformError::DecodeFailed(error.to_string());
    match input.media_type {
        MediaType::Jpeg => retained_metadata(JpegDecoder::new(bytes).map_err(open_failed)?),
        MediaType::Png => retained_metadata(PngDecoder::new(bytes).map_err(open_failed)?),
        MediaType::Webp => retained_metadata(WebPDecoder::new(bytes).map_err(open_failed)?),
        MediaType::Avif | MediaType::Svg | MediaType::Bmp | MediaType::Tiff | MediaType::Gif => {
            Ok(RetainedMetadata::default())
        }
    }
}

/// Reports whether an encoded file in this format can carry an alpha channel at all.
///
/// This is a property of the format, not of the image: it answers whether transparency
/// survives the encode, which is what decides whether the pixels have to be composited
/// against a background first.
pub(crate) const fn format_carries_alpha(media_type: MediaType) -> bool {
    !matches!(media_type, MediaType::Jpeg)
}

/// Reports whether the encoded output carries an alpha channel.
///
/// This mirrors the choice [`EncodeSamples::from_image`] makes, so the metadata reported by
/// `convert` matches what `inspect` reads back from the encoded file.
fn output_has_alpha(image: &DynamicImage, media_type: MediaType) -> bool {
    format_carries_alpha(media_type) && image_has_transparency(image)
}

/// Composites onto the background color when the output format cannot carry alpha.
///
/// Dropping the alpha channel keeps the color samples untouched, so a half-transparent red
/// encodes as a fully saturated red and a fully transparent pixel encodes as whatever color
/// sat under the zero alpha, which is usually black. Compositing first is also what makes a
/// direct conversion agree with one that pads: `pad_to_box` already resolves transparency
/// against this same background on its way through.
///
/// The background defaults to white for the formats that cannot carry alpha, which is
/// [`background_pixel`]'s rule.
pub(crate) fn flatten_for_opaque_output(
    image: DynamicImage,
    background: Option<Rgba8>,
    output_format: MediaType,
) -> DynamicImage {
    if format_carries_alpha(output_format) || !image_has_transparency(&image) {
        return image;
    }

    // Composited in place rather than onto a second canvas: `into_rgba8` reuses the buffer
    // when the image is already RGBA8, which is what a decoded image with alpha always is,
    // so the flattening costs one pass rather than an allocation and a copy as well.
    let fill = background_pixel(background, output_format);
    let mut buffer = image.into_rgba8();
    for pixel in buffer.pixels_mut() {
        // The two ends are what `Pixel::blend` short-circuits to, written out here so the
        // usual image — opaque almost everywhere, transparent in a corner — pays nothing
        // for the general case. Only partial alpha reaches the blend itself.
        match pixel.0[3] {
            0 => *pixel = fill,
            u8::MAX => {}
            _ => {
                let mut composited = fill;
                composited.blend(pixel);
                *pixel = composited;
            }
        }
    }
    DynamicImage::ImageRgba8(buffer)
}

#[cfg(test)]
mod tests {
    use super::{
        apply_exif_orientation, check_output_pixel_limit, check_rotated_pixel_limit,
        optimize_jpeg_bytes_losslessly, png_bytes_satisfying_metadata_policy,
        resolved_output_dimensions, rotated_bounding_box, transform_raster,
    };
    use crate::core::{
        Artifact, ArtifactMetadata, CropRegion, Fit, MediaType, MetadataKind, MetadataPolicy,
        OptimizeMode, Position, Rotation, TransformOptions, TransformRequest, TransformResult,
        TransformWarning, WatermarkInput,
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
    use rstest::rstest;
    use std::io::Cursor;

    /// A flat-colour JPEG written by an encoder that is not this crate's.
    const FLAT_JPEG: &[u8] = include_bytes!("../../integration/fixtures/flat.jpg");
    /// A lossless WebP written by libwebp, which compresses it far better than the plain
    /// encoder the image crate offers.
    const LIBWEBP_LOSSLESS: &[u8] =
        include_bytes!("../../integration/fixtures/libwebp-lossless.webp");

    /// Reads a RIFF chunk payload straight out of a WebP container.
    fn webp_chunk_payload(bytes: &[u8], fourcc: &[u8; 4]) -> Option<Vec<u8>> {
        super::parse_webp_chunks(bytes)
            .ok()?
            .into_iter()
            .find(|chunk| &chunk.fourcc == fourcc)
            .map(|chunk| bytes[chunk.start..chunk.end].to_vec())
    }

    fn webp_icc_profile(bytes: &[u8]) -> Option<Vec<u8>> {
        webp_chunk_payload(bytes, b"ICCP")
    }

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
                orientation: None,
            },
        )
    }

    /// Builds an opaque PNG stored as PNG color type 2 (truecolor, no alpha channel).
    fn opaque_rgb_png_artifact(width: u32, height: u32) -> Artifact {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x * 8) as u8, (y * 8) as u8, 128])
        });
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, width, height, ColorType::Rgb8.into())
            .expect("encode png");

        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff png");
        assert_eq!(
            artifact.metadata.has_alpha,
            Some(false),
            "fixture must start without an alpha channel"
        );
        artifact
    }

    fn encoded_png_has_alpha(bytes: &[u8]) -> bool {
        let decoder = PngDecoder::new(Cursor::new(bytes)).expect("decode png");
        decoder.color_type().has_alpha()
    }

    /// A PNG carrying an ICC profile, for the paths that only a PNG reaches.
    fn png_artifact_with_icc(icc_profile: &[u8]) -> Artifact {
        let image = image::RgbImage::from_pixel(4, 2, image::Rgb([10, 20, 30]));
        let mut bytes = Vec::new();
        let mut encoder = PngEncoder::new(&mut bytes);
        encoder
            .set_icc_profile(icc_profile.to_vec())
            .expect("set png icc profile");
        encoder
            .write_image(&image, 4, 2, ColorType::Rgb8.into())
            .expect("encode png");

        Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(4),
                height: Some(2),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
                orientation: None,
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
                orientation: None,
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
                orientation: None,
            },
        )
    }

    /// A minimal uncompressed RGB TIFF carrying an Orientation tag.
    ///
    /// Written by hand because no encoder in the tree can set tag 274, and a binary fixture
    /// would hide what makes the file interesting.
    fn tiff_bytes_with_orientation(width: u32, height: u32, orientation: u16) -> Vec<u8> {
        const IFD_OFFSET: u32 = 8;
        const ENTRY_COUNT: u16 = 10;
        // Header, entry count, the entries themselves, and the next-IFD pointer.
        const BITS_OFFSET: u32 = IFD_OFFSET + 2 + ENTRY_COUNT as u32 * 12 + 4;
        const PIXELS_OFFSET: u32 = BITS_OFFSET + 6;

        const SHORT: u16 = 3;
        const LONG: u16 = 4;

        let byte_count = width * height * 3;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"II");
        bytes.extend_from_slice(&42u16.to_le_bytes());
        bytes.extend_from_slice(&IFD_OFFSET.to_le_bytes());
        bytes.extend_from_slice(&ENTRY_COUNT.to_le_bytes());

        let mut entry = |tag: u16, field_type: u16, count: u32, value: u32| {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&field_type.to_le_bytes());
            bytes.extend_from_slice(&count.to_le_bytes());
            // A SHORT that fits in the value field is left-aligned in it.
            if field_type == SHORT && count == 1 {
                bytes.extend_from_slice(&u16::try_from(value).expect("short value").to_le_bytes());
                bytes.extend_from_slice(&0u16.to_le_bytes());
            } else {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        };

        entry(256, SHORT, 1, width);
        entry(257, SHORT, 1, height);
        entry(258, SHORT, 3, BITS_OFFSET);
        entry(259, SHORT, 1, 1);
        entry(262, SHORT, 1, 2);
        entry(273, LONG, 1, PIXELS_OFFSET);
        entry(274, SHORT, 1, u32::from(orientation));
        entry(277, SHORT, 1, 3);
        entry(278, SHORT, 1, height);
        entry(279, LONG, 1, byte_count);

        bytes.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..3 {
            bytes.extend_from_slice(&8u16.to_le_bytes());
        }
        bytes.extend(std::iter::repeat_n(0x40u8, byte_count as usize));
        bytes
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
                orientation: None,
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
                rotate: Rotation::DEG_90,
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
                auto_orient: false,
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

    /// With auto-orientation on, the tag has been spent on the pixels, so keeping it would
    /// turn them a second time in the viewer.
    #[test]
    fn transform_raster_clears_a_retained_png_orientation_once_it_has_been_applied() {
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

        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(4));

        let mut decoder = PngDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode png");
        let orientation = decoder
            .exif_metadata()
            .expect("read png exif")
            .and_then(|exif| Orientation::from_exif_chunk(&exif));
        assert!(
            matches!(orientation, None | Some(Orientation::NoTransforms)),
            "expected the tag to be cleared, got {orientation:?}"
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

    /// A lossless optimization drops the metadata it was told to drop without decoding the
    /// scan, and keeps the colour profile, which an optimization keeps under every mode.
    #[test]
    fn transform_raster_lossless_jpeg_optimization_strips_metadata_without_reencoding() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(1), Some(b"demo-icc-profile"));
        let input_len = artifact.bytes.len();
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Lossless,
                ..TransformOptions::default()
            },
        ))
        .expect("lossless jpeg optimize");

        let mut decoder =
            JpegDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode jpeg");

        assert!(result.artifact.bytes.len() < input_len);
        assert_eq!(decoder.exif_metadata().expect("read jpeg exif"), None);
        assert_eq!(
            decoder
                .icc_profile()
                .expect("read jpeg icc")
                .expect("retained icc"),
            b"demo-icc-profile".to_vec()
        );
    }

    #[test]
    fn transform_raster_lossy_jpeg_optimization_preserves_icc_by_default() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(1), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Lossy,
                ..TransformOptions::default()
            },
        ))
        .expect("lossy jpeg optimize should preserve icc");

        let mut decoder =
            JpegDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode jpeg");

        assert_eq!(decoder.exif_metadata().expect("read jpeg exif"), None);
        assert_eq!(
            decoder
                .icc_profile()
                .expect("read jpeg icc")
                .expect("retained icc"),
            b"demo-icc-profile".to_vec()
        );
    }

    /// `auto` reaches the same lossy encoder, so it keeps the profile for the same reason.
    ///
    /// It is the default mode of `truss optimize`, so this is the command as documented in
    /// the README rather than a mode a caller has to choose.
    #[rstest]
    #[case::auto(OptimizeMode::Auto)]
    #[case::lossy(OptimizeMode::Lossy)]
    fn an_optimized_jpeg_keeps_its_profile_whichever_mode_asked(#[case] optimize: OptimizeMode) {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(1), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize,
                ..TransformOptions::default()
            },
        ))
        .expect("the optimization should succeed");

        let mut decoder =
            JpegDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode jpeg");

        assert_eq!(
            decoder
                .icc_profile()
                .expect("read jpeg icc")
                .expect("retained icc"),
            b"demo-icc-profile".to_vec(),
            "{optimize:?} dropped the profile"
        );
    }

    /// A lossless optimization keeps it too: the pixels survive the re-encode and the
    /// profile is what says how to read them.
    #[test]
    fn a_losslessly_optimized_png_keeps_its_profile() {
        let artifact = png_artifact_with_icc(b"demo-icc-profile");
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                optimize: OptimizeMode::Lossless,
                ..TransformOptions::default()
            },
        ))
        .expect("the optimization should succeed");

        let mut decoder = PngDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode png");

        assert_eq!(
            decoder
                .icc_profile()
                .expect("read png icc")
                .expect("retained icc"),
            b"demo-icc-profile".to_vec()
        );
    }

    /// A plain encode is not an optimization and strips what it was told to strip.
    #[test]
    fn an_unoptimized_encode_still_strips_the_profile() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(1), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::None,
                strip_metadata: true,
                ..TransformOptions::default()
            },
        ))
        .expect("the encode should succeed");

        let mut decoder =
            JpegDecoder::new(Cursor::new(&result.artifact.bytes)).expect("decode jpeg");

        assert_eq!(decoder.icc_profile().expect("read jpeg icc"), None);
    }

    /// Metadata a caller asked to keep, for an output that cannot carry it, is reported.
    ///
    /// TIFF and BMP have no injection path for EXIF or an ICC profile, and dropping them
    /// in silence is the one answer the pipeline does not give anywhere else: XMP and IPTC
    /// raise this same warning, and AVIF refuses the request outright.
    #[rstest]
    #[case::tiff(MediaType::Tiff)]
    #[case::bmp(MediaType::Bmp)]
    fn metadata_an_output_cannot_carry_is_reported_rather_than_dropped(#[case] format: MediaType) {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(1), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(format),
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("the encode should succeed");

        let warnings: Vec<String> = result.warnings.iter().map(ToString::to_string).collect();

        assert!(
            warnings.iter().any(|w| w.contains("EXIF")),
            "{format:?} dropped the EXIF without saying so: {warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("ICC")),
            "{format:?} dropped the profile without saying so: {warnings:?}"
        );
    }

    /// The same silence under the other flag that names metadata to keep.
    #[test]
    fn a_preserved_exif_an_output_cannot_carry_is_reported() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(1), None);
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Tiff),
                preserve_exif: true,
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("the encode should succeed");

        let warnings: Vec<String> = result.warnings.iter().map(ToString::to_string).collect();

        assert!(
            warnings.iter().any(|w| w.contains("EXIF")),
            "the EXIF went missing without a word: {warnings:?}"
        );
    }

    /// A format that carries both says nothing, which is what keeps the warning meaningful.
    #[rstest]
    #[case::jpeg(MediaType::Jpeg)]
    #[case::png(MediaType::Png)]
    fn an_output_that_carries_the_metadata_warns_about_nothing(#[case] format: MediaType) {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(1), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(format),
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("the encode should succeed");

        assert!(
            result.warnings.is_empty(),
            "{format:?} carries both and should warn about neither: {:?}",
            result.warnings
        );
    }

    #[test]
    fn lossless_jpeg_optimization_rejects_truncated_scan_data() {
        let mut bytes = jpeg_artifact_with_metadata(4, 2, Some(1), Some(b"demo-icc-profile")).bytes;
        bytes.truncate(bytes.len() - 2);

        let error = optimize_jpeg_bytes_losslessly(&bytes, MetadataPolicy::StripAll)
            .expect_err("truncated jpeg should be rejected");

        assert_eq!(
            error,
            TransformError::InvalidInput("input is not a valid JPEG bitstream".to_string())
        );
    }

    #[cfg(feature = "webp-lossy")]
    #[test]
    // Regression test for https://github.com/nao1215/truss/issues/279: `--mode lossy
    // --format webp` used to fail outright on any ICC-bearing input, and because a strip
    // request is upgraded to "preserve ICC" for lossy output, no flag combination worked.
    fn transform_raster_lossy_webp_optimization_embeds_icc_profile() {
        let artifact = jpeg_artifact_with_metadata(4, 2, None, Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                optimize: OptimizeMode::Lossy,
                ..TransformOptions::default()
            },
        ))
        .expect("lossy webp optimize should embed the ICC profile");

        assert_eq!(result.artifact.media_type, MediaType::Webp);
        assert_eq!(
            webp_icc_profile(&result.artifact.bytes).as_deref(),
            Some(b"demo-icc-profile".as_slice())
        );
        assert!(
            !result
                .warnings
                .contains(&TransformWarning::MetadataDropped(MetadataKind::Icc))
        );
    }

    #[test]
    fn transform_raster_lossy_webp_succeeds_for_icc_input_with_strip_metadata() {
        let artifact = jpeg_artifact_with_metadata(4, 2, None, Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                optimize: OptimizeMode::Lossy,
                strip_metadata: true,
                ..TransformOptions::default()
            },
        ))
        .expect("--strip-metadata must never be the reason a command fails");

        // The strip request is upgraded to PreserveIcc for lossy output so colors stay
        // correct; that upgrade is only meaningful now that the profile can be written.
        assert_eq!(
            webp_icc_profile(&result.artifact.bytes).as_deref(),
            Some(b"demo-icc-profile".as_slice())
        );
    }

    #[test]
    fn transform_raster_lossy_webp_keeps_exif_and_icc_with_keep_metadata() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(6), Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                optimize: OptimizeMode::Lossy,
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("keep-metadata lossy webp should succeed");

        assert_eq!(
            webp_icc_profile(&result.artifact.bytes).as_deref(),
            Some(b"demo-icc-profile".as_slice())
        );
        assert!(
            webp_chunk_payload(&result.artifact.bytes, b"EXIF").is_some(),
            "EXIF should ride in an EXIF chunk"
        );
        assert!(
            result.warnings.is_empty(),
            "nothing was dropped, got: {:?}",
            result.warnings
        );
    }

    #[test]
    fn transform_raster_lossy_webp_output_is_decodable_after_injection() {
        let artifact = jpeg_artifact_with_metadata(8, 8, None, Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                optimize: OptimizeMode::Lossy,
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        // The rewritten container must still be a valid WebP for decoders and for sniffing.
        let sniffed =
            sniff_artifact(RawArtifact::new(result.artifact.bytes.clone(), None)).expect("sniff");
        assert_eq!(sniffed.media_type, MediaType::Webp);
        assert_eq!(sniffed.metadata.width, Some(8));
        assert_eq!(sniffed.metadata.height, Some(8));

        let decoded =
            image::load_from_memory_with_format(&result.artifact.bytes, image::ImageFormat::WebP)
                .expect("decode injected webp");
        assert_eq!(decoded.dimensions(), (8, 8));

        let mut decoder =
            WebPDecoder::new(Cursor::new(&result.artifact.bytes)).expect("webp decoder");
        assert_eq!(
            decoder.icc_profile().expect("icc").as_deref(),
            Some(b"demo-icc-profile".as_slice())
        );
    }

    #[cfg(feature = "avif")]
    #[test]
    fn transform_raster_lossy_avif_succeeds_for_icc_input_with_strip_metadata() {
        // AVIF cannot carry a profile truss writes, so a strip request must stay StripAll
        // instead of being upgraded into a state the encoder rejects.
        let artifact = jpeg_artifact_with_metadata(4, 2, None, Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Avif),
                optimize: OptimizeMode::Lossy,
                strip_metadata: true,
                ..TransformOptions::default()
            },
        ))
        .expect("--strip-metadata must never be the reason a command fails");

        assert_eq!(result.artifact.media_type, MediaType::Avif);
    }

    #[test]
    fn transform_raster_lossless_webp_gains_an_xmp_chunk() {
        let artifact = jpeg_with_xmp_iptc();
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("lossless webp keep-metadata should succeed");

        assert!(
            webp_chunk_payload(&result.artifact.bytes, b"XMP ").is_some(),
            "XMP should be injected as an `XMP ` chunk"
        );
        // IPTC has no WebP container chunk, so only that warning survives.
        assert_eq!(
            result.warnings,
            vec![TransformWarning::MetadataDropped(MetadataKind::Iptc)]
        );
    }

    /// The rewrite drops exactly the chunks the policy names and leaves the container
    /// readable, so the input's own pixels stay a legal answer for a policy that strips.
    #[rstest]
    #[case(MetadataPolicy::StripAll, &[])]
    #[case(MetadataPolicy::PreserveIcc, &[b"ICCP"])]
    #[case(MetadataPolicy::PreserveExif, &[b"EXIF"])]
    fn webp_bytes_satisfying_metadata_policy_drops_only_what_the_policy_names(
        #[case] policy: MetadataPolicy,
        #[case] kept: &[&[u8; 4]],
    ) {
        use super::{inject_webp_metadata, webp_bytes_satisfying_metadata_policy};

        let plain = transform_raster(TransformRequest::new(
            png_artifact(9, 5, Rgba([10, 20, 30, 255])),
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(80),
                ..TransformOptions::default()
            },
        ))
        .expect("encode lossy webp")
        .artifact
        .bytes;
        let carrying = inject_webp_metadata(&plain, Some(b"icc"), Some(b"exif"), Some(b"xmp"))
            .expect("inject");

        let rewritten = webp_bytes_satisfying_metadata_policy(&carrying, policy)
            .expect("a WebP container truss wrote is one it can rewrite");

        for fourcc in [b"ICCP", b"EXIF", b"XMP "] {
            assert_eq!(
                webp_chunk_payload(&rewritten, fourcc).is_some(),
                kept.contains(&fourcc),
                "{}",
                String::from_utf8_lossy(fourcc)
            );
        }
        let vp8x = webp_chunk_payload(&rewritten, b"VP8X").expect("VP8X chunk");
        // ICC (0x20) | EXIF (0x08) | XMP (0x04): a flag for a chunk that is gone would
        // make the file invalid.
        assert_eq!(
            vp8x[0] & 0x2C,
            kept.iter().fold(0u8, |flags, fourcc| flags
                | match *fourcc {
                    b"ICCP" => 0x20,
                    b"EXIF" => 0x08,
                    _ => 0x04,
                }),
        );
        assert!(rewritten.len() < carrying.len());
        image::load_from_memory(&rewritten).expect("the rewritten container still decodes");
    }

    /// A policy that keeps everything has nothing to drop, so the file is its own answer.
    #[test]
    fn webp_bytes_satisfying_metadata_policy_returns_a_kept_file_byte_for_byte() {
        use super::{inject_webp_metadata, webp_bytes_satisfying_metadata_policy};

        let plain = transform_raster(TransformRequest::new(
            png_artifact(9, 5, Rgba([10, 20, 30, 255])),
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(80),
                ..TransformOptions::default()
            },
        ))
        .expect("encode lossy webp")
        .artifact
        .bytes;
        let carrying = inject_webp_metadata(&plain, Some(b"icc"), Some(b"exif"), Some(b"xmp"))
            .expect("inject");

        assert_eq!(
            webp_bytes_satisfying_metadata_policy(&carrying, MetadataPolicy::KeepAll).as_deref(),
            Some(&carrying[..])
        );
        // And so does a file that carries none of the three, whatever the policy is.
        assert_eq!(
            webp_bytes_satisfying_metadata_policy(&plain, MetadataPolicy::StripAll).as_deref(),
            Some(&plain[..])
        );
    }

    #[test]
    fn inject_webp_metadata_promotes_a_simple_container_to_the_extended_format() {
        use super::inject_webp_metadata;

        let plain = transform_raster(TransformRequest::new(
            png_artifact(9, 5, Rgba([10, 20, 30, 255])),
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(80),
                ..TransformOptions::default()
            },
        ))
        .expect("encode lossy webp")
        .artifact
        .bytes;
        assert!(
            webp_chunk_payload(&plain, b"VP8X").is_none(),
            "libwebp should emit a simple container for this input"
        );

        let injected = inject_webp_metadata(&plain, Some(b"icc"), Some(b"exif"), Some(b"xmp"))
            .expect("inject");

        let vp8x = webp_chunk_payload(&injected, b"VP8X").expect("VP8X chunk");
        assert_eq!(vp8x.len(), 10);
        // ICC (0x20) | EXIF (0x08) | XMP (0x04)
        assert_eq!(vp8x[0] & 0x2C, 0x2C);
        assert_eq!(&vp8x[4..7], &[8, 0, 0], "canvas width minus one");
        assert_eq!(&vp8x[7..10], &[4, 0, 0], "canvas height minus one");

        assert_eq!(
            webp_chunk_payload(&injected, b"ICCP").as_deref(),
            Some(&b"icc"[..])
        );
        assert_eq!(
            webp_chunk_payload(&injected, b"EXIF").as_deref(),
            Some(&b"exif"[..])
        );
        assert_eq!(
            webp_chunk_payload(&injected, b"XMP ").as_deref(),
            Some(&b"xmp"[..])
        );

        let sniffed = sniff_artifact(RawArtifact::new(injected, None)).expect("sniff");
        assert_eq!(sniffed.metadata.width, Some(9));
        assert_eq!(sniffed.metadata.height, Some(5));
    }

    #[test]
    fn inject_webp_metadata_reuses_an_existing_vp8x_and_keeps_the_alpha_flag() {
        use super::inject_webp_metadata;

        // A transparent image makes libwebp emit an extended container (VP8X + ALPH),
        // so the injector must extend the existing header rather than build a new one.
        let transparent = transform_raster(TransformRequest::new(
            png_artifact(8, 8, Rgba([10, 20, 30, 128])),
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(80),
                ..TransformOptions::default()
            },
        ))
        .expect("encode lossy webp")
        .artifact
        .bytes;
        let before = webp_chunk_payload(&transparent, b"VP8X").expect("VP8X chunk");
        assert_eq!(before[0] & 0x10, 0x10, "libwebp should flag alpha");

        let injected =
            inject_webp_metadata(&transparent, Some(b"icc"), None, None).expect("inject");

        let after = webp_chunk_payload(&injected, b"VP8X").expect("VP8X chunk");
        assert_eq!(after[0] & 0x10, 0x10, "the alpha flag must survive");
        assert_eq!(after[0] & 0x20, 0x20, "the ICC flag must be set");
        assert_eq!(
            after[4..10],
            before[4..10],
            "the canvas size must not change"
        );
        assert_eq!(
            webp_chunk_payload(&injected, b"ICCP").as_deref(),
            Some(&b"icc"[..])
        );

        let sniffed = sniff_artifact(RawArtifact::new(injected, None)).expect("sniff");
        assert_eq!(sniffed.metadata.has_alpha, Some(true));
        assert_eq!(sniffed.metadata.width, Some(8));
    }

    #[test]
    fn inject_webp_metadata_is_a_no_op_without_payloads() {
        use super::inject_webp_metadata;

        let plain = transform_raster(TransformRequest::new(
            png_artifact(4, 4, Rgba([10, 20, 30, 255])),
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(80),
                ..TransformOptions::default()
            },
        ))
        .expect("encode lossy webp")
        .artifact
        .bytes;

        assert_eq!(
            inject_webp_metadata(&plain, None, None, None).expect("inject"),
            plain
        );
    }

    #[test]
    fn inject_webp_metadata_rejects_non_webp_bytes() {
        use super::inject_webp_metadata;

        let error = inject_webp_metadata(b"not a webp file", Some(b"icc"), None, None)
            .expect_err("must not rewrite foreign bytes");
        assert!(matches!(error, TransformError::EncodeFailed(_)));
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

    #[cfg(feature = "avif")]
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

    #[cfg(feature = "webp-lossy")]
    #[test]
    fn transform_raster_lossy_webp_carries_metadata_that_existed() {
        let artifact = webp_artifact_with_metadata(4, 2, None, Some(b"demo-icc-profile"));
        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                strip_metadata: false,
                quality: Some(80),
                ..TransformOptions::default()
            },
        ))
        .expect("lossy webp quality encode should succeed");

        assert_eq!(
            webp_icc_profile(&result.artifact.bytes).as_deref(),
            Some(b"demo-icc-profile".as_slice())
        );
        assert!(
            result.warnings.is_empty(),
            "nothing was dropped, got: {:?}",
            result.warnings
        );
    }

    #[cfg(feature = "avif")]
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

    #[cfg(feature = "avif")]
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

    /// A sample at the top of its range must round to 255, not past it.
    #[cfg(feature = "avif")]
    #[rstest]
    #[case::ten_bit_max(1023, 2, 255)]
    #[case::twelve_bit_max(4095, 4, 255)]
    #[case::ten_bit_rounds_down(117, 2, 29)]
    #[case::ten_bit_rounds_up(118, 2, 30)]
    #[case::twelve_bit_zero(0, 4, 0)]
    fn narrow_sample_rounds_to_nearest_within_eight_bits(
        #[case] value: u16,
        #[case] shift: u8,
        #[case] expected: u8,
    ) {
        assert_eq!(super::narrow_sample(value, shift), expected);
    }

    /// The `image` crate writes 8-bit AVIF only, so the deep path is exercised by two files
    /// ImageMagick wrote: a blue left half, a red right half, and a white bar along the top,
    /// which are the three saturated samples that used to wrap to zero.
    #[cfg(feature = "avif")]
    #[rstest]
    #[case::ten_bit(include_bytes!("../../integration/fixtures/deep-10bit.avif"))]
    #[case::twelve_bit(include_bytes!("../../integration/fixtures/deep-12bit.avif"))]
    fn transform_raster_decodes_deep_avif_without_wrapping_saturated_samples(#[case] bytes: &[u8]) {
        let artifact = sniff_artifact(RawArtifact::new(bytes.to_vec(), None)).expect("sniff avif");

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("decode deep avif");

        let image = image::load_from_memory(&result.artifact.bytes)
            .expect("decode png")
            .to_rgb8();
        for ((x, y), expected) in [
            ((2, 10), [0, 0, 255]),
            ((30, 10), [255, 0, 0]),
            ((20, 1), [255, 255, 255]),
        ] {
            let pixel = image.get_pixel(x, y).0;
            assert!(
                pixel
                    .iter()
                    .zip(expected)
                    .all(|(got, want)| got.abs_diff(want) < 16),
                "pixel ({x}, {y}) should be near {expected:?}, got {pixel:?}"
            );
        }
    }

    #[cfg(feature = "avif")]
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

    #[cfg(feature = "avif")]
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
                orientation: None,
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
        check_output_pixel_limit(&image, Some(8192), Some(8192), None, false).unwrap();
    }

    #[test]
    fn output_pixel_limit_rejects_oversized() {
        use super::check_output_pixel_limit;
        // 8193 * 8192 = 67_117_056 > MAX_OUTPUT_PIXELS (67_108_864)
        let image =
            image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 255])));
        let err =
            check_output_pixel_limit(&image, Some(8193), Some(8192), None, false).unwrap_err();
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

    // Regression tests for https://github.com/nao1215/truss/issues/253: every decoded image
    // was widened to RGBA8 before encoding, so an opaque RGB PNG came back RGBA — a larger
    // file, and a same-format pass that silently changed the color model.
    #[test]
    fn transform_raster_keeps_opaque_rgb_png_without_alpha() {
        let input = opaque_rgb_png_artifact(16, 16);
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(
            !encoded_png_has_alpha(&result.artifact.bytes),
            "a no-op same-format pass must not add an alpha channel"
        );
        assert_eq!(result.artifact.metadata.has_alpha, Some(false));
        let round_tripped =
            sniff_artifact(RawArtifact::new(result.artifact.bytes, None)).expect("sniff output");
        assert_eq!(round_tripped.metadata.has_alpha, Some(false));
    }

    #[test]
    fn transform_raster_keeps_opaque_rgb_png_without_alpha_after_resize() {
        let input = opaque_rgb_png_artifact(16, 16);
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(8),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(!encoded_png_has_alpha(&result.artifact.bytes));
        assert_eq!(result.artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn transform_raster_keeps_alpha_for_a_transparent_png() {
        let input = png_artifact(4, 4, Rgba([10, 20, 30, 128]));
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(
            encoded_png_has_alpha(&result.artifact.bytes),
            "transparency must survive the round trip"
        );
        assert_eq!(result.artifact.metadata.has_alpha, Some(true));
    }

    #[test]
    fn transform_raster_narrows_a_fully_opaque_rgba_png_to_rgb() {
        // Dropping an all-opaque alpha channel is pixel-lossless and shrinks the output.
        let input = png_artifact(4, 4, Rgba([10, 20, 30, 255]));
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(!encoded_png_has_alpha(&result.artifact.bytes));
        assert_eq!(result.artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn transform_raster_adds_alpha_when_contain_padding_is_transparent() {
        // Padding an opaque image into a wider box with the default transparent fill is a
        // case where the operation genuinely requires an alpha channel.
        let input = opaque_rgb_png_artifact(8, 4);
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(16),
                height: Some(16),
                fit: Some(Fit::Contain),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(encoded_png_has_alpha(&result.artifact.bytes));
        assert_eq!(result.artifact.metadata.has_alpha, Some(true));
    }

    #[test]
    fn transform_raster_keeps_opaque_output_when_contain_padding_is_opaque() {
        let input = opaque_rgb_png_artifact(8, 4);
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(16),
                height: Some(16),
                fit: Some(Fit::Contain),
                background: Some(Rgba8 {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                }),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(!encoded_png_has_alpha(&result.artifact.bytes));
        assert_eq!(result.artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn transform_raster_keeps_opaque_bmp_output_without_alpha() {
        let input = opaque_rgb_png_artifact(8, 8);
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Bmp),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert_eq!(result.artifact.metadata.has_alpha, Some(false));
        let round_tripped =
            sniff_artifact(RawArtifact::new(result.artifact.bytes, None)).expect("sniff output");
        assert_eq!(round_tripped.metadata.has_alpha, Some(false));
    }

    #[test]
    fn transform_raster_keeps_opaque_webp_output_without_alpha() {
        let input = opaque_rgb_png_artifact(8, 8);
        let result = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                format: Some(MediaType::Webp),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert_eq!(result.artifact.metadata.has_alpha, Some(false));
        let round_tripped =
            sniff_artifact(RawArtifact::new(result.artifact.bytes, None)).expect("sniff output");
        assert_eq!(round_tripped.metadata.has_alpha, Some(false));
    }

    // Regression test for https://github.com/nao1215/truss/issues/252: the limit was
    // computed against the *source* height when only --width was given, so a single
    // oversized dimension slipped through and allocated a gigapixel buffer.
    #[test]
    fn output_pixel_limit_uses_aspect_scaled_height_when_only_width_is_given() {
        use super::check_output_pixel_limit;

        let image =
            image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 255])));
        // A square source scaled to width 10000 also becomes 10000 tall: 100 Mpx > 67_108_864.
        let err = check_output_pixel_limit(&image, Some(10000), None, None, false).unwrap_err();
        assert!(matches!(err, TransformError::LimitExceeded(_)));
        assert!(err.to_string().contains("100000000"));
    }

    #[test]
    fn output_pixel_limit_uses_aspect_scaled_width_when_only_height_is_given() {
        use super::check_output_pixel_limit;

        let image =
            image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 255])));
        let err = check_output_pixel_limit(&image, None, Some(10000), None, false).unwrap_err();
        assert!(matches!(err, TransformError::LimitExceeded(_)));
        assert!(err.to_string().contains("100000000"));
    }

    #[test]
    fn output_pixel_limit_accepts_aspect_scaled_single_dimension_within_limit() {
        use super::check_output_pixel_limit;

        // A 4:1 source scaled to width 8192 becomes 8192x2048 = 16_777_216 pixels.
        let image =
            image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(16, 4, Rgba([0, 0, 0, 255])));
        check_output_pixel_limit(&image, Some(8192), None, None, false).unwrap();
    }

    #[test]
    fn transform_rejects_oversized_output_from_width_only_resize() {
        let input = png_artifact(16, 16, Rgba([10, 20, 30, 255]));
        let err = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                width: Some(10000),
                ..TransformOptions::default()
            },
        ))
        .unwrap_err();
        assert!(matches!(err, TransformError::LimitExceeded(_)));
        assert!(err.to_string().contains("output image"));
    }

    #[test]
    fn transform_rejects_oversized_output_from_height_only_resize() {
        let input = png_artifact(16, 16, Rgba([10, 20, 30, 255]));
        let err = transform_raster(TransformRequest::new(
            input,
            TransformOptions {
                height: Some(10000),
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
    fn keep_metadata_retains_xmp_but_drops_iptc_for_webp_output() {
        use super::extract_retained_metadata;
        use crate::core::{MetadataKind, TransformWarning};

        let artifact = jpeg_with_xmp_iptc();

        let (retained, warnings) =
            extract_retained_metadata(&artifact, MetadataPolicy::KeepAll, false, MediaType::Webp)
                .expect("should not error");

        // XMP rides in an `XMP ` RIFF chunk; IPTC has no WebP container chunk.
        let metadata = retained.expect("metadata should be retained");
        assert!(metadata.xmp_metadata.is_some());
        assert!(metadata.iptc_metadata.is_none());
        assert_eq!(
            warnings,
            vec![TransformWarning::MetadataDropped(MetadataKind::Iptc)]
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

    #[rstest]
    #[case::a_thumbnail_is_unchanged(200 * 200, false, 4)]
    #[case::at_the_first_step(2_000_000, false, 4)]
    #[case::just_past_it(2_000_001, false, 8)]
    #[case::at_the_second_step(16_000_000, false, 8)]
    #[case::just_past_that(16_000_001, false, 10)]
    #[case::the_output_ceiling(crate::MAX_OUTPUT_PIXELS, false, 10)]
    #[case::optimize_below_the_first_step_is_unchanged(200 * 200, true, 2)]
    #[case::optimize_at_the_ceiling(crate::MAX_OUTPUT_PIXELS, true, 8)]
    fn avif_speed_climbs_with_the_output_size(
        #[case] pixels: u64,
        #[case] optimized: bool,
        #[case] expected: u8,
    ) {
        assert_eq!(super::avif_speed(pixels, optimized), expected);
    }

    #[test]
    fn avif_speed_never_leaves_the_encoder_range() {
        // rav1e takes 1 through 10. The optimizing path subtracts from the step it lands on,
        // so the lowest step and the subtraction have to be read together.
        for pixels in [
            0,
            1,
            2_000_000,
            2_000_001,
            16_000_000,
            crate::MAX_OUTPUT_PIXELS,
        ] {
            for optimized in [false, true] {
                let speed = super::avif_speed(pixels, optimized);
                assert!(
                    (1..=10).contains(&speed),
                    "speed {speed} for {pixels} pixels is outside what rav1e takes"
                );
            }
        }
    }

    #[test]
    fn the_avif_feature_turns_on_the_encoder_thread_pool() {
        // `image`'s AVIF encoder reaches rav1e through `maybe-rayon`, which is a set of
        // no-op shims until `ravif/threading` is on, and `image/rayon` is what turns that
        // on. Without it the encoder runs on one core: a 4000x3000 photo took 112 seconds
        // rather than 10, for byte-identical output, which is long enough to run past the
        // server's transform deadline and hold a worker while it does. Nothing in the
        // encode path can assert this at run time, so the feature list is what is checked.
        let manifest = include_str!("../../Cargo.toml");
        let avif_feature = manifest
            .lines()
            .find(|line| line.starts_with("avif = "))
            .expect("the manifest declares an avif feature");
        assert!(
            avif_feature.contains("\"image/rayon\""),
            "the avif feature must keep image/rayon: {avif_feature}"
        );
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
                orientation: None,
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

    // ── resize: fit modes and the enlargement policy ──────────────────────────
    //
    // These drive `resolved_output_dimensions`, which is both what the pipeline resizes to
    // and what the output pixel limit measures, so a divergence between the reported and
    // the produced size cannot hide. The pixel-level consequences (padding, cropping) are
    // asserted separately below through `transform_raster`.

    fn output_size(
        source: (u32, u32),
        width: Option<u32>,
        height: Option<u32>,
        fit: Option<Fit>,
        without_enlargement: bool,
    ) -> (u32, u32) {
        resolved_output_dimensions(source, width, height, fit, without_enlargement)
    }

    #[test]
    fn inside_fits_a_landscape_image_without_padding() {
        // The case from https://github.com/nao1215/truss/issues/312: 640x427 bounded by
        // 200x200 is 200x133, not a 200x200 letterbox.
        assert_eq!(
            output_size((640, 427), Some(200), Some(200), Some(Fit::Inside), false),
            (200, 133)
        );
    }

    #[test]
    fn inside_fits_a_portrait_image_within_both_bounds() {
        let (w, h) = output_size((300, 900), Some(200), Some(200), Some(Fit::Inside), false);
        assert!(
            w <= 200 && h <= 200,
            "inside must not exceed either bound, got {w}x{h}"
        );
        // The constrained axis reaches the bound exactly; the other follows the ratio.
        assert_eq!((w, h), (67, 200));
    }

    #[test]
    fn contain_always_reports_the_requested_box() {
        // Same source and box as the inside case: contain pads that 200x133 out to 200x200.
        assert_eq!(
            output_size((640, 427), Some(200), Some(200), Some(Fit::Contain), false),
            (200, 200)
        );
        // Contain is the default when no fit is named.
        assert_eq!(
            output_size((640, 427), Some(200), Some(200), None, false),
            (200, 200)
        );
    }

    #[test]
    fn cover_and_fill_still_report_the_requested_box() {
        assert_eq!(
            output_size((640, 427), Some(200), Some(200), Some(Fit::Cover), false),
            (200, 200)
        );
        assert_eq!(
            output_size((640, 427), Some(200), Some(200), Some(Fit::Fill), false),
            (200, 200)
        );
    }

    #[test]
    fn enlargement_is_allowed_by_default() {
        for fit in [Fit::Inside, Fit::Contain, Fit::Cover, Fit::Fill] {
            assert_eq!(
                output_size((16, 16), Some(200), Some(200), Some(fit), false),
                (200, 200),
                "{fit:?} should enlarge a small source when not told otherwise"
            );
        }
        // A single-axis request enlarges too.
        assert_eq!(
            output_size((16, 16), Some(200), None, None, false),
            (200, 200)
        );
    }

    #[test]
    fn without_enlargement_stops_a_small_source_from_growing() {
        // Inside, cover, and fill report the content size, so all three stay at the source.
        for fit in [Fit::Inside, Fit::Cover, Fit::Fill] {
            assert_eq!(
                output_size((16, 16), Some(200), Some(200), Some(fit), true),
                (16, 16),
                "{fit:?} should leave a smaller source alone"
            );
        }
        // Contain still pads out to the requested box; only the content stops growing.
        assert_eq!(
            output_size((16, 16), Some(200), Some(200), Some(Fit::Contain), true),
            (200, 200)
        );
        // Single-axis requests honour it on both axes.
        assert_eq!(output_size((16, 16), Some(200), None, None, true), (16, 16));
        assert_eq!(output_size((16, 16), None, Some(200), None, true), (16, 16));
    }

    /// A source smaller than the box on one axis only, which is where `cover` surprises.
    ///
    /// The mode returns the box intersected with the source, so the output can have a
    /// ratio the source never had: 100x50 asked for 200x40 comes back 100x40. That is what
    /// `docs/pipeline.md` describes, against a table that says `cover` is exactly the box.
    #[test]
    fn cover_without_enlargement_returns_the_box_intersected_with_the_source() {
        assert_eq!(
            output_size((100, 50), Some(200), Some(40), Some(Fit::Cover), true),
            (100, 40)
        );
        assert_eq!(
            output_size((100, 50), Some(40), Some(200), Some(Fit::Cover), true),
            (40, 50)
        );
        // The box is reached on both axes when the source is large enough for it.
        assert_eq!(
            output_size((300, 100), Some(200), Some(40), Some(Fit::Cover), true),
            (200, 40)
        );
        // Contain answers the same three requests with the box, which is the contrast.
        for (w, h) in [(200, 40), (40, 200)] {
            assert_eq!(
                output_size((100, 50), Some(w), Some(h), Some(Fit::Contain), true),
                (w, h)
            );
        }
    }

    #[test]
    fn without_enlargement_does_not_shrink_what_already_fits() {
        // A source larger than the box is still reduced; the flag only removes upscaling.
        assert_eq!(
            output_size((640, 427), Some(200), Some(200), Some(Fit::Inside), true),
            (200, 133)
        );
        assert_eq!(
            output_size((640, 427), Some(200), None, None, true),
            (200, 133)
        );
    }

    #[test]
    fn single_axis_resize_derives_the_other_from_the_aspect_ratio() {
        assert_eq!(
            output_size((640, 427), Some(200), None, None, false),
            (200, 133)
        );
        assert_eq!(
            output_size((640, 427), None, Some(200), None, false),
            (300, 200)
        );
        // No axis at all is a passthrough, whatever the flags say.
        assert_eq!(output_size((640, 427), None, None, None, true), (640, 427));
    }

    #[test]
    fn transform_raster_inside_adds_no_padding() {
        // 16x8 solid red. Bounded by 8x8, inside gives 8x4 with no transparent bars; contain
        // gives 8x8 with them. Asserting the pixels is what separates the two modes, since
        // both agree the content is 8x4.
        let image = RgbaImage::from_pixel(16, 8, Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 16, 8, ColorType::Rgba8.into())
            .expect("encode png");

        let run = |fit: Fit| -> RgbaImage {
            let artifact = Artifact::new(
                bytes.clone(),
                MediaType::Png,
                ArtifactMetadata {
                    width: Some(16),
                    height: Some(8),
                    frame_count: 1,
                    duration: None,
                    has_alpha: Some(false),
                    orientation: None,
                },
            );
            let result = transform_raster(TransformRequest::new(
                artifact,
                TransformOptions {
                    width: Some(8),
                    height: Some(8),
                    fit: Some(fit),
                    ..TransformOptions::default()
                },
            ))
            .expect("resize");
            image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
                .expect("decode output")
                .to_rgba8()
        };

        let inside = run(Fit::Inside);
        assert_eq!(inside.dimensions(), (8, 4));
        assert!(
            inside.pixels().all(|pixel| pixel[3] == 255),
            "inside must not introduce transparent padding"
        );

        let contain = run(Fit::Contain);
        assert_eq!(contain.dimensions(), (8, 8));
        assert_eq!(
            contain.get_pixel(0, 0)[3],
            0,
            "contain pads the difference in aspect ratio"
        );
    }

    #[test]
    fn transform_raster_without_enlargement_leaves_a_small_source_untouched() {
        let mut image = RgbaImage::from_pixel(4, 4, Rgba([0, 0, 255, 255]));
        image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 4, 4, ColorType::Rgba8.into())
            .expect("encode png");
        let artifact = Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(4),
                height: Some(4),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
                orientation: None,
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                width: Some(64),
                height: Some(64),
                fit: Some(Fit::Inside),
                without_enlargement: true,
                ..TransformOptions::default()
            },
        ))
        .expect("resize");

        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output")
            .to_rgba8();
        assert_eq!(output.dimensions(), (4, 4));
        // Skipping the resize entirely also means the pixels are untouched, not resampled
        // back to the same size.
        assert_eq!(*output.get_pixel(0, 0), Rgba([255, 0, 0, 255]));
        assert_eq!(*output.get_pixel(3, 3), Rgba([0, 0, 255, 255]));
    }

    #[test]
    fn rotated_bounding_box_grows_to_hold_the_whole_image() {
        // A quarter turn just swaps the axes.
        assert_eq!(rotated_bounding_box(16, 8, 90), (8, 16));
        assert_eq!(rotated_bounding_box(16, 8, 180), (16, 8));
        // 45 degrees needs sqrt(2)/2 of each side on both axes: (16+8)*0.7071 = 16.97.
        assert_eq!(rotated_bounding_box(16, 8, 45), (17, 17));
        // A degenerate 1-pixel input never collapses to zero.
        assert_eq!(rotated_bounding_box(1, 1, 45), (2, 2));
    }

    #[test]
    fn rotation_pixel_limit_is_checked_before_allocation() {
        // The input budget is larger than the output one, so a legal input can rotate into
        // an illegal output. This is dimensions-only so the test never allocates.
        let error = check_rotated_pixel_limit(9_000, 9_000, 45)
            .expect_err("a 45 degree turn of 9000x9000 should exceed the output budget");

        match error {
            TransformError::LimitExceeded(message) => {
                assert!(
                    message.contains("rotating 9000x9000 by 45 degrees"),
                    "the error should name the request, got: {message}"
                );
            }
            other => panic!("expected LimitExceeded, got: {other}"),
        }

        // The same angle is fine on a source small enough that the grown canvas still fits.
        assert_eq!(
            check_rotated_pixel_limit(4_000, 4_000, 45).expect("a smaller source fits"),
            (5_657, 5_657)
        );
    }

    #[test]
    fn transform_raster_rotates_by_an_arbitrary_angle() {
        // 16x8, red on the left half and blue on the right.
        let mut image = RgbaImage::from_pixel(16, 8, Rgba([0, 0, 255, 255]));
        for y in 0..8 {
            for x in 0..8 {
                image.put_pixel(x, y, Rgba([255, 0, 0, 255]));
            }
        }
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 16, 8, ColorType::Rgba8.into())
            .expect("encode png");
        let artifact = Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(16),
                height: Some(8),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
                orientation: None,
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                rotate: Rotation::from_degrees(45),
                ..TransformOptions::default()
            },
        ))
        .expect("45 degree rotation");

        assert_eq!(result.artifact.metadata.width, Some(17));
        assert_eq!(result.artifact.metadata.height, Some(17));

        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output")
            .to_rgba8();

        // Clockwise: the left half swings up and to the left, the right half down and right.
        // Getting the sign wrong would swap these two, which a size assertion cannot catch.
        let upper_left = output.get_pixel(4, 4);
        let lower_right = output.get_pixel(12, 12);
        assert!(
            upper_left[0] > upper_left[2],
            "the left half should land upper-left, got {upper_left:?}"
        );
        assert!(
            lower_right[2] > lower_right[0],
            "the right half should land lower-right, got {lower_right:?}"
        );
        // The exposed corner takes the default background, which is transparent for PNG.
        assert_eq!(output.get_pixel(0, 0)[3], 0, "corner should be transparent");
    }

    #[test]
    fn arbitrary_rotation_fills_corners_with_the_requested_background() {
        let image = RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255]));
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
                orientation: None,
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                rotate: Rotation::from_degrees(30),
                background: Some(Rgba8 {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
                ..TransformOptions::default()
            },
        ))
        .expect("30 degree rotation with a background");

        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output")
            .to_rgba8();
        assert_eq!(
            *output.get_pixel(0, 0),
            Rgba([255, 0, 0, 255]),
            "the exposed corner should take the requested background"
        );
    }

    #[test]
    fn quarter_turns_stay_pixel_exact() {
        // Two 90 degree turns must equal one 180 degree turn to the byte. If the quarter
        // turn ever fell through to the resampling path this would drift.
        let mut image = RgbaImage::from_pixel(6, 4, Rgba([0, 0, 255, 255]));
        image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        image.put_pixel(5, 3, Rgba([0, 255, 0, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 6, 4, ColorType::Rgba8.into())
            .expect("encode png");

        let rotate = |degrees: i32, source: Vec<u8>| -> Vec<u8> {
            let artifact = Artifact::new(
                source,
                MediaType::Png,
                ArtifactMetadata {
                    width: None,
                    height: None,
                    frame_count: 1,
                    duration: None,
                    has_alpha: Some(false),
                    orientation: None,
                },
            );
            transform_raster(TransformRequest::new(
                artifact,
                TransformOptions {
                    rotate: Rotation::from_degrees(degrees),
                    ..TransformOptions::default()
                },
            ))
            .expect("rotate")
            .artifact
            .bytes
        };

        let twice = rotate(90, rotate(90, bytes.clone()));
        let once = rotate(180, bytes);
        assert_eq!(twice, once, "90 + 90 must equal 180 exactly");
    }

    #[test]
    fn negative_and_wrapped_rotations_agree() {
        let image = RgbaImage::from_pixel(5, 3, Rgba([12, 34, 56, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 5, 3, ColorType::Rgba8.into())
            .expect("encode png");

        let rotate = |degrees: i32| -> Vec<u8> {
            let artifact = Artifact::new(
                bytes.clone(),
                MediaType::Png,
                ArtifactMetadata {
                    width: None,
                    height: None,
                    frame_count: 1,
                    duration: None,
                    has_alpha: Some(false),
                    orientation: None,
                },
            );
            transform_raster(TransformRequest::new(
                artifact,
                TransformOptions {
                    rotate: Rotation::from_degrees(degrees),
                    ..TransformOptions::default()
                },
            ))
            .expect("rotate")
            .artifact
            .bytes
        };

        assert_eq!(rotate(-90), rotate(270));
        assert_eq!(rotate(370), rotate(10));
        assert_eq!(rotate(-360), rotate(0));
    }

    #[test]
    fn transform_raster_applies_grayscale() {
        // Distinct hues so a channel-collapsing bug is visible: pure red and pure blue
        // have different luminance under Rec. 601, so they must not map to the same gray.
        let mut image = RgbaImage::from_pixel(2, 1, Rgba([255, 0, 0, 255]));
        image.put_pixel(1, 0, Rgba([0, 0, 255, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 2, 1, ColorType::Rgba8.into())
            .expect("encode png");
        let artifact = Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(2),
                height: Some(1),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
                orientation: None,
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                grayscale: true,
                ..TransformOptions::default()
            },
        ))
        .expect("grayscale transform");

        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output")
            .to_rgba8();

        for (x, pixel) in output.pixels().enumerate() {
            assert!(
                pixel[0] == pixel[1] && pixel[1] == pixel[2],
                "pixel {x} is not neutral gray: {pixel:?}"
            );
        }
        assert_ne!(
            output.get_pixel(0, 0)[0],
            output.get_pixel(1, 0)[0],
            "red and blue must map to different luminance values"
        );
    }

    #[test]
    fn transform_raster_grayscale_preserves_alpha() {
        let mut image = RgbaImage::from_pixel(2, 1, Rgba([255, 0, 0, 128]));
        image.put_pixel(1, 0, Rgba([0, 255, 0, 0]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 2, 1, ColorType::Rgba8.into())
            .expect("encode png");
        let artifact = Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(2),
                height: Some(1),
                frame_count: 1,
                duration: None,
                has_alpha: Some(true),
                orientation: None,
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                grayscale: true,
                ..TransformOptions::default()
            },
        ))
        .expect("grayscale transform");

        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output")
            .to_rgba8();
        assert_eq!(
            output.get_pixel(0, 0)[3],
            128,
            "alpha must survive grayscale"
        );
        assert_eq!(output.get_pixel(1, 0)[3], 0, "alpha must survive grayscale");
    }

    #[test]
    fn grayscale_disables_lossless_passthrough() {
        // A lossless PNG->PNG optimize with no other operation normally short-circuits
        // before the pipeline runs. grayscale must defeat that, or it is silently dropped.
        let mut image = RgbaImage::from_pixel(4, 4, Rgba([255, 0, 0, 255]));
        image.put_pixel(0, 0, Rgba([0, 0, 255, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 4, 4, ColorType::Rgba8.into())
            .expect("encode png");
        let artifact = Artifact::new(
            bytes,
            MediaType::Png,
            ArtifactMetadata {
                width: Some(4),
                height: Some(4),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
                orientation: None,
            },
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                grayscale: true,
                optimize: OptimizeMode::Lossless,
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("grayscale + lossless transform");

        let output = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output")
            .to_rgba8();
        let pixel = output.get_pixel(1, 1);
        assert!(
            pixel[0] == pixel[1] && pixel[1] == pixel[2],
            "lossless passthrough must not skip grayscale: {pixel:?}"
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
                orientation: None,
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
    fn a_watermark_that_only_fails_because_of_the_margin_says_so() {
        // A 2x2 mark fits a 4x4 output; a 4px margin on each side is what pushes it out.
        // The message used to blame the watermark image, sending the reader off to
        // shrink something that was already small enough.
        let main = png_artifact(4, 4, Rgba([255, 255, 255, 255]));
        let wm = png_artifact(2, 2, Rgba([0, 0, 0, 128]));

        let mut request = TransformRequest::new(main, TransformOptions::default());
        request.watermark = Some(WatermarkInput {
            image: wm,
            position: Position::BottomRight,
            opacity: 50,
            margin: 4,
        });

        let err = transform_raster(request).expect_err("the margin does not leave room");
        assert_eq!(
            err,
            TransformError::InvalidOptions(
                "watermark 2x2 with a 4px margin does not fit a 4x4 output".to_string()
            )
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
                "watermark 5x5 with a 0px margin does not fit a 4x4 output".to_string()
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
            matches!(err, TransformError::InvalidOptions(ref msg) if msg.contains("does not fit"))
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

    // ── optimize never returns more bytes than it was given ─────────────

    /// Builds a JPEG the encoder compresses worse than Pillow-style flat encoding does.
    /// A flat-colour JPEG produced by this crate's own encoder.
    fn flat_jpeg_artifact(width: u32, height: u32) -> Artifact {
        let image = image::RgbImage::from_pixel(width, height, image::Rgb([30, 80, 200]));
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 85)
            .write_image(&image, width, height, ColorType::Rgb8.into())
            .expect("encode jpeg");
        sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff jpeg")
    }

    fn optimize_bytes(artifact: Artifact, mode: OptimizeMode) -> Vec<u8> {
        let format = artifact.media_type;
        transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(format),
                optimize: mode,
                ..TransformOptions::default()
            },
        ))
        .expect("transform")
        .artifact
        .bytes
    }

    /// A flat JPEG re-encodes larger than it started, so every mode must hand back the input.
    ///
    /// `auto` used to compare two re-encodes and never the bytes it was given, so it
    /// returned a bigger file than `lossless` did, having paid a generation loss for it.
    /// `lossy` kept doing that after `auto` and `lossless` stopped, which made the command's
    /// name wrong for the mode a caller reaches for when they want the largest reduction.
    #[rstest]
    #[case(OptimizeMode::Auto)]
    #[case(OptimizeMode::Lossless)]
    #[case(OptimizeMode::Lossy)]
    fn optimization_never_returns_more_bytes_than_the_input(#[case] mode: OptimizeMode) {
        // The synthetic images are encoded by the same encoder that would re-encode them,
        // which is the easy half of the property. `flat.jpg` came from a different encoder
        // with optimized Huffman tables, which is what a file arriving from a design tool
        // or a phone looks like, and is where a re-encode costs more than it saves.
        let mut inputs = vec![
            sniff_artifact(RawArtifact::new(FLAT_JPEG.to_vec(), None)).expect("sniff flat.jpg"),
            // The same argument in the other container the passthrough can return. The
            // lossless WebP encoder reached through the image crate is a plain one, so a
            // picture libwebp compressed well comes back from a re-encode two and a half
            // times its size.
            sniff_artifact(RawArtifact::new(LIBWEBP_LOSSLESS.to_vec(), None))
                .expect("sniff libwebp-lossless.webp"),
        ];
        for size in [32, 64, 128, 256] {
            inputs.push(flat_jpeg_artifact(size, size));
        }

        for artifact in inputs {
            let dimensions = artifact.metadata.dimensions();
            let input_length = artifact.bytes.len();

            let optimized = optimize_bytes(artifact, mode);

            assert!(
                optimized.len() <= input_length,
                "{dimensions:?}: {mode:?} produced {} bytes from {input_length}",
                optimized.len()
            );
        }
    }

    /// Naming a quality is asking for that encode, so the passthrough stands aside.
    #[test]
    fn lossy_optimization_still_re_encodes_when_a_quality_is_named() {
        let artifact = flat_jpeg_artifact(128, 128);
        let input_length = artifact.bytes.len();

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Lossy,
                quality: Some(98),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(
            result.artifact.bytes.len() > input_length,
            "a named quality must produce the encoder's output, not the input"
        );
    }

    /// A picture no lossy encoder scores well on: every pixel is an independent draw from
    /// a fixed-seed generator, so PSNR stays low at any quality and a target above what the
    /// cap allows is a target that cannot be met.
    fn noisy_png_artifact(size: u32) -> Artifact {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        };
        let image = RgbaImage::from_fn(size, size, |_, _| Rgba([next(), next(), next(), 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, size, size, ColorType::Rgba8.into())
            .expect("encode png");
        sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff noisy png")
    }

    fn target_shortfall(warnings: &[TransformWarning]) -> Option<(f32, u8)> {
        warnings.iter().find_map(|warning| match warning {
            TransformWarning::TargetQualityNotReached {
                achieved, quality, ..
            } => Some((*achieved, *quality)),
            _ => None,
        })
    }

    /// A quality cap that stops the search short of the target is a shortfall the caller
    /// asked to be told about, since the cap and the target came from the same command.
    #[test]
    fn lossy_optimization_warns_when_the_quality_cap_stops_short_of_the_target() {
        let result = transform_raster(TransformRequest::new(
            noisy_png_artifact(64),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Lossy,
                quality: Some(5),
                target_quality: Some("psnr:60".parse().expect("target")),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        let (achieved, quality) = target_shortfall(&result.warnings).expect("a shortfall warning");
        assert!(
            achieved < 60.0,
            "achieved {achieved} should be below the target"
        );
        assert_eq!(quality, 5, "the search was capped at the named quality");
        assert_eq!(result.warnings.len(), 1, "{:?}", result.warnings);
    }

    /// With no cap the search may go to 100, and a target nothing reaches is reported at
    /// that quality. The resize keeps the input from being handed back as the answer.
    #[test]
    fn lossy_optimization_warns_when_no_quality_reaches_the_target() {
        let result = transform_raster(TransformRequest::new(
            noisy_png_artifact(64),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Lossy,
                width: Some(32),
                target_quality: Some("psnr:99".parse().expect("target")),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        let (achieved, quality) = target_shortfall(&result.warnings).expect("a shortfall warning");
        assert!(
            achieved < 99.0,
            "achieved {achieved} should be below the target"
        );
        assert_eq!(quality, 100);
    }

    /// `auto` keeps a named target even when meeting it costs more than the default encode:
    /// the baseline never looked at the target, so it may only win when no target was named.
    #[test]
    fn auto_optimization_keeps_a_named_target_over_a_smaller_baseline() {
        let encode = |optimize: OptimizeMode, target: Option<&str>| {
            transform_raster(TransformRequest::new(
                noisy_png_artifact(64),
                TransformOptions {
                    format: Some(MediaType::Jpeg),
                    optimize,
                    width: Some(32),
                    target_quality: target.map(|value| value.parse().expect("target")),
                    ..TransformOptions::default()
                },
            ))
            .expect("transform")
        };

        let untargeted = encode(OptimizeMode::Auto, None);
        let lossy = encode(OptimizeMode::Lossy, Some("psnr:45"));
        let auto = encode(OptimizeMode::Auto, Some("psnr:45"));

        assert!(
            lossy.artifact.bytes.len() > untargeted.artifact.bytes.len(),
            "the target must cost more than the default encode for this to say anything"
        );
        assert_eq!(
            auto.artifact.bytes.len(),
            lossy.artifact.bytes.len(),
            "auto with a named target should return the targeted encode"
        );
        assert!(
            target_shortfall(&auto.warnings).is_none(),
            "{:?}",
            auto.warnings
        );
    }

    /// A target the search does reach, the default target `auto` picks on its own, and a
    /// request the input passes through all say nothing: there is no shortfall in the
    /// first, no promise in the second, and a perfect score in the third.
    #[rstest]
    #[case::reached(OptimizeMode::Lossy, Some(5), Some("psnr:10"), None)]
    #[case::default_target(OptimizeMode::Auto, None, None, None)]
    #[case::passthrough(OptimizeMode::Lossy, None, Some("psnr:99"), Some(MediaType::Jpeg))]
    fn lossy_optimization_does_not_warn_without_a_shortfall_of_its_own(
        #[case] optimize: OptimizeMode,
        #[case] quality: Option<u8>,
        #[case] target: Option<&str>,
        #[case] passthrough_input: Option<MediaType>,
    ) {
        let artifact = match passthrough_input {
            Some(MediaType::Jpeg) => flat_jpeg_artifact(64, 64),
            _ => noisy_png_artifact(64),
        };
        let input_length = artifact.bytes.len();

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize,
                quality,
                target_quality: target.map(|value| value.parse().expect("target")),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        if passthrough_input.is_some() {
            assert_eq!(
                result.artifact.bytes.len(),
                input_length,
                "the input should have been handed back"
            );
        }
        assert!(
            target_shortfall(&result.warnings).is_none(),
            "unexpected shortfall warning: {:?}",
            result.warnings
        );
    }

    /// And never more than `lossless` would, which is the stronger statement of the same.
    #[test]
    fn auto_optimization_is_never_worse_than_lossless() {
        let artifact = flat_jpeg_artifact(128, 128);

        let auto = optimize_bytes(artifact.clone(), OptimizeMode::Auto);
        let lossless = optimize_bytes(artifact, OptimizeMode::Lossless);

        assert!(
            auto.len() <= lossless.len(),
            "auto produced {} bytes where lossless produced {}",
            auto.len(),
            lossless.len()
        );
    }

    /// The fallback must not disable the optimization for images that do compress.
    #[test]
    fn auto_optimization_still_re_encodes_when_that_is_smaller() {
        let image = image::RgbImage::from_fn(256, 256, |x, y| {
            image::Rgb([(x * 4 % 256) as u8, (y * 4 % 256) as u8, 128])
        });
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 95)
            .write_image(&image, 256, 256, ColorType::Rgb8.into())
            .expect("encode jpeg");
        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff jpeg");
        let input_length = artifact.bytes.len();

        let optimized = optimize_bytes(artifact, OptimizeMode::Auto);

        assert!(
            optimized.len() < input_length,
            "a quality-95 gradient should compress: {} from {input_length}",
            optimized.len()
        );
    }

    /// A request that changes pixels is not eligible, so the guard leaves it alone.
    #[test]
    fn the_passthrough_guard_does_not_apply_to_a_request_that_transforms() {
        let artifact = flat_jpeg_artifact(128, 128);

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Auto,
                width: Some(32),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert_eq!(result.artifact.metadata.width, Some(32));
    }

    /// An indexed PNG has no encoder here, so it comes back as truecolour and grows.
    /// Handing back the input keeps both the size and the colour model.
    #[test]
    fn optimizing_an_indexed_png_returns_it_unchanged() {
        let artifact =
            sniff_artifact(RawArtifact::new(indexed_png_bytes(), None)).expect("sniff indexed png");
        let input = artifact.bytes.clone();

        for mode in [OptimizeMode::Auto, OptimizeMode::Lossless] {
            let optimized = optimize_bytes(artifact.clone(), mode);
            assert_eq!(
                optimized, input,
                "{mode:?} should return the indexed PNG unchanged"
            );
        }
    }

    /// A PNG carrying metadata the policy removes cannot be handed back verbatim.
    #[test]
    fn a_png_with_metadata_to_strip_is_not_passed_through() {
        let mut bytes = indexed_png_bytes();
        insert_png_text_chunk(&mut bytes, &vec![b'x'; 400]);
        let artifact = sniff_artifact(RawArtifact::new(bytes.clone(), None)).expect("sniff png");

        let optimized = optimize_bytes(artifact, OptimizeMode::Lossless);

        assert_ne!(optimized, bytes, "the comment had to be removed");
        assert!(
            !optimized
                .windows(400)
                .any(|window| window.iter().all(|byte| *byte == b'x')),
            "the stripped comment survived"
        );
    }

    /// The rule the guard applies to a PNG, stated directly rather than through an encoder
    /// size race: a file already satisfying the policy can be handed back, and one carrying
    /// metadata the policy removes cannot.
    #[test]
    fn png_metadata_policy_decides_whether_the_input_can_be_handed_back() {
        let clean = indexed_png_bytes();
        for policy in [
            MetadataPolicy::StripAll,
            MetadataPolicy::KeepAll,
            MetadataPolicy::PreserveIcc,
            MetadataPolicy::PreserveExif,
        ] {
            assert_eq!(
                png_bytes_satisfying_metadata_policy(&clean, policy),
                Some(clean.as_slice()),
                "a PNG with no metadata satisfies {policy:?}"
            );
        }

        let mut with_text = indexed_png_bytes();
        insert_png_text_chunk(&mut with_text, b"a comment");
        assert_eq!(
            png_bytes_satisfying_metadata_policy(&with_text, MetadataPolicy::KeepAll),
            Some(with_text.as_slice()),
            "keeping everything is satisfied by anything"
        );
        for policy in [
            MetadataPolicy::StripAll,
            MetadataPolicy::PreserveIcc,
            MetadataPolicy::PreserveExif,
        ] {
            assert_eq!(
                png_bytes_satisfying_metadata_policy(&with_text, policy),
                None,
                "{policy:?} removes the text chunk, so the file has to be re-encoded"
            );
        }
    }

    /// A truncated PNG never reports itself as satisfying a policy, so a malformed input
    /// cannot be handed back in place of a re-encode that would have failed.
    #[test]
    fn png_metadata_policy_rejects_a_container_it_cannot_walk() {
        let mut truncated = indexed_png_bytes();
        truncated.truncate(truncated.len() - 20);

        assert_eq!(
            png_bytes_satisfying_metadata_policy(&truncated, MetadataPolicy::StripAll),
            None
        );
    }

    /// A 4-colour indexed PNG, written by hand so the test carries its own fixture.
    fn indexed_png_bytes() -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        // IHDR: 64x64, bit depth 8, colour type 3 (indexed).
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&64u32.to_be_bytes());
        ihdr.extend_from_slice(&64u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
        push_png_chunk(&mut bytes, b"IHDR", &ihdr);
        push_png_chunk(
            &mut bytes,
            b"PLTE",
            &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
        );

        // One scanline filter byte plus 64 indices, per row.
        let mut raw = Vec::new();
        for y in 0..64u32 {
            raw.push(0);
            for x in 0..64u32 {
                raw.push(((x / 16 + y / 16) % 4) as u8);
            }
        }
        // Best compression, the way a design tool's PNG-8 export is written: the point of
        // the fixture is a file the re-encode cannot beat.
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        std::io::Write::write_all(&mut encoder, &raw).expect("deflate");
        push_png_chunk(
            &mut bytes,
            b"IDAT",
            &encoder.finish().expect("finish deflate"),
        );
        push_png_chunk(&mut bytes, b"IEND", &[]);
        bytes
    }

    fn push_png_chunk(bytes: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(chunk_type);
        bytes.extend_from_slice(data);
        let mut crc_input = chunk_type.to_vec();
        crc_input.extend_from_slice(data);
        bytes.extend_from_slice(&png_crc32(&crc_input).to_be_bytes());
    }

    /// Inserts a `tEXt` chunk just before `IEND`.
    fn insert_png_text_chunk(bytes: &mut Vec<u8>, text: &[u8]) {
        let iend = bytes.len() - 12;
        let mut data = b"Comment\0".to_vec();
        data.extend_from_slice(text);
        let mut chunk = Vec::new();
        push_png_chunk(&mut chunk, b"tEXt", &data);
        bytes.splice(iend..iend, chunk);
    }

    fn png_crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    // ── Lossless optimization and the EXIF orientation tag ──────────────

    fn optimize_losslessly(
        artifact: Artifact,
        auto_orient: bool,
        strip_metadata: bool,
        preserve_exif: bool,
    ) -> Result<TransformResult, TransformError> {
        transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Lossless,
                auto_orient,
                strip_metadata,
                preserve_exif,
                ..TransformOptions::default()
            },
        ))
    }

    /// A JPEG whose orientation tag survives into the output optimizes losslessly.
    ///
    /// Rotating the pixels would need a decode and a re-encode, so it cannot happen here;
    /// it also does not need to. The stored pixels and the retained tag describe the same
    /// picture they described in the input.
    #[test]
    fn lossless_optimization_accepts_an_oriented_jpeg_when_the_tag_is_kept() {
        for (strip_metadata, preserve_exif) in [(false, false), (false, true)] {
            let result = optimize_losslessly(
                jpeg_artifact_with_metadata(40, 20, Some(6), None),
                true,
                strip_metadata,
                preserve_exif,
            )
            .expect("the tag survives, so nothing has to be rotated");

            let output = sniff_artifact(RawArtifact::new(result.artifact.bytes, None))
                .expect("sniff output");
            assert_eq!(
                output.metadata.orientation,
                Some(6),
                "the output must still carry the tag, or it displays rotated"
            );
            assert!(
                result.warnings.is_empty(),
                "nothing was dropped: {:?}",
                result.warnings
            );
        }
    }

    /// With the metadata stripped there is nowhere for the orientation to go, so the
    /// request is refused — and the message says what is in the way.
    #[test]
    fn lossless_optimization_refuses_an_oriented_jpeg_when_the_tag_is_stripped() {
        let error = optimize_losslessly(
            jpeg_artifact_with_metadata(40, 20, Some(6), None),
            true,
            true,
            false,
        )
        .expect_err("the orientation cannot be applied losslessly or preserved");

        let TransformError::CapabilityMissing(message) = error else {
            panic!("unexpected error: {error}");
        };
        assert!(
            message.contains("EXIF orientation (6)"),
            "the message should name the orientation: {message}"
        );
        assert!(
            !message.contains("no pixel transforms are applied"),
            "the generic message blames a transform the caller did not ask for: {message}"
        );
    }

    /// The cases that already worked keep working under every metadata policy.
    #[test]
    fn lossless_optimization_accepts_a_jpeg_with_no_orientation_to_apply() {
        for orientation in [None, Some(1)] {
            for (strip_metadata, preserve_exif) in [(true, false), (false, false), (false, true)] {
                let result = optimize_losslessly(
                    jpeg_artifact_with_metadata(40, 20, orientation, None),
                    true,
                    strip_metadata,
                    preserve_exif,
                )
                .unwrap_or_else(|error| {
                    panic!("orientation {orientation:?} should optimize: {error}")
                });
                assert!(result.warnings.is_empty(), "{:?}", result.warnings);
            }
        }
    }

    /// A real pixel transform is still refused, with the message that describes it.
    #[test]
    fn lossless_optimization_still_refuses_an_actual_pixel_transform() {
        let error = transform_raster(TransformRequest::new(
            jpeg_artifact_with_metadata(40, 20, None, None),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                optimize: OptimizeMode::Lossless,
                width: Some(20),
                ..TransformOptions::default()
            },
        ))
        .expect_err("a resize is not lossless");

        assert!(
            matches!(error, TransformError::CapabilityMissing(ref message) if message.contains("no pixel transforms are applied")),
            "unexpected error: {error}"
        );
    }

    // ── Silently dropping an EXIF orientation ───────────────────────────

    /// Leaving the pixels as stored while stripping the tag that says how to display them
    /// is a rotation. Each flag is honest on its own; the combination has to say so.
    #[test]
    fn dropping_the_orientation_with_the_pixels_as_stored_warns() {
        let result = transform_raster(TransformRequest::new(
            jpeg_artifact_with_metadata(40, 20, Some(6), None),
            TransformOptions {
                format: Some(MediaType::Png),
                auto_orient: false,
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(
            result
                .warnings
                .contains(&TransformWarning::OrientationDropped { orientation: 6 }),
            "expected an orientation warning, got {:?}",
            result.warnings
        );
        // The output really is the stored size, which is what the warning is about.
        assert_eq!(result.artifact.metadata.width, Some(40));
        assert_eq!(result.artifact.metadata.height, Some(20));
    }

    /// Keeping the metadata keeps the tag, so there is nothing to warn about.
    #[test]
    fn dropping_the_orientation_does_not_warn_when_the_tag_is_kept() {
        let result = transform_raster(TransformRequest::new(
            jpeg_artifact_with_metadata(40, 20, Some(6), None),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                auto_orient: false,
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(
            !result
                .warnings
                .iter()
                .any(|warning| matches!(warning, TransformWarning::OrientationDropped { .. })),
            "unexpected warning: {:?}",
            result.warnings
        );
    }

    /// Orientation 1 and no tag at all record nothing, so nothing is lost.
    #[test]
    fn dropping_the_orientation_does_not_warn_when_there_is_nothing_to_drop() {
        for orientation in [None, Some(1)] {
            let result = transform_raster(TransformRequest::new(
                jpeg_artifact_with_metadata(40, 20, orientation, None),
                TransformOptions {
                    format: Some(MediaType::Png),
                    auto_orient: false,
                    ..TransformOptions::default()
                },
            ))
            .expect("transform");

            assert!(
                result.warnings.is_empty(),
                "orientation {orientation:?} produced {:?}",
                result.warnings
            );
        }
    }

    /// A PNG has no orientation in this pipeline, so it never warns.
    #[test]
    fn dropping_the_orientation_does_not_warn_for_a_png_input() {
        let result = transform_raster(TransformRequest::new(
            png_artifact(4, 2, Rgba([255, 0, 0, 255])),
            TransformOptions {
                format: Some(MediaType::Png),
                auto_orient: false,
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    /// The lossless passthrough returns early, so it needs the warning of its own.
    #[test]
    fn the_lossless_passthrough_warns_about_a_dropped_orientation() {
        let result = optimize_losslessly(
            jpeg_artifact_with_metadata(40, 20, Some(8), None),
            false,
            true,
            false,
        )
        .expect("no orientation is applied, so the passthrough is allowed");

        assert!(
            result
                .warnings
                .contains(&TransformWarning::OrientationDropped { orientation: 8 }),
            "expected an orientation warning, got {:?}",
            result.warnings
        );
    }

    // ── Declared pipeline order ─────────────────────────────────────────

    /// Rotation runs before the crop, so the crop box is checked against the rotated size.
    ///
    /// `docs/pipeline.md` states this order. A caller composing operations of its own
    /// depends on it: truss applies its options in a fixed order whatever order they were
    /// given in, so a chain needing a different one has to be split across invocations.
    #[test]
    fn the_pipeline_rotates_before_it_crops() {
        // A 2x4 crop does not fit the 4x2 source, and does fit it once rotated.
        let result = transform_raster(TransformRequest::new(
            png_artifact(4, 2, Rgba([255, 0, 0, 255])),
            TransformOptions {
                format: Some(MediaType::Png),
                rotate: Rotation::from_degrees(90),
                crop: Some(CropRegion {
                    x: 0,
                    y: 0,
                    width: 2,
                    height: 4,
                }),
                ..TransformOptions::default()
            },
        ))
        .expect("the crop fits the rotated image");

        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(4));
    }

    /// The crop runs before the resize, so a single-axis resize scales the cropped region.
    #[test]
    fn the_pipeline_crops_before_it_resizes() {
        // 8x4 cropped to a 4x4 square, then widened to 8: crop-first is 8x8. A resize
        // running first would produce 8x4 and then crop to 4x4.
        let result = transform_raster(TransformRequest::new(
            png_artifact(8, 4, Rgba([255, 0, 0, 255])),
            TransformOptions {
                format: Some(MediaType::Png),
                crop: Some(CropRegion {
                    x: 0,
                    y: 0,
                    width: 4,
                    height: 4,
                }),
                width: Some(8),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert_eq!(result.artifact.metadata.width, Some(8));
        assert_eq!(result.artifact.metadata.height, Some(8));
    }

    /// The watermark is composited after the resize, so it keeps its own size.
    #[test]
    fn the_pipeline_resizes_before_it_watermarks() {
        let watermark = sniff_artifact(RawArtifact::new(
            png_artifact(4, 4, Rgba([0, 0, 255, 255])).bytes,
            None,
        ))
        .expect("sniff watermark");

        let result = transform_raster(TransformRequest::with_watermark(
            png_artifact(8, 8, Rgba([255, 0, 0, 255])),
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(64),
                height: Some(64),
                fit: Some(Fit::Fill),
                ..TransformOptions::default()
            },
            WatermarkInput {
                image: watermark,
                position: Position::TopLeft,
                opacity: 100,
                margin: 0,
            },
        ))
        .expect("transform");

        let image = image::load_from_memory_with_format(&result.artifact.bytes, ImageFormat::Png)
            .expect("decode output")
            .to_rgba8();

        // A 4x4 watermark on a 64x64 output covers (0,0) and leaves (8,8) alone. Had it
        // been composited before the resize, the scale would have grown it to 32x32.
        assert_eq!(
            image.get_pixel(0, 0)[2],
            255,
            "the watermark is at the corner"
        );
        assert_eq!(
            image.get_pixel(8, 8)[2],
            0,
            "the watermark was not scaled with the image"
        );
    }

    // ── Output orientation reporting ────────────────────────────────────

    /// A transform that applies the orientation reports no pending one.
    #[test]
    fn transform_result_reports_no_orientation_once_it_has_been_applied() {
        let result = transform_raster(TransformRequest::new(
            jpeg_artifact_with_metadata(4, 2, Some(6), None),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert_eq!(result.artifact.metadata.orientation, None);
        assert_eq!(result.artifact.metadata.width, Some(2));
        assert_eq!(result.artifact.metadata.height, Some(4));
    }

    /// With auto-orientation off and the metadata retained, the output still carries the
    /// tag, and the reported metadata says so rather than claiming there is none.
    #[test]
    fn transform_result_reports_the_orientation_the_output_still_carries() {
        let result = transform_raster(TransformRequest::new(
            jpeg_artifact_with_metadata(4, 2, Some(6), None),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                auto_orient: false,
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert_eq!(result.artifact.metadata.orientation, Some(6));
        assert_eq!(
            result.artifact.metadata.oriented_dimensions(),
            Some(crate::core::Dimensions::new(2, 4)),
            "a caller reading the oriented size gets what a viewer will show"
        );
    }

    // ── fit=cover intermediate buffer limit ─────────────────────────────

    /// A cover request whose intermediate buffer is oversized is rejected, not attempted.
    ///
    /// The check runs on dimensions alone, so this test never allocates the buffer it is
    /// about: a 10000x1 source scaled to cover a 3x9999 box is 99,990,000x9999 pixels.
    #[test]
    fn cover_rejects_an_oversized_intermediate_buffer() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(10000, 1));

        let error = check_output_pixel_limit(&image, Some(3), Some(9999), Some(Fit::Cover), false)
            .expect_err("cover into an opposite aspect ratio should exceed the limit");

        assert!(
            matches!(error, TransformError::LimitExceeded(ref message) if message.contains("fit=cover")),
            "unexpected error: {error}"
        );
    }

    /// The same request succeeds under the fit modes that scale by the smaller ratio.
    #[test]
    fn the_other_fit_modes_accept_what_cover_rejects() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(10000, 1));

        for fit in [Fit::Contain, Fit::Inside, Fit::Fill] {
            check_output_pixel_limit(&image, Some(3), Some(9999), Some(fit), false)
                .unwrap_or_else(|error| panic!("{fit:?} should be within the limit: {error}"));
        }
    }

    /// An ordinary panorama cropped to a portrait box is rejected rather than silently
    /// allocating an intermediate buffer several times the output limit.
    #[test]
    fn cover_rejects_a_panorama_cropped_to_a_portrait_box() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(4000, 100));

        let error =
            check_output_pixel_limit(&image, Some(200), Some(2000), Some(Fit::Cover), false)
                .expect_err("80000x2000 is over the output limit");

        assert!(
            matches!(error, TransformError::LimitExceeded(_)),
            "unexpected error: {error}"
        );
    }

    /// A cover request that fits stays accepted.
    #[test]
    fn cover_accepts_an_intermediate_buffer_within_the_limit() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(4000, 3000));

        check_output_pixel_limit(&image, Some(200), Some(2000), Some(Fit::Cover), false)
            .expect("2667x2000 is within the limit");
    }

    /// `withoutEnlargement` caps the scale at 1.0, so it cannot produce a large buffer.
    #[test]
    fn cover_without_enlargement_never_scales_past_the_source() {
        let image = DynamicImage::ImageRgba8(RgbaImage::new(10000, 1));

        check_output_pixel_limit(&image, Some(3), Some(9999), Some(Fit::Cover), true)
            .expect("the scale is clamped to 1.0, so the buffer stays at the source size");
    }

    /// The whole pipeline reports the limit rather than aborting on the allocation.
    #[test]
    fn transform_raster_rejects_an_oversized_cover_intermediate() {
        let error = transform_raster(TransformRequest::new(
            png_artifact(1000, 1, Rgba([255, 0, 0, 255])),
            TransformOptions {
                format: Some(MediaType::Png),
                width: Some(3),
                height: Some(9999),
                fit: Some(Fit::Cover),
                ..TransformOptions::default()
            },
        ))
        .expect_err("cover should be rejected before the resize");

        assert!(
            matches!(error, TransformError::LimitExceeded(ref message) if message.contains("fit=cover")),
            "unexpected error: {error}"
        );
    }

    // ── Alpha flattening for formats without an alpha channel ───────────

    /// Decodes the single pixel at (0, 0) of an encoded image.
    fn first_pixel(bytes: &[u8], format: ImageFormat) -> Rgba<u8> {
        let image = image::load_from_memory_with_format(bytes, format).expect("decode output");
        *image.to_rgba8().get_pixel(0, 0)
    }

    /// Every channel is within `tolerance` of the expected value.
    fn assert_close(actual: Rgba<u8>, expected: [u8; 3], tolerance: i16, what: &str) {
        for channel in 0..3 {
            let difference = i16::from(actual[channel]) - i16::from(expected[channel]);
            assert!(
                difference.abs() <= tolerance,
                "{what}: channel {channel} was {}, expected about {}",
                actual[channel],
                expected[channel]
            );
        }
    }

    fn convert_to(artifact: Artifact, options: TransformOptions) -> Artifact {
        transform_raster(TransformRequest::new(artifact, options))
            .expect("transform")
            .artifact
    }

    /// Half-transparent red over the default background is a light red, not a saturated one.
    #[test]
    fn jpeg_output_composites_alpha_over_the_default_white_background() {
        let output = convert_to(
            png_artifact(8, 8, Rgba([255, 0, 0, 128])),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                ..TransformOptions::default()
            },
        );

        assert_close(
            first_pixel(&output.bytes, ImageFormat::Jpeg),
            [255, 127, 127],
            4,
            "50% red on white",
        );
    }

    /// An explicit background is honored without a padding stage in the request.
    #[test]
    fn jpeg_output_composites_alpha_over_an_explicit_background() {
        let output = convert_to(
            png_artifact(8, 8, Rgba([255, 0, 0, 128])),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                background: Some(Rgba8 {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 255,
                }),
                ..TransformOptions::default()
            },
        );

        assert_close(
            first_pixel(&output.bytes, ImageFormat::Jpeg),
            [127, 0, 0],
            4,
            "50% red on black",
        );
    }

    /// A fully transparent pixel is the background, not the black that sat under the alpha.
    #[test]
    fn jpeg_output_renders_fully_transparent_pixels_as_the_background() {
        let source = png_artifact(8, 8, Rgba([0, 0, 0, 0]));

        let default_background = convert_to(
            source.clone(),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                ..TransformOptions::default()
            },
        );
        assert_close(
            first_pixel(&default_background.bytes, ImageFormat::Jpeg),
            [255, 255, 255],
            4,
            "transparent with no background",
        );

        let explicit_background = convert_to(
            source,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                background: Some(Rgba8 {
                    r: 0,
                    g: 0,
                    b: 255,
                    a: 255,
                }),
                ..TransformOptions::default()
            },
        );
        assert_close(
            first_pixel(&explicit_background.bytes, ImageFormat::Jpeg),
            [0, 0, 255],
            4,
            "transparent with an explicit background",
        );
    }

    /// The direct path and the padding path resolve transparency the same way.
    ///
    /// This equivalence is the property that was broken: a request that happened to include
    /// a `contain` resize composited correctly while the same conversion without one did not.
    #[test]
    fn jpeg_output_agrees_with_and_without_a_padding_stage() {
        let background = Some(Rgba8 {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        });

        let direct = convert_to(
            png_artifact(8, 8, Rgba([0, 0, 255, 64])),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                background,
                ..TransformOptions::default()
            },
        );
        let padded = convert_to(
            png_artifact(8, 8, Rgba([0, 0, 255, 64])),
            TransformOptions {
                format: Some(MediaType::Jpeg),
                background,
                width: Some(16),
                height: Some(8),
                fit: Some(Fit::Contain),
                ..TransformOptions::default()
            },
        );

        let direct_pixel = first_pixel(&direct.bytes, ImageFormat::Jpeg);
        let padded_pixel = *image::load_from_memory_with_format(&padded.bytes, ImageFormat::Jpeg)
            .expect("decode padded")
            .to_rgba8()
            .get_pixel(8, 4);
        assert_close(
            direct_pixel,
            [padded_pixel[0], padded_pixel[1], padded_pixel[2]],
            6,
            "direct conversion versus padded conversion",
        );
    }

    /// Formats that carry alpha keep it. Flattening is only correct where it cannot survive.
    #[test]
    fn alpha_capable_output_formats_keep_their_transparency() {
        for format in [
            MediaType::Png,
            MediaType::Webp,
            MediaType::Bmp,
            MediaType::Tiff,
        ] {
            let output = convert_to(
                png_artifact(8, 8, Rgba([255, 0, 0, 128])),
                TransformOptions {
                    format: Some(format),
                    ..TransformOptions::default()
                },
            );
            assert_eq!(
                output.metadata.has_alpha,
                Some(true),
                "{} output should keep the alpha channel",
                format.as_name()
            );
        }
    }

    // ── Exhaustive EXIF orientation tests (issue #106) ──────────────────

    /// Every EXIF orientation, checked by where a single marker pixel lands.
    ///
    /// The expected coordinates are derived from the TIFF/EXIF definition of each value
    /// rather than from the implementation. Orientations 5 to 8 all turn a 4x2 image into
    /// a 2x4 one, so a dimension assertion cannot tell them apart; the marker can.
    #[test]
    fn apply_exif_orientation_places_the_marker_pixel_per_the_exif_definition() {
        // (orientation, expected dimensions, expected marker position, meaning)
        let cases = [
            (1, (4, 2), (0, 0), "no transform"),
            (2, (4, 2), (3, 0), "mirror horizontal"),
            (3, (4, 2), (3, 1), "rotate 180"),
            (4, (4, 2), (0, 1), "mirror vertical"),
            (5, (2, 4), (0, 0), "mirror horizontal and rotate 270 CW"),
            (6, (2, 4), (1, 0), "rotate 90 CW"),
            (7, (2, 4), (1, 3), "mirror horizontal and rotate 90 CW"),
            (8, (2, 4), (0, 3), "rotate 270 CW"),
        ];

        let marker = Rgba([255, 0, 0, 255]);
        for (orientation, dimensions, (marker_x, marker_y), meaning) in cases {
            let mut image = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
            image.put_pixel(0, 0, marker);
            let result = apply_exif_orientation(DynamicImage::ImageRgba8(image), orientation);
            let rgba = result.to_rgba8();
            assert_eq!(
                rgba.dimensions(),
                dimensions,
                "orientation {orientation} ({meaning}) produced the wrong dimensions"
            );
            assert_eq!(
                *rgba.get_pixel(marker_x, marker_y),
                marker,
                "orientation {orientation} ({meaning}) put the marker somewhere else"
            );
        }
    }

    /// Orientation 5 and 7 differ. They were each other's transform until this was pinned.
    #[test]
    fn apply_exif_orientation_5_and_7_differ() {
        let mut image = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 0, 255]));
        image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let source = DynamicImage::ImageRgba8(image);

        let five = apply_exif_orientation(source.clone(), 5).to_rgba8();
        let seven = apply_exif_orientation(source, 7).to_rgba8();

        assert_ne!(five.into_raw(), seven.into_raw());
    }

    /// Orientation values outside 1..=8 leave the image alone.
    #[test]
    fn apply_exif_orientation_passes_through_out_of_range_values() {
        for orientation in [0, 9, u16::MAX] {
            let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([1, 2, 3, 255])));
            let result = apply_exif_orientation(image, orientation);
            assert_eq!(
                result.dimensions(),
                (4, 2),
                "orientation {orientation} should be a no-op"
            );
        }
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

            let (expected_w, expected_h) = if orientation >= 5 { (2, 4) } else { (4, 2) };
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

    /// PNG, WebP, and TIFF carry the same tag, and every browser honours it in all three.
    /// Reading it in one container and not the others turns the container a photo happens
    /// to arrive in into the thing that decides whether the picture comes out upright.
    #[rstest]
    #[case(MediaType::Png)]
    #[case(MediaType::Webp)]
    #[case(MediaType::Tiff)]
    fn transform_raster_auto_orients_every_container_that_carries_the_tag(
        #[case] media_type: MediaType,
    ) {
        let bytes = match media_type {
            MediaType::Png => png_artifact_with_metadata(4, 2, Some(6), None).bytes,
            MediaType::Webp => webp_artifact_with_metadata(4, 2, Some(6), None).bytes,
            MediaType::Tiff => tiff_bytes_with_orientation(4, 2, 6),
            other => unreachable!("{other:?}"),
        };
        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff");

        assert_eq!(
            artifact.metadata.orientation,
            Some(6),
            "{media_type:?}: the sniffer should report the tag"
        );

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .unwrap_or_else(|error| panic!("{media_type:?} should transform: {error}"));

        assert_eq!(
            (
                result.artifact.metadata.width,
                result.artifact.metadata.height
            ),
            (Some(2), Some(4)),
            "{media_type:?}: a quarter turn should have been applied"
        );
    }

    /// Whether the tag survives is a fact about the bytes, not about the policy: a TIFF's
    /// metadata is never read, and BMP and TIFF outputs carry none, so keeping the metadata
    /// keeps nothing in those cases and the drop has to be said.
    #[rstest]
    #[case::tiff_input_to_png(MediaType::Tiff, MediaType::Png)]
    #[case::jpeg_input_to_bmp(MediaType::Jpeg, MediaType::Bmp)]
    #[case::jpeg_input_to_tiff(MediaType::Jpeg, MediaType::Tiff)]
    fn transform_raster_warns_when_a_kept_orientation_never_reaches_the_output(
        #[case] input: MediaType,
        #[case] output: MediaType,
    ) {
        let bytes = match input {
            MediaType::Tiff => tiff_bytes_with_orientation(4, 2, 6),
            MediaType::Jpeg => jpeg_artifact_with_metadata(4, 2, Some(6), None).bytes,
            other => unreachable!("{other:?}"),
        };
        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff");

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(output),
                auto_orient: false,
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .unwrap_or_else(|error| panic!("{input:?} to {output:?} should transform: {error}"));

        assert_eq!(
            result.artifact.metadata.orientation, None,
            "{input:?} to {output:?}: the tag should not have survived"
        );
        assert!(
            result.warnings.iter().any(|warning| matches!(
                warning,
                TransformWarning::OrientationDropped { orientation: 6 }
            )),
            "{input:?} to {output:?}: expected an OrientationDropped warning, got {:?}",
            result.warnings
        );
    }

    /// The negative: when the tag does reach the output there is nothing to warn about, and
    /// the output's metadata reports the tag `inspect` will find in it.
    #[test]
    fn transform_raster_does_not_warn_when_the_kept_orientation_reaches_the_output() {
        let artifact = jpeg_artifact_with_metadata(4, 2, Some(6), None);

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Jpeg),
                auto_orient: false,
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert_eq!(result.artifact.metadata.orientation, Some(6));
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    /// The #331 warning is about the combination, not about the container.
    #[test]
    fn transform_raster_warns_when_a_png_orientation_is_dropped_unapplied() {
        let bytes = png_artifact_with_metadata(4, 2, Some(6), None).bytes;
        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff png");

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                auto_orient: false,
                ..TransformOptions::default()
            },
        ))
        .expect("transform");

        assert!(
            result.warnings.iter().any(|warning| matches!(
                warning,
                TransformWarning::OrientationDropped { orientation: 6 }
            )),
            "expected an OrientationDropped warning, got {:?}",
            result.warnings
        );
    }

    /// A clean aperture is cut at decode, before the orientation: the cropped fixture is a
    /// 40x20 picture whose aperture keeps the middle 30 columns, so 5 of the 10 blue columns
    /// survive, and the rotated one turns that cut by a quarter turn afterwards.
    #[cfg(feature = "avif")]
    #[rstest]
    #[case::cropped(
        include_bytes!("../../integration/fixtures/clap-cropped.avif"),
        (30, 20),
        [((2, 10), [0, 0, 255]), ((27, 10), [255, 0, 0])]
    )]
    #[case::rotated(
        include_bytes!("../../integration/fixtures/clap-rotated.avif"),
        (20, 30),
        [((10, 2), [0, 0, 255]), ((10, 27), [255, 0, 0])]
    )]
    fn transform_raster_cuts_an_avif_to_its_clean_aperture_before_orienting_it(
        #[case] bytes: &[u8],
        #[case] expected: (u32, u32),
        #[case] markers: [((u32, u32), [u8; 3]); 2],
    ) {
        let artifact = sniff_artifact(RawArtifact::new(bytes.to_vec(), None)).expect("sniff avif");

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("decode a clean-aperture avif");

        assert_eq!(
            (
                result.artifact.metadata.width,
                result.artifact.metadata.height
            ),
            (Some(expected.0), Some(expected.1))
        );

        let image = image::load_from_memory(&result.artifact.bytes)
            .expect("decode png")
            .to_rgb8();
        for ((x, y), expected) in markers {
            let pixel = image.get_pixel(x, y).0;
            assert!(
                pixel
                    .iter()
                    .zip(expected)
                    .all(|(got, want)| got.abs_diff(want) < 40),
                "pixel ({x}, {y}) should be near {expected:?}, got {pixel:?}"
            );
        }
    }

    /// The two fixtures are what libheif writes for a phone photo: the transform as `irot`
    /// and `imir` item properties, with no Exif block. The transposed one is the pair a
    /// mirror in the wrong order gets backwards, so the marker bars are checked and not
    /// only the dimensions, which are 20x40 for every orientation from 5 to 8.
    #[cfg(feature = "avif")]
    #[rstest]
    #[case::rotated(
        include_bytes!("../../integration/fixtures/irot-rotated.avif"),
        6,
        [((10, 2), [0, 0, 255]), ((10, 30), [255, 0, 0])]
    )]
    #[case::transposed(
        include_bytes!("../../integration/fixtures/imir-transposed-5.avif"),
        5,
        [((8, 1), [0, 0, 255]), ((1, 8), [255, 0, 0])]
    )]
    fn transform_raster_auto_orients_an_avif_by_its_item_properties(
        #[case] bytes: &[u8],
        #[case] orientation: u16,
        #[case] markers: [((u32, u32), [u8; 3]); 2],
    ) {
        let artifact = sniff_artifact(RawArtifact::new(bytes.to_vec(), None)).expect("sniff avif");
        assert_eq!(artifact.metadata.orientation, Some(orientation));

        let result = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect("transform avif");

        assert_eq!(
            (
                result.artifact.metadata.width,
                result.artifact.metadata.height
            ),
            (Some(20), Some(40))
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);

        let image = image::load_from_memory(&result.artifact.bytes)
            .expect("decode png")
            .to_rgb8();
        for ((x, y), expected) in markers {
            let pixel = image.get_pixel(x, y).0;
            assert!(
                pixel
                    .iter()
                    .zip(expected)
                    .all(|(got, want)| got.abs_diff(want) < 40),
                "pixel ({x}, {y}) should be near {expected:?}, got {pixel:?}"
            );
        }
    }
}
