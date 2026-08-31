//! Helpers the unit tests share, compiled only for a test build.
//!
//! What belongs here is a fixture whose definition several modules would otherwise each
//! write out: what a plain PNG is, and how a test names a path of its own under the
//! temporary directory. What does not belong here is a helper one module uses, or one whose
//! shape is part of what a module is testing; those stay beside their tests.
//!
//! This is not `tests/common`. That module is shared by the integration tests, which are
//! separate binaries linking the crate from outside; this one is inside the crate and is
//! only ever built with `cfg(test)`.

use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// A PNG of one flat colour, encoded the way an encoder that is not this crate's would.
///
/// The colour is arbitrary and opaque. A test that cares about the pixels should build its
/// own image; this is for the tests that need a file that is unmistakably a PNG.
pub(crate) fn flat_png(width: u32, height: u32) -> Vec<u8> {
    let image = RgbaImage::from_pixel(width, height, Rgba([10, 20, 30, 255]));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, width, height, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

/// A path under the temporary directory that no other test holds.
///
/// The nanosecond clock is what makes it unique, so two tests running at once do not collide
/// and a leftover from an earlier run is never picked up. Nothing is created here; the caller
/// decides whether it wants a file or a directory.
pub(crate) fn unique_temp_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}
