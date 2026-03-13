use crate::{
    MediaType, RawArtifact, TransformRequest, WatermarkInput, sniff_artifact, transform,
};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{
    ClapConvertArgs, CliError, Command, ConvertCommand, EXIT_INPUT, EXIT_IO, EXIT_RUNTIME,
    EXIT_USAGE, HelpTopic, InputSource, OutputTarget, TransformFields, convert_error,
    convert_usage, map_transform_error, read_input_bytes, runtime_error, validate_url,
};

// ---------------------------------------------------------------------------
// Clap -> Command conversion
// ---------------------------------------------------------------------------

pub(super) fn convert_from_clap(args: ClapConvertArgs) -> Result<Command, CliError> {
    if args.help {
        return Ok(Command::Help(HelpTopic::Convert));
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
                message: "'convert' requires an input file, URL, or -".to_string(),
                usage: Some(convert_usage().to_string()),
                hint: Some("try 'truss convert input.png -o output.jpg'".to_string()),
            });
        }
        (Some(_), Some(_)) => {
            return Err(convert_error("'convert' accepts exactly one input"));
        }
    };

    let output = match args.output {
        Some(ref value) if value == "-" => OutputTarget::Stdout,
        Some(ref value) => OutputTarget::Path(PathBuf::from(value)),
        None => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                message: "'convert' requires -o <output>".to_string(),
                usage: Some(convert_usage().to_string()),
                hint: Some("try 'truss convert input.png -o output.jpg'".to_string()),
            });
        }
    };

    let watermark_path = args.watermark.clone();
    if watermark_path.is_none()
        && (args.watermark_position.is_some()
            || args.watermark_opacity.is_some()
            || args.watermark_margin.is_some())
    {
        return Err(CliError {
            exit_code: EXIT_USAGE,
            message: "--watermark-position, --watermark-opacity, and --watermark-margin require --watermark".to_string(),
            usage: Some(convert_usage().to_string()),
            hint: Some("provide --watermark <path> when using watermark options".to_string()),
        });
    }
    let watermark_position = args.watermark_position;
    let watermark_opacity = args.watermark_opacity;
    let watermark_margin = args.watermark_margin;

    let options = TransformFields {
        width: args.width,
        height: args.height,
        fit: args.fit,
        position: args.position,
        format: args.format,
        quality: args.quality,
        background: args.background,
        rotate: args.rotate,
        auto_orient: args.auto_orient,
        no_auto_orient: args.no_auto_orient,
        strip_metadata: args.strip_metadata,
        keep_metadata: args.keep_metadata,
        preserve_exif: args.preserve_exif,
        crop: args.crop,
        blur: args.blur,
        sharpen: args.sharpen,
    }
    .into_options()
    .map_err(map_transform_error)?;

    Ok(Command::Convert(ConvertCommand {
        input,
        output,
        options,
        watermark_path,
        watermark_position,
        watermark_opacity,
        watermark_margin,
    }))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

pub(super) fn execute_convert<R, W>(
    command: ConvertCommand,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<(), CliError>
where
    R: Read,
    W: Write,
{
    let bytes = read_input_bytes(command.input, stdin)?;
    let input = sniff_artifact(RawArtifact::new(bytes, None))
        .map_err(|error| runtime_error(EXIT_INPUT, &error.to_string()))?;

    let mut options = command.options;
    if options.format.is_none() {
        options.format = infer_output_format(&command.output).or(Some(input.media_type));
    }

    let watermark = if let Some(ref wm_path) = command.watermark_path {
        let wm_bytes = fs::read(wm_path).map_err(|error| {
            runtime_error(EXIT_IO, &format!("failed to read watermark file: {error}"))
        })?;
        let wm_artifact = sniff_artifact(RawArtifact::new(wm_bytes, None)).map_err(|error| {
            runtime_error(
                EXIT_INPUT,
                &format!(
                    "failed to decode watermark '{}': {error}",
                    wm_path.display()
                ),
            )
        })?;
        Some(WatermarkInput {
            image: wm_artifact,
            position: command
                .watermark_position
                .unwrap_or(crate::Position::BottomRight),
            opacity: command.watermark_opacity.unwrap_or(50),
            margin: command.watermark_margin.unwrap_or(10),
        })
    } else {
        None
    };

    let mut request = TransformRequest::new(input, options);
    request.watermark = watermark;
    let result = transform(request).map_err(map_transform_error)?;

    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }

    write_output_bytes(command.output, &result.artifact.bytes, stdout)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn infer_output_format(output: &OutputTarget) -> Option<MediaType> {
    match output {
        OutputTarget::Stdout => None,
        OutputTarget::Path(path) => infer_output_format_from_path(path),
    }
}

fn infer_output_format_from_path(path: &Path) -> Option<MediaType> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    std::str::FromStr::from_str(&extension).ok()
}

fn write_output_bytes<W>(output: OutputTarget, bytes: &[u8], stdout: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    match output {
        OutputTarget::Stdout => stdout.write_all(bytes).map_err(|error| {
            runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
        }),
        OutputTarget::Path(path) => fs::write(&path, bytes).map_err(|error| {
            runtime_error(
                EXIT_IO,
                &format!("failed to write {}: {error}", path.display()),
            )
        }),
    }
}
