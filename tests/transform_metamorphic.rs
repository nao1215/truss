//! Metamorphic relations between transforms, over generated images.
//!
//! The right output of a rotation, a crop or a format conversion is hard to write down for an
//! arbitrary image, but how two transforms relate is not: four quarter turns are no turn, a
//! crop of the whole image is the image, the pipeline rotates before it crops whether that is
//! one request or two, and a lossless format gives back the pixels it was handed. Each test
//! states one such relation and proptest searches small random images for a counterexample.
//! The relations are exact, because every stage involved only moves or re-encodes pixels.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "the error-handling lints in Cargo.toml are for the code that ships; a test stops at the first thing it cannot set up"
)]

use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, RgbaImage};
use proptest::prelude::*;
use truss::{
    CropRegion, MediaType, RawArtifact, Rotation, TransformOptions, TransformRequest,
    sniff_artifact, transform,
};

/// A small image of random pixels. Opaque when asked, since the formats without alpha and
/// lossless WebP are only exact for opaque pixels.
fn image(opaque: bool) -> impl Strategy<Value = RgbaImage> {
    (1u32..=12, 1u32..=12).prop_flat_map(move |(width, height)| {
        proptest::collection::vec(any::<[u8; 4]>(), (width * height) as usize).prop_map(
            move |pixels| {
                let mut image = RgbaImage::new(width, height);
                for (pixel, value) in image.pixels_mut().zip(pixels) {
                    pixel.0 = if opaque {
                        [value[0], value[1], value[2], u8::MAX]
                    } else {
                        value
                    };
                }
                image
            },
        )
    })
}

/// An image together with a crop region that lies inside it.
fn image_and_region() -> impl Strategy<Value = (RgbaImage, CropRegion)> {
    image(false).prop_flat_map(|image| {
        let (width, height) = image.dimensions();
        (0..width, 0..height)
            .prop_flat_map(move |(x, y)| (Just((x, y)), 1..=width - x, 1..=height - y))
            .prop_map(move |((x, y), w, h)| {
                (
                    image.clone(),
                    CropRegion {
                        x,
                        y,
                        width: w,
                        height: h,
                    },
                )
            })
    })
}

fn png(image: &RgbaImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            image,
            image.width(),
            image.height(),
            ColorType::Rgba8.into(),
        )
        .expect("encode png");
    bytes
}

/// The default options with the changes a test names. `TransformOptions` is
/// `#[non_exhaustive]`, so a test outside the crate starts from the default and assigns.
fn options(change: impl FnOnce(&mut TransformOptions)) -> TransformOptions {
    let mut options = TransformOptions::default();
    change(&mut options);
    options
}

fn run(bytes: Vec<u8>, options: TransformOptions) -> Vec<u8> {
    let input = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff input");
    transform(TransformRequest::new(input, options))
        .expect("transform")
        .artifact
        .bytes
}

fn rotate(bytes: Vec<u8>, degrees: i32) -> Vec<u8> {
    run(
        bytes,
        options(|o| o.rotate = Rotation::from_degrees(degrees)),
    )
}

fn crop(bytes: Vec<u8>, region: CropRegion) -> Vec<u8> {
    run(bytes, options(|o| o.crop = Some(region)))
}

fn convert(bytes: Vec<u8>, format: MediaType) -> Vec<u8> {
    run(bytes, options(|o| o.format = Some(format)))
}

fn pixels(bytes: &[u8]) -> RgbaImage {
    image::load_from_memory(bytes)
        .expect("decode output")
        .to_rgba8()
}

