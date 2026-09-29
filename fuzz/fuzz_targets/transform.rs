//! The whole decode, transform, and encode pipeline through the public library API.
//!
//! The input is a header of option bytes followed by the image bytes, and the options are
//! kept small so one input cannot take seconds: the box is at most 64 pixels a side, the
//! source at most 256 a side by what the sniffer reports, and SVG is always rendered into a box.
//!
//! Beyond not panicking, a successful transform must produce what it says it produced:
//!
//! - the output is in the requested format, and the sniffer reads it back as that format with
//!   the dimensions the result reports;
//! - with a box and `contain`, `cover`, or `fill`, the output is exactly the box, with
//!   `inside` it fits in the box, and with a width or a height alone that side is exact;
//! - with no resize, crop, or rotation, the output has the dimensions the sniffer reported for
//!   the input, oriented when auto-orientation is on (the sniffer and the decoder agree).
#![no_main]

use std::str::FromStr;
use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use truss::{
    CropRegion, Dimensions, Fit, MediaType, OptimizeMode, Position, RawArtifact, Rgba8, Rotation,
    TransformOptions, TransformRequest, WatermarkInput, sniff_artifact, transform,
};
use truss_fuzz::Input;

const MAX_BOX: u8 = 64;
const MAX_SOURCE_SIDE: u32 = 256;
const WATERMARK: &[u8] = include_bytes!("../../integration/fixtures/1px.png");

const FORMATS: [Option<MediaType>; 8] = [
    None,
    Some(MediaType::Jpeg),
    Some(MediaType::Png),
    Some(MediaType::Webp),
    Some(MediaType::Bmp),
    Some(MediaType::Tiff),
    Some(MediaType::Gif),
    Some(MediaType::Svg),
];
const FITS: [Option<Fit>; 5] = [
    None,
    Some(Fit::Contain),
    Some(Fit::Cover),
    Some(Fit::Fill),
    Some(Fit::Inside),
];
const POSITIONS: [&str; 9] = [
    "center",
    "top",
    "right",
    "bottom",
    "left",
    "top-left",
    "top-right",
    "bottom-left",
    "bottom-right",
];

fn side(byte: u8) -> Option<u32> {
    (byte & 0x80 != 0).then(|| u32::from(byte % MAX_BOX) + 1)
}

fn options_from(input: &mut Input<'_>) -> (TransformOptions, bool) {
    let mut options = TransformOptions::default();
    options.format = FORMATS[usize::from(input.byte()) % FORMATS.len()];
    options.width = side(input.byte());
    options.height = side(input.byte());
    let fit_position = input.byte();
    options.fit = FITS[usize::from(fit_position) % FITS.len()];
    if fit_position >= 128 {
        options.position =
            Position::from_str(POSITIONS[usize::from(fit_position / 5) % POSITIONS.len()]).ok();
    }
    options.rotate = Rotation::from_degrees((i32::from(input.byte()) - 128) * 3);

    let flags = input.byte();
    options.grayscale = flags & 0x01 != 0;
    options.without_enlargement = flags & 0x02 != 0;
    options.auto_orient = flags & 0x04 == 0;
    options.strip_metadata = flags & 0x08 == 0;
    options.preserve_exif = flags & 0x10 != 0;
    let (r, g, b, a) = (input.byte(), input.byte(), input.byte(), input.byte());
    if flags & 0x20 != 0 {
        options.background = Some(Rgba8 { r, g, b, a });
    }
    options.optimize = match flags >> 6 {
        0 => OptimizeMode::None,
        1 => OptimizeMode::Auto,
        2 => OptimizeMode::Lossless,
        _ => OptimizeMode::Lossy,
    };

    let quality = input.byte();
    options.quality = (quality != 0).then_some(quality % 101);
    let filter = input.byte();
    match filter {
        1..=100 => options.blur = Some(f32::from(filter) / 20.0),
        101..=200 => options.sharpen = Some(f32::from(filter - 100) / 20.0),
        _ => {}
    }

    let (crop, crop_size) = (input.byte(), input.byte());
    if crop & 0x01 != 0 {
        options.crop = Some(CropRegion {
            x: u32::from((crop >> 1) & 0x07),
            y: u32::from((crop >> 4) & 0x07),
            width: u32::from(crop_size & 0x0f) + 1,
            height: u32::from(crop_size >> 4) + 1,
        });
    }
    options.deadline = Some(Duration::from_secs(2));

    let watermark = input.byte() & 0x01 != 0;
    (options, watermark)
}

