use crate::{MediaType, RawArtifact, TransformRequest, WatermarkInput, sniff_artifact, transform};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::core::error_class::ErrorClass;

use super::{
    ClapConvertArgs, ClapOptimizeArgs, CliError, Command, ConvertCommand, EXIT_IO, EXIT_RUNTIME,
    EXIT_USAGE, HelpTopic, InputSource, OutputTarget, TransformFields, class_for_io_error,
    classified_error, convert_error, convert_usage, is_dash, map_transform_error, optimize_error,
    optimize_usage, read_input_bytes, runtime_error, validate_url,
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
        (None, Some(value)) if is_dash(value) => InputSource::Stdin,
        (None, Some(value)) => InputSource::Path(value.clone()),
        (None, None) => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                class: ErrorClass::InvalidRequest,
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
        Some(ref value) if is_dash(value) => OutputTarget::Stdout,
        Some(ref value) => OutputTarget::Path(value.clone()),
        None => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                class: ErrorClass::InvalidRequest,
                message: "'convert' requires -o <output>".to_string(),
                usage: Some(convert_usage().to_string()),
                hint: Some("try 'truss convert input.png -o output.jpg'".to_string()),
            });
        }
    };

    if args.format.is_none() {
        reject_unencodable_output_extension(&output, convert_error)?;
    }

    let watermark_path = args.watermark.clone();
    if watermark_path.is_none()
        && (args.watermark_position.is_some()
            || args.watermark_opacity.is_some()
            || args.watermark_margin.is_some())
    {
        return Err(CliError {
            exit_code: EXIT_USAGE,
            class: ErrorClass::InvalidRequest,
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
        optimize: args.optimize,
        target_quality: args.target_quality,
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
        grayscale: args.grayscale,
        without_enlargement: args.without_enlargement,
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

pub(super) fn optimize_from_clap(args: ClapOptimizeArgs) -> Result<Command, CliError> {
    if args.help {
        return Ok(Command::Help(HelpTopic::Optimize));
    }

    let input = match (&args.url, &args.input) {
        (Some(url), None) => {
            validate_url(url, "--url")?;
            InputSource::Url(url.clone())
        }
        (None, Some(value)) if is_dash(value) => InputSource::Stdin,
        (None, Some(value)) => InputSource::Path(value.clone()),
        (None, None) => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                class: ErrorClass::InvalidRequest,
                message: "'optimize' requires an input file, URL, or -".to_string(),
                usage: Some(optimize_usage().to_string()),
                hint: Some("try 'truss optimize input.jpg -o output.jpg'".to_string()),
            });
        }
        (Some(_), Some(_)) => {
            return Err(optimize_error("'optimize' accepts exactly one input"));
        }
    };

    let output = match args.output {
        Some(ref value) if is_dash(value) => OutputTarget::Stdout,
        Some(ref value) => OutputTarget::Path(value.clone()),
        None => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                class: ErrorClass::InvalidRequest,
                message: "'optimize' requires -o <output>".to_string(),
                usage: Some(optimize_usage().to_string()),
                hint: Some("try 'truss optimize input.jpg -o output.jpg'".to_string()),
            });
        }
    };

    if args.format.is_none() {
        reject_unencodable_output_extension(&output, optimize_error)?;
    }

    let options = TransformFields {
        width: None,
        height: None,
        fit: None,
        position: None,
        format: args.format,
        quality: args.quality,
        optimize: Some(args.mode.unwrap_or(crate::OptimizeMode::Auto)),
        target_quality: args.target_quality,
        background: None,
        rotate: None,
        auto_orient: args.auto_orient,
        no_auto_orient: args.no_auto_orient,
        strip_metadata: args.strip_metadata,
        keep_metadata: args.keep_metadata,
        preserve_exif: args.preserve_exif,
        crop: None,
        blur: None,
        sharpen: None,
        grayscale: false,
        without_enlargement: false,
    }
    .into_options()
    .map_err(map_transform_error)?;

    Ok(Command::Optimize(ConvertCommand {
        input,
        output,
        options,
        watermark_path: None,
        watermark_position: None,
        watermark_opacity: None,
        watermark_margin: None,
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
    // The sniff fails with the same classes the transform does, so it reports them the
    // same way: `decode-failed` is exit 4 here as it is there, not exit 3 because this
    // call site happens to come first.
    let input = sniff_artifact(RawArtifact::new(bytes, None)).map_err(map_transform_error)?;

    let mut options = command.options;
    if options.format.is_none() {
        // Leave it None when the output gives no hint (stdout, or an extension truss does
        // not recognize) so `TransformOptions::normalize` picks the default. It resolves to
        // the input format, except for GIF input, which has no encoder and falls back to PNG.
        options.format = infer_output_format(&command.output);
    }

    let watermark = if let Some(ref wm_path) = command.watermark_path {
        let wm_bytes = fs::read(wm_path).map_err(|error| {
            classified_error(
                class_for_io_error(&error),
                EXIT_IO,
                &format!("failed to read watermark {}: {error}", wm_path.display()),
            )
        })?;
        let wm_artifact = sniff_artifact(RawArtifact::new(wm_bytes, None)).map_err(|error| {
            let mut failure = map_transform_error(error);
            failure.message = format!(
                "failed to decode watermark '{}': {}",
                wm_path.display(),
                failure.message
            );
            failure
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

/// Rejects an output extension that names a format truss never encodes.
///
/// `--format gif` is refused by the flag's value parser with the alternatives spelled
/// out, but `-o out.gif` reached the same wall from the other side and surfaced
/// `unsupported output media type` from deep in the pipeline with a different exit code.
/// Both spellings ask for the same impossible thing, so both are usage errors now.
fn reject_unencodable_output_extension<F>(output: &OutputTarget, error: F) -> Result<(), CliError>
where
    F: Fn(&str) -> CliError,
{
    let Some(media_type) = infer_output_format(output) else {
        return Ok(());
    };
    match media_type.unencodable_reason() {
        Some(reason) => Err(error(&reason)),
        None => Ok(()),
    }
}

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
        OutputTarget::Path(path) => replace_file(&path, bytes).map_err(|error| {
            classified_error(
                class_for_io_error(&error),
                EXIT_IO,
                &format!("failed to write {}: {error}", path.display()),
            )
        }),
    }
}

/// Replaces a file with new bytes, or leaves it exactly as it was.
///
/// `fs::write` opens the destination with `O_TRUNC`, so a write that stops partway, on a
/// full disk or a filled quota, leaves a file that is shorter than the image it held and
/// still looks like an image to anything that reads its header. Converting in place makes
/// that the only copy. The bytes therefore go to a temporary file in the destination's own
/// directory, which is renamed over the destination once every byte is in it; a rename
/// within one directory is atomic, so a reader sees the old file or the new one.
///
/// This is what `server::cache` does for a cache entry, for the same reason.
///
/// The mode of an existing destination is copied onto the temporary file before the
/// rename, since the new file would otherwise carry the umask rather than the permissions
/// the destination was given. A directory that will not take a temporary file, which on
/// Unix is one the caller cannot write to even though the file inside it may be
/// replaceable, falls back to writing the destination directly: it is the write that was
/// done before, and refusing it would take away something that works today.
fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let Some(temporary) = temporary_sibling(path) else {
        return fs::write(path, bytes);
    };
    let Ok(mut file) = fs::File::create(&temporary) else {
        return fs::write(path, bytes);
    };

    let outcome = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        drop(file);
        copy_permissions(path, &temporary);
        fs::rename(&temporary, path)
    })();

    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

/// Builds the path of the temporary file a replacement is written to.
///
/// It sits beside the destination, because a rename across file systems fails, and it
/// carries the process id so two `truss` runs writing the same output do not collide.
fn temporary_sibling(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let mut candidate = OsString::from(".");
    candidate.push(name);
    candidate.push(format!(".tmp.{}", std::process::id()));
    Some(path.with_file_name(candidate))
}

/// Gives the replacement the permissions the destination already had.
///
/// A destination that is not there yet has none to copy, and a file system that does not
/// carry Unix modes leaves the temporary file as it was created; neither is a failure of
/// the write, so nothing is reported.
fn copy_permissions(path: &Path, temporary: &Path) {
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(temporary, metadata.permissions());
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputTarget, write_output_bytes};
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let path = crate::test_support::unique_temp_path(&format!("truss-{name}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    /// A replacement swaps a new file in rather than emptying the old one.
    ///
    /// `fs::write` opens the destination and truncates it, so a write that stops partway,
    /// on a full disk or a filled quota, leaves a file shorter than the image it held. The
    /// bytes go to a temporary file and are renamed over the destination instead, which
    /// this checks by the inode: a truncating write keeps it, a rename replaces it. The
    /// process-wide file size limit would reproduce the partial write directly, and it is
    /// not used here because it applies to every test running beside this one.
    #[cfg(unix)]
    #[test]
    fn replacing_a_file_swaps_it_in_rather_than_truncating_it() {
        use std::os::unix::fs::MetadataExt;

        let dir = temp_dir("write-swap");
        let destination = dir.join("out.png");
        fs::write(&destination, b"the bytes that were already there").expect("write it");
        let before = fs::metadata(&destination).expect("stat it").ino();

        let mut stdout = Vec::new();
        write_output_bytes(OutputTarget::Path(destination.clone()), b"new", &mut stdout)
            .expect("the write succeeds");

        let after = fs::metadata(&destination).expect("stat it again").ino();
        let content = fs::read(&destination).expect("read it back");
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(content, b"new");
        assert_ne!(
            before, after,
            "the destination was written in place, so a failure partway would truncate it"
        );
    }

    /// A replacement that cannot be completed leaves the destination alone and takes its
    /// temporary file with it.
    #[test]
    fn a_replacement_that_fails_leaves_nothing_behind() {
        let dir = temp_dir("write-failure");
        // A non-empty directory cannot be renamed over, and it is a destination whose
        // contents are observable afterwards.
        let destination = dir.join("occupied");
        fs::create_dir(&destination).expect("create the destination directory");
        fs::write(destination.join("inside"), b"still here").expect("fill it");

        let mut stdout = Vec::new();
        let result =
            write_output_bytes(OutputTarget::Path(destination.clone()), b"new", &mut stdout);

        let inside = fs::read(destination.join("inside")).expect("read what was inside");
        let leftovers: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("list the directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path != &destination)
            .collect();
        let _ = fs::remove_dir_all(&dir);

        assert!(result.is_err(), "the write failed and has to say so");
        assert_eq!(inside, b"still here");
        assert!(
            leftovers.is_empty(),
            "a failed write left files behind: {leftovers:?}"
        );
    }

    /// A successful write replaces the file and leaves nothing beside it.
    #[test]
    fn a_successful_write_leaves_no_other_file_behind() {
        let dir = temp_dir("write-success");
        let destination = dir.join("out.png");
        fs::write(&destination, b"old").expect("write the destination");

        let mut stdout = Vec::new();
        write_output_bytes(OutputTarget::Path(destination.clone()), b"new", &mut stdout)
            .expect("the write succeeds");

        let entries: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("list the directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        let content = fs::read(&destination).expect("read the destination");
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(content, b"new");
        assert_eq!(entries.len(), 1, "the directory holds only the output");
    }

    /// Replacing a file keeps the permissions it had, which a rename over it would not
    /// do on its own: the new inode would carry the umask instead.
    #[cfg(unix)]
    #[test]
    fn replacing_a_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("write-permissions");
        let destination = dir.join("out.png");
        fs::write(&destination, b"old").expect("write the destination");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o640))
            .expect("set the destination mode");

        let mut stdout = Vec::new();
        write_output_bytes(OutputTarget::Path(destination.clone()), b"new", &mut stdout)
            .expect("the write succeeds");

        let mode = fs::metadata(&destination)
            .expect("read the destination metadata")
            .permissions()
            .mode()
            & 0o777;
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(mode, 0o640, "the mode of the replaced file is kept");
    }

    /// A directory that cannot be written to is still a directory whose files can be
    /// replaced, which is what the file's own mode decides on Unix. Nothing that worked
    /// before the write became atomic stops working.
    #[cfg(unix)]
    #[test]
    fn a_file_in_a_read_only_directory_is_still_replaced() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("write-read-only-dir");
        let destination = dir.join("out.png");
        fs::write(&destination, b"old").expect("write the destination");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("seal the directory");

        let mut stdout = Vec::new();
        let result =
            write_output_bytes(OutputTarget::Path(destination.clone()), b"new", &mut stdout);
        let content = fs::read(&destination).expect("read the destination");

        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&dir);

        if result.is_err() {
            // Running as root ignores the directory mode, and there is nothing to assert.
            return;
        }
        assert_eq!(content, b"new");
    }
}
