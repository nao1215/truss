use crate::core::{
    Artifact, ArtifactMetadata, Fit, MediaType, MetadataPolicy, Position, Rotation, TransformError,
    TransformRequest,
};
use crate::Rgba8;
use exif::{In, Reader, Tag, Value};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::{self, FilterType};
use image::{
    ColorType, DynamicImage, GenericImageView, ImageEncoder, ImageFormat, Rgba, RgbaImage,
};
use std::io::Cursor;

/// Transforms a raster artifact using the current backend implementation.
///
/// The input artifact must already be classified by [`crate::sniff_artifact`]. This backend
/// performs raster-only work for the current implementation phase: optional EXIF auto-orient
/// for JPEG input, explicit rotation, resize handling, and encoding into the requested output
/// format.
///
/// # Errors
///
/// Returns [`TransformError::InvalidOptions`] when the request fails Core validation,
/// [`TransformError::DecodeFailed`] or [`TransformError::EncodeFailed`] when image processing
/// fails, and [`TransformError::CapabilityMissing`] for features that are intentionally not
/// implemented yet, such as metadata retention or AVIF encode/decode.
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
pub fn transform_raster(request: TransformRequest) -> Result<Artifact, TransformError> {
    let normalized = request.normalize()?;

    if matches!(
        normalized.options.metadata_policy,
        MetadataPolicy::KeepAll | MetadataPolicy::PreserveExif
    ) {
        return Err(TransformError::CapabilityMissing(
            "metadata retention is not implemented yet".to_string(),
        ));
    }

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
) -> Result<Vec<u8>, TransformError> {
    let mut bytes = Vec::new();

    match media_type {
        MediaType::Jpeg => {
            let quality = quality.unwrap_or(80);
            let encoder = JpegEncoder::new_with_quality(&mut bytes, quality);
            let rgb = image.to_rgb8();
            encoder
                .write_image(&rgb, rgb.width(), rgb.height(), ColorType::Rgb8.into())
                .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
        }
        MediaType::Png => {
            let encoder = PngEncoder::new(&mut bytes);
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

            let encoder = WebPEncoder::new_lossless(&mut bytes);
            let rgba = image.to_rgba8();
            encoder
                .write_image(&rgba, rgba.width(), rgba.height(), ColorType::Rgba8.into())
                .map_err(|error| TransformError::EncodeFailed(error.to_string()))?;
        }
        MediaType::Avif => {
            return Err(TransformError::CapabilityMissing(
                "avif encode is not implemented yet".to_string(),
            ));
        }
    }

    Ok(bytes)
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
    use crate::{Rgba8, TransformError};
    use image::codecs::png::PngEncoder;
    use image::{ColorType, GenericImageView, ImageEncoder, ImageFormat, Rgba, RgbaImage};

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
    fn transform_raster_rejects_metadata_retention() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let err = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                strip_metadata: false,
                ..TransformOptions::default()
            },
        ))
        .expect_err("metadata retention should fail");

        assert_eq!(
            err,
            TransformError::CapabilityMissing(
                "metadata retention is not implemented yet".to_string()
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
    fn transform_raster_rejects_avif_encode_for_now() {
        let artifact = png_artifact(4, 3, Rgba([10, 20, 30, 255]));
        let err = transform_raster(TransformRequest::new(
            artifact,
            TransformOptions {
                format: Some(MediaType::Avif),
                ..TransformOptions::default()
            },
        ))
        .expect_err("avif encode should fail");

        assert_eq!(
            err,
            TransformError::CapabilityMissing("avif encode is not implemented yet".to_string())
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
