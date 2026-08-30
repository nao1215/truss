//! `truss capabilities` — what this build of the binary can do, as JSON.
//!
//! An integrator that ships truss as a downloaded binary has to know which formats it
//! reads and writes, which pixel operations exist and in what order they run, and which
//! optional features were compiled in. Without a way to ask, that knowledge gets hardcoded
//! in the caller and drifts: a build without the `avif` feature still advertises AVIF, and
//! the pipeline order ends up discovered by experiment rather than declared.
//!
//! The WASM adapter has answered this question since it shipped (`getCapabilitiesJson`).
//! This is the same question from the command line.

use super::{CliError, EXIT_RUNTIME, runtime_error};
use crate::{Fit, MAX_DECODED_PIXELS, MAX_OUTPUT_PIXELS, MediaType, OptimizeMode, Position};
use serde::Serialize;
use std::io::Write;

/// The pixel stages a transform runs, in the order it runs them.
///
/// A caller that composes operations of its own has to know this: truss applies its options
/// in a fixed order regardless of the order they were given in, so a chain that needs a
/// different one has to be split across invocations. Declaring the order is what lets a
/// caller work that out without discovering it by experiment.
const PIPELINE: &[&str] = &[
    "autoOrient",
    "rotate",
    "crop",
    "resize",
    "blur",
    "sharpen",
    "grayscale",
    "watermark",
    "flatten",
    "encode",
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    version: &'static str,
    input_formats: Vec<&'static str>,
    output_formats: Vec<&'static str>,
    pipeline: &'static [&'static str],
    fit_modes: Vec<&'static str>,
    positions: Vec<&'static str>,
    optimize_modes: Vec<&'static str>,
    features: Features,
    limits: Limits,
}

/// The optional cargo features this binary was built with.
///
/// A release binary is not one thing: AVIF, SVG, lossy WebP, and each storage backend are
/// compile-time choices, and asking for one that is absent fails at transform time with a
/// `CapabilityMissing` error. Reporting them lets a caller refuse the work up front.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Features {
    avif: bool,
    svg: bool,
    webp_lossy: bool,
    server: bool,
    s3: bool,
    gcs: bool,
    azure: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Limits {
    max_input_pixels: u64,
    max_output_pixels: u64,
}

fn capabilities() -> Capabilities {
    Capabilities {
        version: env!("CARGO_PKG_VERSION"),
        input_formats: input_formats(),
        output_formats: output_formats(),
        pipeline: PIPELINE,
        fit_modes: vec![
            Fit::Contain.as_name(),
            Fit::Cover.as_name(),
            Fit::Fill.as_name(),
            Fit::Inside.as_name(),
        ],
        positions: vec![
            Position::Center.as_name(),
            Position::Top.as_name(),
            Position::Right.as_name(),
            Position::Bottom.as_name(),
            Position::Left.as_name(),
            Position::TopLeft.as_name(),
            Position::TopRight.as_name(),
            Position::BottomLeft.as_name(),
            Position::BottomRight.as_name(),
        ],
        optimize_modes: vec![
            OptimizeMode::None.as_name(),
            OptimizeMode::Auto.as_name(),
            OptimizeMode::Lossless.as_name(),
            OptimizeMode::Lossy.as_name(),
        ],
        features: Features {
            avif: cfg!(feature = "avif"),
            svg: cfg!(feature = "svg"),
            webp_lossy: cfg!(feature = "webp-lossy"),
            server: cfg!(feature = "server"),
            s3: cfg!(feature = "s3"),
            gcs: cfg!(feature = "gcs"),
            azure: cfg!(feature = "azure"),
        },
        limits: Limits {
            max_input_pixels: MAX_DECODED_PIXELS,
            max_output_pixels: MAX_OUTPUT_PIXELS,
        },
    }
}

/// Formats this build decodes.
///
/// GIF is here and absent from the output list: truss reads it and never writes it.
fn input_formats() -> Vec<&'static str> {
    let mut formats = vec![
        MediaType::Jpeg.as_name(),
        MediaType::Png.as_name(),
        MediaType::Webp.as_name(),
    ];
    if cfg!(feature = "avif") {
        formats.push(MediaType::Avif.as_name());
    }
    if cfg!(feature = "svg") {
        formats.push(MediaType::Svg.as_name());
    }
    formats.push(MediaType::Bmp.as_name());
    formats.push(MediaType::Tiff.as_name());
    formats.push(MediaType::Gif.as_name());
    formats
}

