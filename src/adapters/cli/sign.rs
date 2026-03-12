use crate::adapters::server::{self, SignedUrlSource, sign_public_url};
use std::io::Write;

use super::{
    ClapSignArgs, CliError, Command, EXIT_RUNTIME, EXIT_TRANSFORM, EXIT_USAGE, HelpTopic,
    SignCommand, TransformFields, map_transform_error, runtime_error, sign_error,
};

// ---------------------------------------------------------------------------
// Clap -> Command conversion
// ---------------------------------------------------------------------------

pub(super) fn sign_from_clap(args: ClapSignArgs) -> Result<Command, CliError> {
    if args.help {
        return Ok(Command::Help(HelpTopic::Sign));
    }

    let base_url = args
        .base_url
        .ok_or_else(|| sign_error("'sign' requires --base-url"))?;

    let source = match (args.path, args.url) {
        (Some(path), None) => SignedUrlSource::Path {
            path,
            version: args.version,
        },
        (None, Some(url)) => SignedUrlSource::Url {
            url,
            version: args.version,
        },
        (None, None) => {
            return Err(sign_error("'sign' requires exactly one of --path or --url"));
        }
        (Some(_), Some(_)) => {
            return Err(sign_error("'sign' accepts exactly one of --path or --url"));
        }
    };

    let key_id = args
        .key_id
        .ok_or_else(|| sign_error("'sign' requires --key-id"))?;
    let secret = args
        .secret
        .ok_or_else(|| sign_error("'sign' requires --secret"))?;
    let expires = args
        .expires
        .ok_or_else(|| sign_error("'sign' requires --expires"))?;

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

    Ok(Command::Sign(SignCommand {
        base_url,
        source,
        key_id,
        secret,
        expires,
        options,
        watermark_url: args.watermark_url,
        watermark_position: args.watermark_position,
        watermark_opacity: args.watermark_opacity,
        watermark_margin: args.watermark_margin,
        preset: args.preset,
    }))
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

pub(super) fn execute_sign<W>(command: SignCommand, stdout: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    if command.watermark_url.is_none()
        && (command.watermark_position.is_some()
            || command.watermark_opacity.is_some()
            || command.watermark_margin.is_some())
    {
        return Err(runtime_error(
            EXIT_USAGE,
            "--watermark-position, --watermark-opacity, and --watermark-margin require --watermark-url",
        ));
    }

    let watermark_params =
        command
            .watermark_url
            .as_ref()
            .map(|url| server::SignedWatermarkParams {
                url: url.clone(),
                position: command.watermark_position.map(|p| p.as_name().to_string()),
                opacity: command.watermark_opacity,
                margin: command.watermark_margin,
            });
    let url = sign_public_url(
        &command.base_url,
        command.source,
        &command.options,
        &command.key_id,
        &command.secret,
        command.expires,
        watermark_params.as_ref(),
        command.preset.as_deref(),
    )
    .map_err(|reason| runtime_error(EXIT_TRANSFORM, &reason))?;

    writeln!(stdout, "{url}").map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write output: {error}"))
    })?;

    Ok(())
}