fuzz_target!(|data: &[u8]| {
    let mut input = Input::new(data);
    let (options, with_watermark) = options_from(&mut input);
    let Ok(source) = sniff_artifact(RawArtifact::new(input.rest().to_vec(), None)) else {
        return;
    };

    // Keep one input cheap. A raster source is bounded on each side by the size its header
    // claims (a bound on the area alone lets a 10000x1 strip rotate into a 7000x7000 canvas),
    // and an SVG, whose own size can be anything, is always drawn into the small box.
    if source.media_type == MediaType::Svg {
        if options.width.is_none()
            || options.height.is_none()
            || options.fit == Some(Fit::Cover)
            || options.crop.is_some()
        {
            return;
        }
    } else {
        match source.metadata.dimensions() {
            Some(size) if size.width <= MAX_SOURCE_SIDE && size.height <= MAX_SOURCE_SIDE => {}
            _ => return,
        }
    }

    let source_metadata = source.metadata.clone();
    let requested_format = options.format.unwrap_or(source.media_type.default_output());
    let request = if with_watermark {
        let mark = sniff_artifact(RawArtifact::new(WATERMARK.to_vec(), None))
            .expect("the watermark fixture sniffs");
        TransformRequest::with_watermark(source, options.clone(), WatermarkInput::new(mark))
    } else {
        TransformRequest::new(source, options.clone())
    };

    let Ok(result) = transform(request) else {
        return;
    };
    let output = result.artifact;
    assert_eq!(
        output.media_type, requested_format,
        "the output is not in the requested format"
    );
    if output.media_type == MediaType::Svg {
        return;
    }

    let reread = sniff_artifact(RawArtifact::new(output.bytes.clone(), None))
        .unwrap_or_else(|error| panic!("the output does not sniff: {error}"));
    assert_eq!(
        reread.media_type, output.media_type,
        "the output sniffs as another format"
    );
    let size = output
        .metadata
        .dimensions()
        .expect("a raster output reports its dimensions");
    assert_eq!(
        reread.metadata.dimensions(),
        Some(size),
        "the output's reported size is not the size it encodes"
    );

    if !options.without_enlargement {
        match (options.width, options.height, options.fit) {
            (Some(width), Some(height), Some(Fit::Contain | Fit::Cover | Fit::Fill)) => {
                assert_eq!(size, Dimensions::new(width, height), "the box is not exact");
            }
            (Some(width), Some(height), Some(Fit::Inside)) => {
                assert!(
                    size.width <= width && size.height <= height,
                    "{size:?} does not fit inside {width}x{height}"
                );
            }
            (Some(width), None, _) => assert_eq!(size.width, width, "the width is not exact"),
            (None, Some(height), _) => {
                assert_eq!(size.height, height, "the height is not exact");
            }
            _ => {}
        }
    }

    let untouched = options.width.is_none()
        && options.height.is_none()
        && options.crop.is_none()
        && options.rotate.is_identity();
    if untouched {
        let expected = if options.auto_orient {
            source_metadata.oriented_dimensions()
        } else {
            source_metadata.dimensions()
        };
        if let Some(expected) = expected {
            assert_eq!(
                size, expected,
                "the decoded size disagrees with the sniffed size"
            );
        }
    }
});