/// Formats this build encodes.
///
/// SVG output is sanitize-only and requires an SVG input; every other entry accepts any
/// input this build can decode.
fn output_formats() -> Vec<&'static str> {
    let mut formats = vec![
        MediaType::Jpeg.as_name(),
        MediaType::Png.as_name(),
        MediaType::Webp.as_name(),
    ];
    if cfg!(feature = "avif") {
        formats.push(MediaType::Avif.as_name());
    }
    if cfg!(feature = "svg") {
        formats.push(MediaType::Svg.as_name());
    }
    formats.push(MediaType::Bmp.as_name());
    formats.push(MediaType::Tiff.as_name());
    formats
}

pub(super) fn execute_capabilities<W>(stdout: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    let mut json =
        serde_json::to_string_pretty(&capabilities()).expect("serialization cannot fail");
    json.push('\n');

    stdout
        .write_all(json.as_bytes())
        .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("failed to write output: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered() -> serde_json::Value {
        let mut stdout = Vec::new();
        execute_capabilities(&mut stdout).expect("render capabilities");
        serde_json::from_slice(&stdout).expect("valid json")
    }

    #[test]
    fn capabilities_reports_the_crate_version() {
        assert_eq!(rendered()["version"], env!("CARGO_PKG_VERSION"));
    }

    /// GIF is decode-only, and the lists have to say so: a caller reading `outputFormats`
    /// and offering GIF would produce requests truss answers with 415.
    #[test]
    fn capabilities_lists_gif_as_input_only() {
        let json = rendered();
        assert!(
            json["inputFormats"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("gif"))
        );
        assert!(
            !json["outputFormats"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("gif"))
        );
    }

    /// A format that needs a compile-time feature is listed only when it is present, or a
    /// caller learns the build cannot do it by trying and failing.
    #[test]
    fn capabilities_gates_optional_formats_on_the_build() {
        let json = rendered();
        let inputs = json["inputFormats"].as_array().unwrap().clone();
        let outputs = json["outputFormats"].as_array().unwrap().clone();

        assert_eq!(
            inputs.contains(&serde_json::json!("avif")),
            cfg!(feature = "avif")
        );
        assert_eq!(
            outputs.contains(&serde_json::json!("avif")),
            cfg!(feature = "avif")
        );
        assert_eq!(
            inputs.contains(&serde_json::json!("svg")),
            cfg!(feature = "svg")
        );
    }

    #[test]
    fn capabilities_reports_the_build_features() {
        let json = rendered();
        assert_eq!(json["features"]["avif"], cfg!(feature = "avif"));
        assert_eq!(json["features"]["svg"], cfg!(feature = "svg"));
        assert_eq!(json["features"]["webpLossy"], cfg!(feature = "webp-lossy"));
        assert_eq!(json["features"]["server"], cfg!(feature = "server"));
        assert_eq!(json["features"]["s3"], cfg!(feature = "s3"));
        assert_eq!(json["features"]["gcs"], cfg!(feature = "gcs"));
        assert_eq!(json["features"]["azure"], cfg!(feature = "azure"));
    }

    #[test]
    fn capabilities_reports_the_pixel_limits() {
        let json = rendered();
        assert_eq!(json["limits"]["maxInputPixels"], MAX_DECODED_PIXELS);
        assert_eq!(json["limits"]["maxOutputPixels"], MAX_OUTPUT_PIXELS);
    }

    /// The order here is a contract, not a description. `codecs::raster` has behavioral
    /// tests for the parts of it a caller depends on: rotation before the crop, the crop
    /// before the resize, the resize before the watermark.
    #[test]
    fn capabilities_declares_the_pipeline_order() {
        let json = rendered();
        let pipeline: Vec<String> = json["pipeline"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_string())
            .collect();

        assert_eq!(
            pipeline,
            vec![
                "autoOrient",
                "rotate",
                "crop",
                "resize",
                "blur",
                "sharpen",
                "grayscale",
                "watermark",
                "flatten",
                "encode",
            ]
        );
    }

    #[test]
    fn capabilities_lists_the_option_vocabularies() {
        let json = rendered();
        assert_eq!(
            json["fitModes"],
            serde_json::json!(["contain", "cover", "fill", "inside"])
        );
        assert_eq!(
            json["optimizeModes"],
            serde_json::json!(["none", "auto", "lossless", "lossy"])
        );
        assert_eq!(json["positions"].as_array().unwrap().len(), 9);
    }

    #[test]
    fn capabilities_output_ends_with_a_newline() {
        let mut stdout = Vec::new();
        execute_capabilities(&mut stdout).expect("render capabilities");
        assert_eq!(stdout.last(), Some(&b'\n'));
    }
}
