use crate::core::{
    Artifact, ArtifactMetadata, Fit, MediaType, MetadataPolicy, Position, Rotation, TransformError,
    TransformRequest,
};
use crate::Rgba8;
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
use std::io::Cursor;

/// Transforms a raster artifact using the current backend implementation.
///
/// The input artifact must already be classified by [`crate::sniff_artifact`]. This backend
/// performs raster-only work for the current implementation phase: optional EXIF auto-orient
/// for JPEG input, explicit rotation, resize handling, and encoding into the requested output
/// format. Metadata stripping remains the default, while `preserve_exif` retains EXIF and
/// `keep-metadata` retains EXIF plus ICC profiles for JPEG, PNG, and WebP output. Metadata types
/// that the current encoders cannot round-trip, such as XMP or IPTC, still raise a capability
/// error instead of being silently dropped.
///
/// # Errors
///
/// Returns [`TransformError::InvalidOptions`] when the request fails Core validation,
/// [`TransformError::DecodeFailed`] or [`TransformError::EncodeFailed`] when image processing
/// fails, and [`TransformError::CapabilityMissing`] for features that are intentionally not
/// implemented yet, such as AVIF input decode or metadata types that the current encoders cannot
/// preserve.
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
/// assert_eq!(output.media_type, MediaType::Jpeg);
/// assert_eq!(output.metadata.width, Some(2));
/// assert_eq!(output.metadata.height, Some(2));
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
/// let sniffed = sniff_artifact(RawArtifact::new(output.bytes.clone(), None)).unwrap();
///
/// assert_eq!(output.media_type, MediaType::Avif);
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
/// let mut decoder = JpegDecoder::new(Cursor::new(&output.bytes)).unwrap();
/// let exif = decoder.exif_metadata().unwrap().unwrap();
///
/// assert_eq!(output.metadata.width, Some(1));
/// assert_eq!(output.metadata.height, Some(2));
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
/// let mut decoder = JpegDecoder::new(Cursor::new(&output.bytes)).unwrap();
/// assert_eq!(decoder.icc_profile().unwrap(), Some(b"demo-icc-profile".to_vec()));
/// ```
pub fn transform_raster(request: TransformRequest) -> Result<Artifact, TransformError> {
    let normalized = request.normalize()?;

    let retained_metadata = extract_retained_metadata(
        &normalized.input,
        normalized.options.metadata_policy,
        normalized.options.auto_orient,
        normalized.options.format,
    )?;

    let mut image = decode_input(&normalized.input)?;

    if normalized.options.auto_orient {
        image = apply_auto_orientation(image, &normalized.input);
    }

    image = apply_rotation(image, normalized.options.rotate);
    image = apply_resize(
        image,
        normalized.options.width,
        normalized.options.height,
        normalized.options.fit,
        normalized.options.position,
        normalized.options.background,
        normalized.options.format,
    );

    let bytes = encode_output(
        &image,
        normalized.options.format,
        normalized.options.quality,
        retained_metadata.as_ref(),
    )?;
    let (width, height) = image.dimensions();

    Ok(Artifact::new(
        bytes,
        normalized.options.format,
        ArtifactMetadata {
            width: Some(width),
            height: Some(height),
            frame_count: 1,
            duration: None,
            has_alpha: Some(output_has_alpha(&image, normalized.options.format)),
        },
    ))
}

fn decode_input(input: &Artifact) -> Result<DynamicImage, TransformError> {
    let image_format = match input.media_type {
        MediaType::Jpeg => ImageFormat::Jpeg,
        MediaType::Png => ImageFormat::Png,
        MediaType::Webp => ImageFormat::WebP,
        MediaType::Avif => {
            return Err(TransformError::CapabilityMissing(
                "avif decode is not implemented yet".to_string(),
            ));
        }
    };

    image::load_from_memory_with_format(&input.bytes, image_format)
        .map_err(|error| TransformError::DecodeFailed(error.to_string()))
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
        None if matches!(output_format, MediaType::Jpeg | MediaType::Avif) => {
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
            if quality.is_some() {
                return Err(TransformError::CapabilityMissing(
                    "webp quality control is not implemented yet".to_string(),
                ));
            }

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
            let rgba = image.to_rgba8();
            encoder
                .write_image(&rgba, rgba.width(), rgba.height(), ColorType::Rgba8.into())
                .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
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
    }

    Ok(bytes)
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

    fn has_unsupported_fields(&self) -> bool {
        self.xmp_metadata.is_some() || self.iptc_metadata.is_some()
    }

    fn retain_exif_only(mut self) -> Self {
        self.icc_profile = None;
        self.xmp_metadata = None;
        self.iptc_metadata = None;
        self
    }

    fn retain_supported_keep_all(mut self) -> Self {
        self.xmp_metadata = None;
        self.iptc_metadata = None;
        self
    }
}

