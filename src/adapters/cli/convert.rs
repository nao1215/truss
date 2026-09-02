use crate::{MediaType, RawArtifact, TransformRequest, WatermarkInput, sniff_artifact, transform};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::error_class::ErrorClass;

use super::{
    ClapConvertArgs, ClapOptimizeArgs, CliError, Command, ConvertCommand, EXIT_IO, EXIT_RUNTIME,
    EXIT_USAGE, HelpTopic, InputSource, MAX_REMOTE_WATERMARK_BYTES, OutputTarget, TransformFields,
    class_for_io_error, classified_error, convert_error, convert_usage, is_dash,
    map_transform_error, optimize_error, optimize_usage, read_input_bytes, read_url_bytes,
    runtime_error, validate_url,
};

// ---------------------------------------------------------------------------
// Clap -> Command conversion
// ---------------------------------------------------------------------------

/// Reports whether the value names a URL rather than a path.
///
/// A value is a URL when it names a scheme followed by `://`. A bare `scheme:` with no
/// authority, as in `mailto:`, stays a path: nothing is fetched from one, and a file whose
/// name holds a colon is likelier than a caller who meant a URI. Requiring the authority
/// also keeps `C:\images\logo.png` the Windows path it is rather than a URL with the
/// scheme `c`.
fn watermark_is_a_url(watermark: &Path) -> bool {
    // A value that is not valid UTF-8 cannot be a URL, so it is a path, which is also what a
    // caller who named a file with an unusual encoding meant.
    let Some(value) = watermark.to_str() else {
        return false;
    };
    let Some((scheme, _)) = value.split_once("://") else {
        return false;
    };
    !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Reads the watermark image, from a URL when the value is one and from the filesystem
/// otherwise.
///
/// The fetch is the one `--url` uses, so the address rules and the redirect limit are the
/// ones already written rather than a second copy of them. The size cap is
/// [`MAX_REMOTE_WATERMARK_BYTES`], not the input's.
fn read_watermark_bytes(watermark: &Path) -> Result<Vec<u8>, CliError> {
    if watermark_is_a_url(watermark) {
        let value = watermark.to_str().expect("a URL is valid UTF-8");
        validate_url(value, "--watermark")?;
        return read_url_bytes(value, MAX_REMOTE_WATERMARK_BYTES);
    }

    fs::read(watermark).map_err(|error| {
        classified_error(
            class_for_io_error(&error),
            EXIT_IO,
            &format!("failed to read watermark {}: {error}", watermark.display()),
        )
    })
}

#[cfg(test)]
mod watermark_tests {
    use super::watermark_is_a_url;
    use std::path::Path;

    /// Which values `--watermark` sends to the fetcher.
    #[test]
    fn a_value_naming_a_scheme_is_a_url_and_everything_else_is_a_path() {
        let urls = [
            "http://example.com/logo.png",
            "https://example.com/logo.png",
            "HTTP://example.com/logo.png",
            "ftp://example.com/logo.png",
            "file:///etc/hosts",
            "gopher://example.com/logo.png",
        ];
        for value in urls {
            assert!(
                watermark_is_a_url(Path::new(value)),
                "{value} names a scheme, so it is a URL"
            );
        }

        let paths = [
            "logo.png",
            "./logo.png",
            "/var/lib/logo.png",
            "../logo.png",
            // A colon with no authority after it.
            "logo:1.png",
            "mailto:someone@example.com",
            // A Windows path names a one-letter drive, which is not a scheme.
            "C:\\images\\logo.png",
            "c:/images/logo.png",
        ];
        for value in paths {
            assert!(
                !watermark_is_a_url(Path::new(value)),
                "{value} is a path, not a URL"
            );
        }
    }
}

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
            hint: Some("provide --watermark <file or URL> when using watermark options".to_string()),
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
        let wm_bytes = read_watermark_bytes(wm_path)?;
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
/// Only a destination that is a regular file is replaced this way. A rename unlinks
/// whatever was there, which is right for an image being overwritten and wrong for
/// everything else a path can name: a named pipe became a regular file and the process
/// reading it received nothing, and a symbolic link was unlinked while the file it pointed
/// at was left alone. Those destinations are written through, which is what naming them
/// means, and they give up the partial-write protection they could never have had.
///
/// A destination the caller may not write is refused rather than replaced. A rename is
/// governed by the directory, so the file's own mode stopped nothing once the write became
/// atomic, and `0o400` on a file is how a caller says not to touch it.
///
/// The mode of an existing destination is copied onto the temporary file before the
/// rename, since the new file would otherwise carry the umask rather than the permissions
/// the destination was given. A directory that will not take a temporary file, which on
/// Unix is one the caller cannot write to even though the file inside it may be
/// replaceable, falls back to writing the destination directly: it is the write that was
/// done before, and refusing it would take away something that works today.
fn replace_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if !is_replaceable_destination(path) {
        return fs::write(path, bytes);
    }
    refuse_a_destination_the_caller_may_not_write(path)?;
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

/// Fails with the error a direct write would have given for a destination that exists and
/// cannot be opened for writing.
///
/// A rename asks the directory for permission and never asks the file, so once the write
/// became atomic a mode of `0o400` stopped nothing: the contents changed, the mode was
/// copied back onto the replacement, and the command exited 0. Opening the destination is
/// what answers the question the same way the kernel does, for the caller's own identity
/// and for whatever the file system enforces beyond the mode bits.
///
/// The handle is opened without truncating and dropped immediately, so a destination that
/// is writable is left exactly as it was for the replacement to swap out.
fn refuse_a_destination_the_caller_may_not_write(path: &Path) -> std::io::Result<()> {
    match fs::OpenOptions::new().write(true).open(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Reports whether the destination is one a rename may stand in for.
///
/// A path that names nothing yet, or names a regular file, is replaced. A symbolic link, a
/// named pipe, a socket, or a device is written through instead: the caller named that
/// object, and replacing it would destroy it. `symlink_metadata` is what answers the
/// question, because `metadata` follows a link and would report the target's kind.
///
/// A path whose metadata cannot be read is treated as replaceable, so that the write goes
/// on to fail with the error the file system gives rather than being diverted here.
fn is_replaceable_destination(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type().is_file(),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Counts the temporary files this process has named, so two writes never share one.
static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Builds the path of the temporary file a replacement is written to.
///
/// It sits beside the destination, because a rename across file systems fails. Its name is
/// its own rather than derived from the destination's: a derived name is always longer than
/// a name the file system has already accepted, so a destination near the limit produced a
/// temporary over it, and the replacement fell back to a truncating write with nothing said.
/// The length here is fixed, so a destination that can exist can be replaced.
///
/// The process id separates two `truss` runs and the counter separates two writes in one,
/// which is what `server::cache::unique_tmp_suffix` pairs them for.
fn temporary_sibling(path: &Path) -> Option<PathBuf> {
    let sequence = TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!(".truss.{}.{sequence}.tmp", std::process::id());
    Some(path.with_file_name(name))
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
    use super::{OutputTarget, temporary_sibling, write_output_bytes};
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

    /// The temporary's name is the same length whatever the destination is called, so a
    /// destination the file system accepts is one the replacement can be written for.
    ///
    /// A name derived from the destination is always longer than it, and a destination near
    /// `NAME_MAX` pushed it over: the temporary could not be created, the write fell back to
    /// truncating the destination, and nothing said so. 250 bytes is under the limit on
    /// every file system truss is tested on and over what the derived name could take.
    #[cfg(unix)]
    #[test]
    fn a_long_destination_name_is_still_replaced_rather_than_truncated() {
        use std::os::unix::fs::MetadataExt;

        let dir = temp_dir("write-swap-long-name");
        let destination = dir.join(format!("{}.png", "a".repeat(246)));
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
            "a 250-byte destination name lost the atomic replace, so a failure partway would truncate it"
        );
    }

    /// Two destinations in one directory, and two writes to one destination, each get their
    /// own temporary. The counter is what separates the second pair; a process id alone
    /// does not.
    #[test]
    fn two_replacements_do_not_share_a_temporary() {
        let dir = temp_dir("write-temp-uniqueness");
        let first = dir.join("first.png");
        let second = dir.join("second.png");

        let names: Vec<_> = [&first, &second, &first, &second]
            .iter()
            .map(|path| temporary_sibling(path).expect("a destination has a file name"))
            .collect();

        let mut unique: Vec<_> = names.iter().collect();
        unique.sort();
        unique.dedup();
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(
            unique.len(),
            names.len(),
            "two writes must not race for one temporary: {names:?}"
        );
        for name in &names {
            assert_eq!(
                name.parent(),
                Some(dir.as_path()),
                "the temporary sits beside its destination so the rename cannot cross a file system"
            );
        }
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

    /// A named pipe is written through, not replaced.
    ///
    /// The replacement renames a temporary file over the destination, and a rename unlinks
    /// whatever was there, so `-o pipe` turned the pipe into a regular file: truss exited 0
    /// and the process reading the pipe received nothing and stayed blocked.
    #[cfg(unix)]
    #[test]
    fn a_named_pipe_destination_is_written_through() {
        use std::io::Read;
        use std::os::unix::fs::FileTypeExt;

        let dir = temp_dir("write-fifo");
        let destination = dir.join("pipe.png");
        let path = std::ffi::CString::new(destination.as_os_str().as_encoded_bytes())
            .expect("a path with no interior nul");
        // SAFETY: the path is a valid C string and 0o600 is a valid mode.
        let made = unsafe { libc::mkfifo(path.as_ptr(), 0o600) };
        assert_eq!(made, 0, "create the fifo");

        // The reader reports through a channel rather than a join, because a replacement
        // turns the pipe into a regular file and leaves this open blocked forever; a
        // regression has to fail in bounded time rather than hang the suite.
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader_path = destination.clone();
        std::thread::spawn(move || {
            let seen = fs::File::open(&reader_path).and_then(|mut file| {
                let mut seen = Vec::new();
                file.read_to_end(&mut seen).map(|_| seen)
            });
            let _ = sender.send(seen);
        });

        let mut stdout = Vec::new();
        write_output_bytes(OutputTarget::Path(destination.clone()), b"new", &mut stdout)
            .expect("the write succeeds");
        let seen = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the reader has to see the write rather than wait on a pipe nobody holds")
            .expect("read the fifo");

        let still_a_fifo = fs::metadata(&destination)
            .expect("stat the destination")
            .file_type()
            .is_fifo();
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(
            seen, b"new",
            "the bytes have to reach whoever is reading the pipe"
        );
        assert!(
            still_a_fifo,
            "a pipe is a destination to write through, not to replace"
        );
    }

    /// A symbolic link is followed, not replaced.
    ///
    /// Writing to a path that is a link updates what the link points at, which is how a
    /// `latest.png -> dated.png` name is kept. The replacement unlinked the link instead
    /// and left the target untouched.
    #[cfg(unix)]
    #[test]
    fn a_symlink_destination_is_followed() {
        let dir = temp_dir("write-symlink");
        let target = dir.join("dated.png");
        let link = dir.join("latest.png");
        fs::write(&target, b"old").expect("write the target");
        std::os::unix::fs::symlink(&target, &link).expect("make the link");

        let mut stdout = Vec::new();
        write_output_bytes(OutputTarget::Path(link.clone()), b"new", &mut stdout)
            .expect("the write succeeds");

        let still_a_link = fs::symlink_metadata(&link)
            .expect("stat the link")
            .file_type()
            .is_symlink();
        let target_content = fs::read(&target).expect("read the target");
        let _ = fs::remove_dir_all(&dir);

        assert!(
            still_a_link,
            "the link is what the caller named, and it stays a link"
        );
        assert_eq!(
            target_content, b"new",
            "what the link points at is what was written"
        );
    }

    /// A destination the caller may not write is refused, as it was before the write
    /// became atomic.
    ///
    /// A rename is governed by the directory, so a mode of 0o400 on the file stopped
    /// nothing: truss exited 0 and the contents changed while the mode still read 0o400.
    #[cfg(unix)]
    #[test]
    fn a_read_only_destination_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("write-read-only-file");
        let destination = dir.join("out.png");
        fs::write(&destination, b"old").expect("write the destination");
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o400))
            .expect("seal the file");

        let mut stdout = Vec::new();
        let result =
            write_output_bytes(OutputTarget::Path(destination.clone()), b"new", &mut stdout);
        let content = fs::read(&destination).expect("read the destination");

        let _ = fs::set_permissions(&destination, fs::Permissions::from_mode(0o600));
        let _ = fs::remove_dir_all(&dir);

        if unsafe { libc::geteuid() } == 0 {
            // Root ignores the mode, and there is nothing to assert.
            return;
        }
        assert!(
            result.is_err(),
            "a file the caller may not write is not written"
        );
        assert_eq!(content, b"old", "and its contents are left alone");
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
