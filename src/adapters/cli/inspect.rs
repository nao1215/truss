use crate::{RawArtifact, sniff_artifact};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::PathBuf;

use super::{
    ClapInspectArgs, CliError, Command, EXIT_INPUT, EXIT_RUNTIME, EXIT_USAGE, HelpTopic,
    InputSource, InspectCommand, inspect_usage, read_input_bytes, runtime_error, validate_url,
};

// ---------------------------------------------------------------------------
// Clap -> Command conversion
// ---------------------------------------------------------------------------

pub(super) fn inspect_from_clap(args: ClapInspectArgs) -> Result<Command, CliError> {
    if args.help {
        return Ok(Command::Help(HelpTopic::Inspect));
    }

    let input = match (&args.url, &args.input) {
        (Some(url), None) => {
            validate_url(url, "--url")?;
            InputSource::Url(url.clone())
        }
        (None, Some(value)) if value == "-" => InputSource::Stdin,
        (None, Some(value)) => InputSource::Path(PathBuf::from(value)),
        (None, None) => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                message: "'inspect' requires an input file, URL, or -".to_string(),
                usage: Some(inspect_usage().to_string()),
                hint: Some(
                    "try 'truss inspect photo.jpg' or 'truss inspect --url https://...'"
                        .to_string(),
                ),
            });
        }
        (Some(_), Some(_)) => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                message: "'inspect' accepts exactly one input".to_string(),
                usage: Some(inspect_usage().to_string()),
                hint: Some("run 'truss inspect --help' for inspect options".to_string()),
            });
        }
    };

    Ok(Command::Inspect(InspectCommand { input }))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