proptest! {
    /// Four quarter turns, and two half turns, are no turn.
    #[test]
    fn whole_turns_give_the_image_back(image in image(false)) {
        let original = png(&image);
        let mut quarters = original.clone();
        for _ in 0..4 {
            quarters = rotate(quarters, 90);
        }
        prop_assert_eq!(pixels(&quarters), image.clone());
        let halves = rotate(rotate(original, 180), 180);
        prop_assert_eq!(pixels(&halves), image);
    }

    /// A turn and the opposite turn cancel, whichever way the angle is spelled.
    #[test]
    fn opposite_turns_cancel(image in image(false), quarter in 1i32..4) {
        let there = rotate(png(&image), 90 * quarter);
        prop_assert_eq!(pixels(&rotate(there.clone(), -90 * quarter)), image.clone());
        prop_assert_eq!(pixels(&rotate(there, 360 - 90 * quarter)), image);
    }

    /// Cropping the whole image is the image.
    #[test]
    fn a_full_crop_is_the_identity(image in image(false)) {
        let region = CropRegion { x: 0, y: 0, width: image.width(), height: image.height() };
        prop_assert_eq!(pixels(&crop(png(&image), region)), image);
    }

    /// A crop takes exactly the pixels of its rectangle.
    #[test]
    fn a_crop_takes_its_rectangle((image, region) in image_and_region()) {
        let expected = image::imageops::crop_imm(&image, region.x, region.y, region.width, region.height).to_image();
        prop_assert_eq!(pixels(&crop(png(&image), region)), expected);
    }

    /// The pipeline rotates before it crops, so a request naming both answers the same as a
    /// rotation followed by a second request for the crop; and cropping first, then rotating,
    /// gives the same pixels when the rectangle is mapped back through the turn.
    #[test]
    fn rotate_then_crop_is_one_request_or_two((image, region) in image_and_region()) {
        // Rotate the source a quarter turn clockwise, then pick the rectangle in the rotated
        // frame that corresponds to `region` in the source frame.
        let height = image.height();
        let rotated_region = CropRegion {
            x: height - region.y - region.height,
            y: region.x,
            width: region.height,
            height: region.width,
        };
        let one = run(png(&image), options(|o| {
            o.rotate = Rotation::DEG_90;
            o.crop = Some(rotated_region);
        }));
        let two = crop(rotate(png(&image), 90), rotated_region);
        let crop_first = rotate(crop(png(&image), region), 90);
        prop_assert_eq!(pixels(&one), pixels(&two));
        prop_assert_eq!(pixels(&one), pixels(&crop_first));
    }

    /// A lossless format gives back the pixels it was handed, so a round trip through it and
    /// back to PNG is the identity.
    #[test]
    fn a_lossless_round_trip_keeps_the_pixels(
        image in image(true),
        format in prop_oneof![Just(MediaType::Webp), Just(MediaType::Bmp), Just(MediaType::Tiff)],
    ) {
        let round_trip = convert(convert(png(&image), format), MediaType::Png);
        prop_assert_eq!(pixels(&round_trip), image);
    }

    /// A resize to the size the image already has, or one that `--without-enlargement` stops
    /// from growing it, leaves every pixel where it was.
    #[test]
    fn a_resize_that_changes_no_size_keeps_the_pixels(
        image in image(false),
        fit in prop_oneof![Just(truss::Fit::Cover), Just(truss::Fit::Fill), Just(truss::Fit::Inside)],
        extra in (0u32..8, 0u32..8),
    ) {
        let (width, height) = image.dimensions();
        let same = run(png(&image), options(|o| {
            o.width = Some(width);
            o.height = Some(height);
            o.fit = Some(fit);
        }));
        prop_assert_eq!(pixels(&same), image.clone());
        let bounded = run(png(&image), options(|o| {
            o.width = Some(width + extra.0);
            o.height = Some(height + extra.1);
            o.fit = Some(truss::Fit::Inside);
            o.without_enlargement = true;
        }));
        prop_assert_eq!(pixels(&bounded), image);
    }

    /// Grayscale is idempotent: desaturating a gray image changes nothing.
    #[test]
    fn grayscale_twice_is_grayscale_once(image in image(false)) {
        let gray = |bytes: Vec<u8>| run(bytes, options(|o| o.grayscale = true));
        let once = gray(png(&image));
        let twice = gray(once.clone());
        prop_assert_eq!(pixels(&once), pixels(&twice));
    }
}