fn extract_retained_metadata(
    input: &Artifact,
    metadata_policy: MetadataPolicy,
    auto_orient: bool,
    output_format: MediaType,
) -> Result<Option<RetainedMetadata>, TransformError> {
    if matches!(metadata_policy, MetadataPolicy::StripAll) {
        return Ok(None);
    }

    let mut metadata = read_input_metadata(input)?;
    if let Some(exif_chunk) = metadata.exif_metadata.as_mut() {
        if auto_orient && matches!(input.media_type, MediaType::Jpeg) {
            let _ = Orientation::remove_from_exif_chunk(exif_chunk);
        }
    }

    let metadata = match metadata_policy {
        MetadataPolicy::StripAll => return Ok(None),
        MetadataPolicy::PreserveExif => metadata.retain_exif_only(),
        MetadataPolicy::KeepAll => {
            if metadata.has_unsupported_fields() {
                return Err(TransformError::CapabilityMissing(
                    "xmp and iptc retention is not implemented yet".to_string(),
                ));
            }
            metadata.retain_supported_keep_all()
        }
    };

    if matches!(output_format, MediaType::Avif) && !metadata.is_empty() {
        return Err(TransformError::CapabilityMissing(
            "metadata retention is not implemented for avif output".to_string(),
        ));
    }

    if metadata.is_empty() {
        return Ok(None);
    }

    Ok(Some(metadata))
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
        MediaType::Avif => Ok(RetainedMetadata::default()),
    }
}

fn output_has_alpha(image: &DynamicImage, media_type: MediaType) -> bool {
    match media_type {
        MediaType::Jpeg => false,
        MediaType::Png | MediaType::Webp | MediaType::Avif => image.color().has_alpha(),
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_exif_orientation, transform_raster};
    use crate::core::{
        Artifact, ArtifactMetadata, Fit, MediaType, Position, Rotation, TransformOptions,
        TransformRequest,
    };
    use crate::{sniff_artifact, RawArtifact, Rgba8, TransformError};
    use image::codecs::jpeg::JpegDecoder;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngDecoder;
    use image::codecs::png::PngEncoder;
    use image::codecs::webp::WebPDecoder;
    use image::codecs::webp::WebPEncoder;
    use image::metadata::Orientation;
    use image::{
        ColorType, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat, Rgba, RgbaImage,
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

        assert_eq!(result.media_type, MediaType::Jpeg);
        assert_eq!(result.metadata.width, Some(4));
        assert_eq!(result.metadata.height, Some(3));
        assert_eq!(result.metadata.has_alpha, Some(false));
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

        assert_eq!(result.metadata.width, Some(8));
        assert_eq!(result.metadata.height, Some(4));
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

        assert_eq!(result.metadata.width, Some(8));
        assert_eq!(result.metadata.height, Some(8));
        assert_eq!(
            top_left_pixel(&result.bytes, ImageFormat::Png),
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

        assert_eq!(result.metadata.width, Some(2));
        assert_eq!(result.metadata.height, Some(2));
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

        assert_eq!(result.metadata.width, Some(2));
        assert_eq!(result.metadata.height, Some(4));
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

        let mut decoder = JpegDecoder::new(Cursor::new(&result.bytes)).expect("decode jpeg");
        let exif = decoder
            .exif_metadata()
            .expect("read jpeg exif")
            .expect("retained exif");

        assert_eq!(result.metadata.width, Some(2));
        assert_eq!(result.metadata.height, Some(4));
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

        let mut decoder = JpegDecoder::new(Cursor::new(&result.bytes)).expect("decode jpeg");

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

        let mut decoder = PngDecoder::new(Cursor::new(&result.bytes)).expect("decode png");
        let exif = decoder
            .exif_metadata()
            .expect("read png exif")
            .expect("retained png exif");

        assert_eq!(result.metadata.width, Some(4));
        assert_eq!(result.metadata.height, Some(2));
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

        let mut decoder = JpegDecoder::new(Cursor::new(&result.bytes)).expect("decode jpeg");
        let exif = decoder
            .exif_metadata()
            .expect("read jpeg exif")
            .expect("retained exif");
        let icc_profile = decoder
            .icc_profile()
            .expect("read jpeg icc")
            .expect("retained icc");

        assert_eq!(result.metadata.width, Some(2));
        assert_eq!(result.metadata.height, Some(4));
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

        let mut decoder = PngDecoder::new(Cursor::new(&result.bytes)).expect("decode png");
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

        let mut decoder = WebPDecoder::new(Cursor::new(&result.bytes)).expect("decode webp");
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

        assert_eq!(result.media_type, MediaType::Png);
        assert_eq!(result.metadata.width, Some(4));
        assert_eq!(result.metadata.height, Some(3));
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

    #[test]
    fn transform_raster_rejects_webp_quality_for_now() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let err = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Webp),
                quality: Some(90),
                ..TransformOptions::default()
            },
        ))
        .expect_err("webp quality should fail");

        assert_eq!(
            err,
            TransformError::CapabilityMissing(
                "webp quality control is not implemented yet".to_string()
            )
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
        let sniffed = sniff_artifact(RawArtifact::new(result.bytes.clone(), None))
            .expect("sniff avif output");

        assert_eq!(result.media_type, MediaType::Avif);
        assert_eq!(result.metadata.width, Some(4));
        assert_eq!(result.metadata.height, Some(3));
        assert_eq!(sniffed.media_type, MediaType::Avif);
    }

    #[test]
    fn transform_raster_rejects_avif_decode_for_now() {
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
            .expect_err("avif decode should fail");

        assert_eq!(
            err,
            TransformError::CapabilityMissing("avif decode is not implemented yet".to_string())
        );
    }

    #[test]
    fn apply_exif_orientation_rotates_dimensions() {
        let image =
            image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 2, Rgba([10, 20, 30, 255])));
        let rotated = apply_exif_orientation(image, 6);

        assert_eq!(rotated.dimensions(), (2, 4));
    }
}