pub(super) fn execute_inspect<R, W>(
    command: InspectCommand,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<(), CliError>
where
    R: Read,
    W: Write,
{
    let bytes = read_input_bytes(command.input, stdin)?;
    let artifact = sniff_artifact(RawArtifact::new(bytes, None))
        .map_err(|error| runtime_error(EXIT_INPUT, &error.to_string()))?;
    let json = render_inspection_json(&artifact);

    stdout.write_all(json.as_bytes()).map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write output: {error}"))
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// `width` and `height` are the dimensions as stored in the container.
/// `orientedWidth` and `orientedHeight` are the dimensions `truss convert` produces, which
/// differ whenever an EXIF orientation transposes the axes. They are always present, so a
/// caller recording dimensions for later markup can read those two and be right either way.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectionOutput {
    format: String,
    mime: String,
    width: Option<u32>,
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    orientation: Option<u16>,
    oriented_width: Option<u32>,
    oriented_height: Option<u32>,
    has_alpha: Option<bool>,
    is_animated: bool,
}

fn render_inspection_json(artifact: &crate::Artifact) -> String {
    let oriented = artifact.metadata.oriented_dimensions();
    let output = InspectionOutput {
        format: artifact.media_type.as_name().to_string(),
        mime: artifact.media_type.as_mime().to_string(),
        width: artifact.metadata.width,
        height: artifact.metadata.height,
        orientation: artifact.metadata.orientation,
        oriented_width: oriented.map(|dimensions| dimensions.width),
        oriented_height: oriented.map(|dimensions| dimensions.height),
        has_alpha: artifact.metadata.has_alpha,
        is_animated: artifact.metadata.frame_count > 1 || artifact.metadata.duration.is_some(),
    };
    let mut json = serde_json::to_string_pretty(&output).expect("serialization cannot fail");
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Artifact, ArtifactMetadata, MediaType};
    use std::time::Duration;

    fn make_artifact(
        media_type: MediaType,
        width: Option<u32>,
        height: Option<u32>,
        has_alpha: Option<bool>,
        frame_count: u32,
        duration: Option<Duration>,
    ) -> Artifact {
        Artifact {
            bytes: vec![],
            media_type,
            metadata: ArtifactMetadata {
                width,
                height,
                has_alpha,
                frame_count,
                duration,
                orientation: None,
            },
        }
    }

    fn make_oriented_artifact(width: u32, height: u32, orientation: Option<u16>) -> Artifact {
        Artifact {
            bytes: vec![],
            media_type: MediaType::Jpeg,
            metadata: ArtifactMetadata {
                width: Some(width),
                height: Some(height),
                has_alpha: Some(false),
                frame_count: 1,
                duration: None,
                orientation,
            },
        }
    }

    fn parse(artifact: &Artifact) -> serde_json::Value {
        serde_json::from_str(&render_inspection_json(artifact)).unwrap()
    }

    /// Orientations 5 to 8 include a quarter turn, so the oriented dimensions are swapped.
    /// The other values, and no tag at all, leave them equal to the stored ones.
    #[test]
    fn render_inspection_json_reports_the_oriented_dimensions() {
        for orientation in [None, Some(1), Some(2), Some(3), Some(4)] {
            let parsed = parse(&make_oriented_artifact(40, 20, orientation));
            assert_eq!(parsed["orientedWidth"], 40, "orientation {orientation:?}");
            assert_eq!(parsed["orientedHeight"], 20, "orientation {orientation:?}");
        }

        for orientation in [5, 6, 7, 8] {
            let parsed = parse(&make_oriented_artifact(40, 20, Some(orientation)));
            assert_eq!(parsed["orientation"], orientation);
            assert_eq!(parsed["orientedWidth"], 20, "orientation {orientation}");
            assert_eq!(parsed["orientedHeight"], 40, "orientation {orientation}");
        }
    }

    /// An input without the tag omits the field rather than reporting a value it does not have.
    #[test]
    fn render_inspection_json_omits_orientation_when_there_is_no_tag() {
        let parsed = parse(&make_oriented_artifact(40, 20, None));
        assert!(parsed.get("orientation").is_none());
    }

    /// The oriented fields are present even when the dimensions are unknown.
    #[test]
    fn render_inspection_json_reports_null_oriented_dimensions_for_an_unsized_input() {
        let artifact = make_artifact(MediaType::Svg, None, None, None, 1, None);
        let parsed = parse(&artifact);
        assert!(parsed["orientedWidth"].is_null());
        assert!(parsed["orientedHeight"].is_null());
    }

    #[test]
    fn render_inspection_json_static_image() {
        let artifact = make_artifact(MediaType::Png, Some(100), Some(200), Some(true), 1, None);
        let json = render_inspection_json(&artifact);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["format"], "png");
        assert_eq!(parsed["mime"], "image/png");
        assert_eq!(parsed["width"], 100);
        assert_eq!(parsed["height"], 200);
        assert_eq!(parsed["hasAlpha"], true);
        assert_eq!(parsed["isAnimated"], false);
    }

    #[test]
    fn render_inspection_json_animated_by_frame_count() {
        let artifact = make_artifact(MediaType::Webp, Some(50), Some(50), None, 5, None);
        let json = render_inspection_json(&artifact);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["isAnimated"], true);
        assert_eq!(parsed["format"], "webp");
    }

    #[test]
    fn render_inspection_json_animated_by_duration() {
        let artifact = make_artifact(
            MediaType::Webp,
            Some(50),
            Some(50),
            None,
            1,
            Some(Duration::from_millis(2500)),
        );
        let json = render_inspection_json(&artifact);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["isAnimated"], true);
    }

    #[test]
    fn render_inspection_json_ends_with_newline() {
        let artifact = make_artifact(MediaType::Jpeg, Some(10), Some(10), None, 1, None);
        let json = render_inspection_json(&artifact);
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn render_inspection_json_null_dimensions() {
        let artifact = make_artifact(MediaType::Svg, None, None, None, 1, None);
        let json = render_inspection_json(&artifact);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["width"].is_null());
        assert!(parsed["height"].is_null());
        assert!(parsed["hasAlpha"].is_null());
    }
}
