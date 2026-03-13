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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectionOutput {
    format: String,
    mime: String,
    width: Option<u32>,
    height: Option<u32>,
    has_alpha: Option<bool>,
    is_animated: bool,
}

fn render_inspection_json(artifact: &crate::Artifact) -> String {
    let output = InspectionOutput {
        format: artifact.media_type.as_name().to_string(),
        mime: artifact.media_type.as_mime().to_string(),
        width: artifact.metadata.width,
        height: artifact.metadata.height,
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
            },
        }
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
