use crate::adapters::server::{self, sign_public_url, ServerConfig, SignedUrlSource};
use crate::{
    sniff_artifact, transform_raster, Fit, MediaType, Position, RawArtifact, Rgba8, Rotation,
    TransformOptions, TransformRequest,
};
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

const MAX_REMOTE_BYTES: u64 = 32 * 1024 * 1024;

const HELP_TEXT: &str = "\
truss

USAGE:
  truss <INPUT> -o <OUTPUT> [OPTIONS]
  truss --url <URL> -o <OUTPUT> [OPTIONS]
  truss -o <OUTPUT> [OPTIONS] <INPUT>
  truss serve [--bind <ADDR>] [--storage-root <PATH>] [--public-base-url <URL>] [--signed-url-key-id <KEY_ID>] [--signed-url-secret <SECRET>] [--allow-insecure-url-sources]
  truss [--bind <ADDR>] [--storage-root <PATH>] [--public-base-url <URL>] [--signed-url-key-id <KEY_ID>] [--signed-url-secret <SECRET>] [--allow-insecure-url-sources]
  truss inspect <INPUT>
  truss inspect --url <URL>
  truss convert <INPUT> -o <OUTPUT> [OPTIONS]
  truss convert --url <URL> -o <OUTPUT> [OPTIONS]
  truss sign --base-url <URL> (--path <PATH> | --url <URL>) --key-id <KEY_ID> --secret <SECRET> --expires <UNIX_SECS> [OPTIONS]
  truss --help

OPTIONS FOR SERVE:
      --bind <ADDR>
      --storage-root <PATH>
      --public-base-url <URL>
      --signed-url-key-id <KEY_ID>
      --signed-url-secret <SECRET>
      --allow-insecure-url-sources

OPTIONS FOR CONVERT:
  -o, --output <OUTPUT>
      --width <PX>
      --height <PX>
      --fit <contain|cover|fill|inside>
      --position <center|top|right|bottom|left|top-left|top-right|bottom-left|bottom-right>
      --format <jpeg|png|webp|avif>
      --quality <1-100>
      --background <RRGGBB|RRGGBBAA>
      --rotate <0|90|180|270>
      --auto-orient
      --no-auto-orient
      --strip-metadata
      --keep-metadata
      --preserve-exif

OPTIONS FOR SIGN:
      --base-url <URL>
      --path <PATH>
      --url <URL>
      --version <VALUE>
      --key-id <KEY_ID>
      --secret <SECRET>
      --expires <UNIX_SECS>
      --width <PX>
      --height <PX>
      --fit <contain|cover|fill|inside>
      --position <center|top|right|bottom|left|top-left|top-right|bottom-left|bottom-right>
      --format <jpeg|png|webp|avif>
      --quality <1-100>
      --background <RRGGBB|RRGGBBAA>
      --rotate <0|90|180|270>
      --auto-orient
      --no-auto-orient
      --strip-metadata
      --keep-metadata
      --preserve-exif

NOTES:
  Omitting `convert` enters implicit convert mode.
  The server starts when `serve` or a server runtime flag is used.
  `sign` builds a public signed GET URL for `/images/by-path` or `/images/by-url`.
  `inspect` currently supports local files, `-` for stdin, and `--url`.
  `convert` currently supports local files, `-` for stdin, and `--url`.
";

/// Runs the command-line adapter and returns a process exit code.
///
/// This function is the stable entry point for the CLI adapter. It parses command-line
/// arguments, dispatches the selected subcommand, writes output to the process streams,
/// and converts adapter-specific failures into the documented numeric exit codes.
///
/// # Examples
///
/// ```no_run
/// use truss::adapters::cli;
///
/// let _ = cli::run(vec![
///     "truss".to_string(),
///     "input.png".to_string(),
///     "-o".to_string(),
///     "output.jpg".to_string(),
/// ]);
/// ```
///
/// ```no_run
/// use truss::adapters::cli;
///
/// let _ = cli::run(vec![
///     "truss".to_string(),
///     "--bind".to_string(),
///     "127.0.0.1:8080".to_string(),
/// ]);
/// ```
pub fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item = String>,
{
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();

    ExitCode::from(run_with_io(args, &mut stdin, &mut stdout, &mut stderr))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Help,
    Serve(ServeCommand),
    Inspect(InspectCommand),
    Convert(ConvertCommand),
    Sign(SignCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeCommand {
    bind_addr: Option<String>,
    storage_root: Option<PathBuf>,
    public_base_url: Option<String>,
    signed_url_key_id: Option<String>,
    signed_url_secret: Option<String>,
    allow_insecure_url_sources: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InspectCommand {
    input: InputSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConvertCommand {
    input: InputSource,
    output: OutputTarget,
    options: TransformOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SignCommand {
    base_url: String,
    source: SignedUrlSource,
    key_id: String,
    secret: String,
    expires: u64,
    options: TransformOptions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InputSource {
    Stdin,
    Path(PathBuf),
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutputTarget {
    Stdout,
    Path(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
    exit_code: u8,
    message: String,
}

fn run_with_io<I, R, W, E>(args: I, stdin: &mut R, stdout: &mut W, stderr: &mut E) -> u8
where
    I: IntoIterator<Item = String>,
    R: Read,
    W: Write,
    E: Write,
{
    match parse_args(args) {
        Ok(Command::Help) => match stdout.write_all(HELP_TEXT.as_bytes()) {
            Ok(_) => 0,
            Err(_) => 5,
        },
        Ok(Command::Serve(command)) => match execute_serve(command) {
            Ok(()) => 0,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Inspect(command)) => match execute_inspect(command, stdin, stdout) {
            Ok(()) => 0,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Convert(command)) => match execute_convert(command, stdin, stdout) {
            Ok(()) => 0,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Sign(command)) => match execute_sign(command, stdout) {
            Ok(()) => 0,
            Err(error) => write_error(stderr, error),
        },
        Err(error) => write_error(stderr, error),
    }
}

fn parse_args<I>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let _program_name = args.next();
    let remaining: Vec<String> = args.collect();
    let Some(command) = remaining.first() else {
        return Err(usage_error("`convert` requires an input path or `-`"));
    };

    match command.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "serve" => parse_serve_args(remaining[1..].to_vec()),
        "inspect" => parse_inspect_args(remaining[1..].to_vec()),
        "convert" => parse_convert_args(remaining[1..].to_vec()),
        "sign" => parse_sign_args(remaining[1..].to_vec()),
        value if is_serve_flag(value) => parse_serve_args(remaining),
        _ => parse_convert_args(remaining),
    }
}

fn is_serve_flag(value: &str) -> bool {
    matches!(
        value,
        "--bind"
            | "--storage-root"
            | "--public-base-url"
            | "--signed-url-key-id"
            | "--signed-url-secret"
            | "--allow-insecure-url-sources"
    )
}

fn parse_serve_args(args: Vec<String>) -> Result<Command, CliError> {
    let mut bind_addr = None;
    let mut storage_root = None;
    let mut public_base_url = None;
    let mut signed_url_key_id = None;
    let mut signed_url_secret = None;
    let mut allow_insecure_url_sources = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--bind" => {
                index += 1;
                let value = args
                    .get(index)
                    .cloned()
                    .ok_or_else(|| usage_error("`--bind` requires an address"))?;
                bind_addr = Some(value);
            }
            "--storage-root" => {
                index += 1;
                let value = required_arg(args.get(index), "--storage-root")?;
                storage_root = Some(PathBuf::from(value));
            }
            "--public-base-url" => {
                index += 1;
                let value = required_arg(args.get(index), "--public-base-url")?;
                public_base_url = Some(parse_url_arg(value, "--public-base-url")?);
            }
            "--signed-url-key-id" => {
                index += 1;
                signed_url_key_id =
                    Some(required_arg(args.get(index), "--signed-url-key-id")?.to_string());
            }
            "--signed-url-secret" => {
                index += 1;
                signed_url_secret =
                    Some(required_arg(args.get(index), "--signed-url-secret")?.to_string());
            }
            "--allow-insecure-url-sources" => allow_insecure_url_sources = true,
            unknown => {
                return Err(usage_error(&format!(
                    "unknown argument for `serve`: `{unknown}`"
                )));
            }
        }

        index += 1;
    }

    Ok(Command::Serve(ServeCommand {
        bind_addr,
        storage_root,
        public_base_url,
        signed_url_key_id,
        signed_url_secret,
        allow_insecure_url_sources,
    }))
}

fn parse_inspect_args(args: Vec<String>) -> Result<Command, CliError> {
    if args.is_empty() {
        return Err(usage_error("`inspect` requires an input path or `-`"));
    }

    let input = if args.len() == 2 && args[0] == "--url" {
        InputSource::Url(parse_url_arg(&args[1], "--url")?)
    } else if args.len() == 1 {
        parse_input_source(&args[0], "inspect")?
    } else {
        return Err(usage_error("`inspect` accepts exactly one input"));
    };

    Ok(Command::Inspect(InspectCommand { input }))
}

fn parse_convert_args(args: Vec<String>) -> Result<Command, CliError> {
    if args.is_empty() {
        return Err(usage_error("`convert` requires an input path or `-`"));
    }

    let mut index = 0;
    let mut input = None;
    let mut output = None;
    let mut options = TransformOptions::default();

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--url" => {
                index += 1;
                let value = required_arg(args.get(index), "--url")?;
                if input.is_some() {
                    return Err(usage_error("`convert` accepts exactly one input"));
                }
                input = Some(InputSource::Url(parse_url_arg(value, "--url")?));
            }
            "-o" | "--output" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| usage_error("`--output` requires a path or `-`"))?;
                output = Some(parse_output_target(value)?);
            }
            "--width" => {
                index += 1;
                options.width = Some(parse_u32_arg(args.get(index), "--width")?);
            }
            "--height" => {
                index += 1;
                options.height = Some(parse_u32_arg(args.get(index), "--height")?);
            }
            "--fit" => {
                index += 1;
                let value = required_arg(args.get(index), "--fit")?;
                options.fit = Some(parse_named(value, "--fit", Fit::from_str)?);
            }
            "--position" => {
                index += 1;
                let value = required_arg(args.get(index), "--position")?;
                options.position = Some(parse_named(value, "--position", Position::from_str)?);
            }
            "--format" => {
                index += 1;
                let value = required_arg(args.get(index), "--format")?;
                options.format = Some(parse_named(value, "--format", MediaType::from_str)?);
            }
            "--quality" => {
                index += 1;
                options.quality = Some(parse_u8_arg(args.get(index), "--quality")?);
            }
            "--background" => {
                index += 1;
                let value = required_arg(args.get(index), "--background")?;
                options.background = Some(parse_named(value, "--background", Rgba8::from_hex)?);
            }
            "--rotate" => {
                index += 1;
                let value = required_arg(args.get(index), "--rotate")?;
                options.rotate = parse_named(value, "--rotate", Rotation::from_str)?;
            }
            "--auto-orient" => options.auto_orient = true,
            "--no-auto-orient" => options.auto_orient = false,
            "--strip-metadata" => options.strip_metadata = true,
            "--keep-metadata" => options.strip_metadata = false,
            "--preserve-exif" => options.preserve_exif = true,
            "-" => {
                if input.is_some() {
                    return Err(usage_error("`convert` accepts exactly one input"));
                }
                input = Some(InputSource::Stdin);
            }
            value if value.starts_with('-') => {
                return Err(usage_error(&format!(
                    "unknown argument for `convert`: `{value}`"
                )));
            }
            value => {
                if input.is_some() {
                    return Err(usage_error("`convert` accepts exactly one input"));
                }
                input = Some(parse_input_source(value, "convert")?);
            }
        }

        index += 1;
    }

    let input = input.ok_or_else(|| usage_error("`convert` requires an input path or `-`"))?;
    let output = output.ok_or_else(|| usage_error("`convert` requires `--output`"))?;

    Ok(Command::Convert(ConvertCommand {
        input,
        output,
        options,
    }))
}

fn parse_sign_args(args: Vec<String>) -> Result<Command, CliError> {
    if args.is_empty() {
        return Err(usage_error(
            "`sign` requires `--base-url`, a source flag, credentials, and `--expires`",
        ));
    }

    let mut index = 0;
    let mut base_url = None;
    let mut path = None;
    let mut url = None;
    let mut version = None;
    let mut key_id = None;
    let mut secret = None;
    let mut expires = None;
    let mut options = TransformOptions::default();

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--base-url" => {
                index += 1;
                base_url = Some(parse_url_arg(
                    required_arg(args.get(index), "--base-url")?,
                    "--base-url",
                )?);
            }
            "--path" => {
                index += 1;
                if path.is_some() || url.is_some() {
                    return Err(usage_error(
                        "`sign` accepts exactly one of `--path` or `--url`",
                    ));
                }
                path = Some(required_arg(args.get(index), "--path")?.to_string());
            }
            "--url" => {
                index += 1;
                if path.is_some() || url.is_some() {
                    return Err(usage_error(
                        "`sign` accepts exactly one of `--path` or `--url`",
                    ));
                }
                url = Some(parse_url_arg(
                    required_arg(args.get(index), "--url")?,
                    "--url",
                )?);
            }
            "--version" => {
                index += 1;
                version = Some(required_arg(args.get(index), "--version")?.to_string());
            }
            "--key-id" => {
                index += 1;
                key_id = Some(required_arg(args.get(index), "--key-id")?.to_string());
            }
            "--secret" => {
                index += 1;
                secret = Some(required_arg(args.get(index), "--secret")?.to_string());
            }
            "--expires" => {
                index += 1;
                expires = Some(parse_u64_arg(args.get(index), "--expires")?);
            }
            "--width" => {
                index += 1;
                options.width = Some(parse_u32_arg(args.get(index), "--width")?);
            }
            "--height" => {
                index += 1;
                options.height = Some(parse_u32_arg(args.get(index), "--height")?);
            }
            "--fit" => {
                index += 1;
                let value = required_arg(args.get(index), "--fit")?;
                options.fit = Some(parse_named(value, "--fit", Fit::from_str)?);
            }
            "--position" => {
                index += 1;
                let value = required_arg(args.get(index), "--position")?;
                options.position = Some(parse_named(value, "--position", Position::from_str)?);
            }
            "--format" => {
                index += 1;
                let value = required_arg(args.get(index), "--format")?;
                options.format = Some(parse_named(value, "--format", MediaType::from_str)?);
            }
            "--quality" => {
                index += 1;
                options.quality = Some(parse_u8_arg(args.get(index), "--quality")?);
            }
            "--background" => {
                index += 1;
                let value = required_arg(args.get(index), "--background")?;
                options.background = Some(parse_named(value, "--background", Rgba8::from_hex)?);
            }
            "--rotate" => {
                index += 1;
                let value = required_arg(args.get(index), "--rotate")?;
                options.rotate = parse_named(value, "--rotate", Rotation::from_str)?;
            }
            "--auto-orient" => options.auto_orient = true,
            "--no-auto-orient" => options.auto_orient = false,
            "--strip-metadata" => options.strip_metadata = true,
            "--keep-metadata" => options.strip_metadata = false,
            "--preserve-exif" => options.preserve_exif = true,
            value => {
                return Err(usage_error(&format!(
                    "unknown argument for `sign`: `{value}`"
                )));
            }
        }

        index += 1;
    }

    let base_url = base_url.ok_or_else(|| usage_error("`sign` requires `--base-url`"))?;
    let source = match (path, url) {
        (Some(path), None) => SignedUrlSource::Path { path, version },
        (None, Some(url)) => SignedUrlSource::Url { url, version },
        (None, None) => {
            return Err(usage_error(
                "`sign` requires exactly one of `--path` or `--url`",
            ))
        }
        (Some(_), Some(_)) => {
            return Err(usage_error(
                "`sign` accepts exactly one of `--path` or `--url`",
            ))
        }
    };
    let key_id = key_id.ok_or_else(|| usage_error("`sign` requires `--key-id`"))?;
    let secret = secret.ok_or_else(|| usage_error("`sign` requires `--secret`"))?;
    let expires = expires.ok_or_else(|| usage_error("`sign` requires `--expires`"))?;

    Ok(Command::Sign(SignCommand {
        base_url,
        source,
        key_id,
        secret,
        expires,
        options,
    }))
}

fn execute_serve(command: ServeCommand) -> Result<(), CliError> {
    let bind_addr = command.bind_addr.clone().unwrap_or_else(server::bind_addr);
    let config = resolve_server_config(command)?;
    let listener = TcpListener::bind(&bind_addr)
        .map_err(|error| runtime_error(5, &format!("failed to bind {bind_addr}: {error}")))?;
    let listen_addr = listener
        .local_addr()
        .map_err(|error| runtime_error(5, &format!("failed to read listener address: {error}")))?;
    let mut stdout = io::stdout().lock();

    writeln!(stdout, "truss listening on http://{listen_addr}")
        .map_err(|error| runtime_error(5, &format!("failed to write stdout: {error}")))?;
    writeln!(stdout, "storage root: {}", config.storage_root.display())
        .map_err(|error| runtime_error(5, &format!("failed to write stdout: {error}")))?;
    if let Some(public_base_url) = &config.public_base_url {
        writeln!(stdout, "public base URL: {public_base_url}")
            .map_err(|error| runtime_error(5, &format!("failed to write stdout: {error}")))?;
    }
    if let Some(signed_url_key_id) = &config.signed_url_key_id {
        writeln!(stdout, "signed URL key ID: {signed_url_key_id}")
            .map_err(|error| runtime_error(5, &format!("failed to write stdout: {error}")))?;
    }
    if config.allow_insecure_url_sources {
        writeln!(stdout, "insecure URL sources: enabled")
            .map_err(|error| runtime_error(5, &format!("failed to write stdout: {error}")))?;
    }
    stdout
        .flush()
        .map_err(|error| runtime_error(5, &format!("failed to flush stdout: {error}")))?;

    server::serve_with_config(listener, &config)
        .map_err(|error| runtime_error(5, &format!("server runtime failed: {error}")))
}

fn resolve_server_config(command: ServeCommand) -> Result<ServerConfig, CliError> {
    let mut config = ServerConfig::from_env().map_err(|error| {
        runtime_error(5, &format!("failed to load server configuration: {error}"))
    })?;

    if let Some(storage_root) = command.storage_root {
        config.storage_root = storage_root.canonicalize().map_err(|error| {
            runtime_error(
                5,
                &format!(
                    "failed to resolve storage root {}: {error}",
                    storage_root.display()
                ),
            )
        })?;
    }

    if let Some(public_base_url) = command.public_base_url {
        config.public_base_url = Some(public_base_url);
    }

    match (command.signed_url_key_id, command.signed_url_secret) {
        (Some(key_id), Some(secret)) => {
            config = config.with_signed_url_credentials(key_id, secret);
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(usage_error(
                "`--signed-url-key-id` and `--signed-url-secret` must be provided together",
            ));
        }
        (None, None) => {}
    }

    if command.allow_insecure_url_sources {
        config.allow_insecure_url_sources = true;
    }

    Ok(config)
}

fn execute_inspect<R, W>(
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
        .map_err(|error| runtime_error(4, &error.to_string()))?;
    let json = render_inspection_json(&artifact);

    stdout
        .write_all(json.as_bytes())
        .map_err(|error| runtime_error(5, &format!("failed to write output: {error}")))?;

    Ok(())
}

fn execute_convert<R, W>(
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
        .map_err(|error| runtime_error(4, &error.to_string()))?;

    let mut options = command.options;
    if options.format.is_none() {
        options.format = infer_output_format(&command.output).or(Some(input.media_type));
    }

    let output = transform_raster(TransformRequest::new(input, options))
        .map_err(map_transform_error)?;

    write_output_bytes(command.output, &output.bytes, stdout)
}

fn execute_sign<W>(command: SignCommand, stdout: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    let url = sign_public_url(
        &command.base_url,
        command.source,
        &command.options,
        &command.key_id,
        &command.secret,
        command.expires,
    )
    .map_err(|reason| runtime_error(4, &reason))?;

    writeln!(stdout, "{url}")
        .map_err(|error| runtime_error(5, &format!("failed to write output: {error}")))?;

    Ok(())
}

fn map_transform_error(error: crate::TransformError) -> CliError {
    match error {
        crate::TransformError::InvalidInput(reason)
        | crate::TransformError::InvalidOptions(reason) => runtime_error(2, &reason),
        crate::TransformError::UnsupportedInputMediaType(reason)
        | crate::TransformError::DecodeFailed(reason)
        | crate::TransformError::EncodeFailed(reason)
        | crate::TransformError::CapabilityMissing(reason)
        | crate::TransformError::LimitExceeded(reason) => runtime_error(4, &reason),
        crate::TransformError::UnsupportedOutputMediaType(media_type) => {
            runtime_error(4, &format!("unsupported output media type: {media_type}"))
        }
    }
}

fn read_input_bytes<R>(input: InputSource, stdin: &mut R) -> Result<Vec<u8>, CliError>
where
    R: Read,
{
    match input {
        InputSource::Stdin => {
            let mut bytes = Vec::new();
            stdin
                .read_to_end(&mut bytes)
                .map_err(|error| runtime_error(3, &format!("failed to read stdin: {error}")))?;
            Ok(bytes)
        }
        InputSource::Path(path) => fs::read(&path).map_err(|error| {
            runtime_error(3, &format!("failed to read {}: {error}", path.display()))
        }),
        InputSource::Url(url) => read_url_bytes(&url),
    }
}

fn read_url_bytes(url: &str) -> Result<Vec<u8>, CliError> {
    let response = ureq::get(url)
        .call()
        .map_err(|error| map_http_fetch_error(url, error))?;

    if let Some(content_length) = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
    {
        if content_length > MAX_REMOTE_BYTES {
            return Err(runtime_error(
                3,
                &format!("failed to fetch {url}: response exceeds {MAX_REMOTE_BYTES} bytes"),
            ));
        }
    }

    let mut reader = response.into_reader().take(MAX_REMOTE_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| runtime_error(3, &format!("failed to fetch {url}: {error}")))?;

    if bytes.len() as u64 > MAX_REMOTE_BYTES {
        return Err(runtime_error(
            3,
            &format!("failed to fetch {url}: response exceeds {MAX_REMOTE_BYTES} bytes"),
        ));
    }

    Ok(bytes)
}

fn map_http_fetch_error(url: &str, error: ureq::Error) -> CliError {
    match error {
        ureq::Error::Status(status, _) => {
            runtime_error(3, &format!("failed to fetch {url}: HTTP {status}"))
        }
        ureq::Error::Transport(error) => {
            runtime_error(3, &format!("failed to fetch {url}: {error}"))
        }
    }
}

fn write_output_bytes<W>(output: OutputTarget, bytes: &[u8], stdout: &mut W) -> Result<(), CliError>
where
    W: Write,
{
    match output {
        OutputTarget::Stdout => stdout
            .write_all(bytes)
            .map_err(|error| runtime_error(5, &format!("failed to write stdout: {error}"))),
        OutputTarget::Path(path) => fs::write(&path, bytes).map_err(|error| {
            runtime_error(5, &format!("failed to write {}: {error}", path.display()))
        }),
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
    MediaType::from_str(&extension).ok()
}

fn parse_input_source(value: &str, command: &str) -> Result<InputSource, CliError> {
    if value == "-" {
        return Ok(InputSource::Stdin);
    }

    if value.starts_with('-') {
        return Err(usage_error(&format!(
            "unknown argument for `{command}`: `{value}`"
        )));
    }

    Ok(InputSource::Path(PathBuf::from(value)))
}

fn parse_url_arg(value: &str, flag: &str) -> Result<String, CliError> {
    if value.starts_with("http://") || value.starts_with("https://") {
        return Ok(value.to_string());
    }

    Err(usage_error(&format!(
        "`{flag}` requires an http:// or https:// URL"
    )))
}

fn parse_output_target(value: &str) -> Result<OutputTarget, CliError> {
    if value == "-" {
        return Ok(OutputTarget::Stdout);
    }

    if value.starts_with('-') {
        return Err(usage_error(&format!("unknown output target `{value}`")));
    }

    Ok(OutputTarget::Path(PathBuf::from(value)))
}

fn parse_u32_arg(value: Option<&String>, flag: &str) -> Result<u32, CliError> {
    let value = required_arg(value, flag)?;
    value
        .parse::<u32>()
        .map_err(|_| usage_error(&format!("`{flag}` requires an integer")))
}

fn parse_u8_arg(value: Option<&String>, flag: &str) -> Result<u8, CliError> {
    let value = required_arg(value, flag)?;
    value
        .parse::<u8>()
        .map_err(|_| usage_error(&format!("`{flag}` requires an integer")))
}

fn parse_u64_arg(value: Option<&String>, flag: &str) -> Result<u64, CliError> {
    let value = required_arg(value, flag)?;
    value
        .parse::<u64>()
        .map_err(|_| usage_error(&format!("`{flag}` requires an integer")))
}

fn required_arg<'a>(value: Option<&'a String>, flag: &str) -> Result<&'a str, CliError> {
    value
        .map(String::as_str)
        .ok_or_else(|| usage_error(&format!("`{flag}` requires a value")))
}

fn parse_named<T, F>(value: &str, flag: &str, parser: F) -> Result<T, CliError>
where
    F: FnOnce(&str) -> Result<T, String>,
{
    parser(value).map_err(|reason| usage_error(&format!("{flag}: {reason}")))
}

fn render_inspection_json(artifact: &crate::Artifact) -> String {
    format!(
        concat!(
            "{{\n",
            "  \"format\": \"{}\",\n",
            "  \"mime\": \"{}\",\n",
            "  \"width\": {},\n",
            "  \"height\": {},\n",
            "  \"hasAlpha\": {},\n",
            "  \"isAnimated\": {}\n",
            "}}\n"
        ),
        artifact.media_type.as_name(),
        artifact.media_type.as_mime(),
        render_optional_u32(artifact.metadata.width),
        render_optional_u32(artifact.metadata.height),
        render_optional_bool(artifact.metadata.has_alpha),
        render_bool(artifact.metadata.frame_count > 1 || artifact.metadata.duration.is_some()),
    )
}

fn render_optional_u32(value: Option<u32>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

fn render_optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

fn render_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn usage_error(message: &str) -> CliError {
    CliError {
        exit_code: 2,
        message: message.to_string(),
    }
}

fn runtime_error(exit_code: u8, message: &str) -> CliError {
    CliError {
        exit_code,
        message: message.to_string(),
    }
}

fn write_error<E>(stderr: &mut E, error: CliError) -> u8
where
    E: Write,
{
    let _ = writeln!(stderr, "error: {}", error.message);
    error.exit_code
}

#[cfg(test)]
mod tests {
    use super::{
        parse_args, resolve_server_config, run_with_io, Command, ConvertCommand, InputSource,
        OutputTarget, ServeCommand, SignCommand,
    };
    use crate::{sniff_artifact, Fit, MediaType, RawArtifact, SignedUrlSource, TransformOptions};
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    use std::env;
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn png_bytes() -> Vec<u8> {
        let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 4, 3, ColorType::Rgba8.into())
            .expect("encode png");
        bytes
    }

    fn temp_file_path(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        env::temp_dir().join(format!("truss-{name}-{unique}.bin"))
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let path = env::temp_dir().join(format!("truss-{name}-{unique}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn spawn_http_server(
        body: Vec<u8>,
        content_type: &'static str,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind http test server");
        let addr = listener.local_addr().expect("server addr");
        let url = format!("http://{addr}/image");

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).expect("write headers");
            stream.write_all(&body).expect("write body");
            stream.flush().expect("flush response");
        });

        (url, handle)
    }

    #[test]
    fn parse_args_rejects_empty_invocation() {
        let error =
            parse_args(vec!["truss".to_string()]).expect_err("empty invocation should fail");

        assert_eq!(error.exit_code, 2);
        assert_eq!(error.message, "`convert` requires an input path or `-`");
    }

    #[test]
    fn parse_args_supports_serve_bind() {
        let command = parse_args(vec![
            "truss".to_string(),
            "serve".to_string(),
            "--bind".to_string(),
            "127.0.0.1:9000".to_string(),
        ])
        .expect("parse serve bind");

        assert_eq!(
            command,
            Command::Serve(ServeCommand {
                bind_addr: Some("127.0.0.1:9000".to_string()),
                storage_root: None,
                public_base_url: None,
                signed_url_key_id: None,
                signed_url_secret: None,
                allow_insecure_url_sources: false,
            })
        );
    }

    #[test]
    fn parse_args_supports_server_flags_without_subcommand() {
        let command = parse_args(vec![
            "truss".to_string(),
            "--storage-root".to_string(),
            "fixtures".to_string(),
            "--public-base-url".to_string(),
            "https://assets.example.com".to_string(),
            "--allow-insecure-url-sources".to_string(),
        ])
        .expect("parse implicit serve");

        assert_eq!(
            command,
            Command::Serve(ServeCommand {
                bind_addr: None,
                storage_root: Some(PathBuf::from("fixtures")),
                public_base_url: Some("https://assets.example.com".to_string()),
                signed_url_key_id: None,
                signed_url_secret: None,
                allow_insecure_url_sources: true,
            })
        );
    }

    #[test]
    fn parse_args_supports_serve_runtime_options() {
        let command = parse_args(vec![
            "truss".to_string(),
            "serve".to_string(),
            "--storage-root".to_string(),
            "fixtures".to_string(),
            "--public-base-url".to_string(),
            "https://assets.example.com".to_string(),
            "--signed-url-key-id".to_string(),
            "public-dev".to_string(),
            "--signed-url-secret".to_string(),
            "secret-value".to_string(),
            "--allow-insecure-url-sources".to_string(),
        ])
        .expect("parse serve runtime options");

        assert_eq!(
            command,
            Command::Serve(ServeCommand {
                bind_addr: None,
                storage_root: Some(PathBuf::from("fixtures")),
                public_base_url: Some("https://assets.example.com".to_string()),
                signed_url_key_id: Some("public-dev".to_string()),
                signed_url_secret: Some("secret-value".to_string()),
                allow_insecure_url_sources: true,
            })
        );
    }

    #[test]
    fn parse_args_rejects_partial_signed_url_credentials() {
        let error = parse_args(vec![
            "truss".to_string(),
            "serve".to_string(),
            "--signed-url-key-id".to_string(),
            "public-dev".to_string(),
        ])
        .expect("parse serve args first");

        let error = match error {
            Command::Serve(command) => {
                resolve_server_config(command).expect_err("partial credentials should fail")
            }
            _ => panic!("expected serve command"),
        };

        assert_eq!(error.exit_code, 2);
        assert_eq!(
            error.message,
            "`--signed-url-key-id` and `--signed-url-secret` must be provided together"
        );
    }

    #[test]
    fn parse_args_rejects_invalid_public_base_url() {
        let error = parse_args(vec![
            "truss".to_string(),
            "serve".to_string(),
            "--public-base-url".to_string(),
            "ftp://assets.example.com".to_string(),
        ])
        .expect_err("invalid public base URL should fail");

        assert_eq!(error.exit_code, 2);
        assert_eq!(
            error.message,
            "`--public-base-url` requires an http:// or https:// URL"
        );
    }

    #[test]
    fn resolve_server_config_applies_serve_overrides() {
        let storage_root = temp_dir("serve-config");
        let expected_storage_root = storage_root.canonicalize().expect("canonicalize temp dir");
        let config = resolve_server_config(ServeCommand {
            bind_addr: Some("127.0.0.1:0".to_string()),
            storage_root: Some(storage_root.clone()),
            public_base_url: Some("https://assets.example.com".to_string()),
            signed_url_key_id: Some("public-dev".to_string()),
            signed_url_secret: Some("secret-value".to_string()),
            allow_insecure_url_sources: true,
        })
        .expect("resolve server config");

        let _ = fs::remove_dir_all(storage_root);

        assert_eq!(config.storage_root, expected_storage_root);
        assert_eq!(
            config.public_base_url.as_deref(),
            Some("https://assets.example.com")
        );
        assert_eq!(config.signed_url_key_id.as_deref(), Some("public-dev"));
        assert_eq!(config.signed_url_secret.as_deref(), Some("secret-value"));
        assert!(config.allow_insecure_url_sources);
    }

    #[test]
    fn parse_args_supports_implicit_convert_path_and_output() {
        let command = parse_args(vec![
            "truss".to_string(),
            "input.png".to_string(),
            "-o".to_string(),
            "output.jpg".to_string(),
            "--width".to_string(),
            "100".to_string(),
            "--fit".to_string(),
            "contain".to_string(),
        ])
        .expect("parse implicit convert");

        assert_eq!(
            command,
            Command::Convert(ConvertCommand {
                input: InputSource::Path(PathBuf::from("input.png")),
                output: OutputTarget::Path(PathBuf::from("output.jpg")),
                options: TransformOptions {
                    width: Some(100),
                    fit: Some(Fit::Contain),
                    ..TransformOptions::default()
                }
            })
        );
    }

    #[test]
    fn parse_args_supports_inspect_path() {
        let command = parse_args(vec![
            "truss".to_string(),
            "inspect".to_string(),
            "input.png".to_string(),
        ])
        .expect("parse inspect path");

        assert_eq!(
            command,
            Command::Inspect(super::InspectCommand {
                input: InputSource::Path(PathBuf::from("input.png"))
            })
        );
    }

    #[test]
    fn parse_args_supports_inspect_url() {
        let command = parse_args(vec![
            "truss".to_string(),
            "inspect".to_string(),
            "--url".to_string(),
            "http://example.com/image.png".to_string(),
        ])
        .expect("parse inspect url");

        assert_eq!(
            command,
            Command::Inspect(super::InspectCommand {
                input: InputSource::Url("http://example.com/image.png".to_string())
            })
        );
    }

    #[test]
    fn parse_args_supports_convert_path_and_output() {
        let command = parse_args(vec![
            "truss".to_string(),
            "convert".to_string(),
            "input.png".to_string(),
            "-o".to_string(),
            "output.jpg".to_string(),
            "--width".to_string(),
            "100".to_string(),
            "--fit".to_string(),
            "contain".to_string(),
        ])
        .expect("parse convert");

        assert_eq!(
            command,
            Command::Convert(ConvertCommand {
                input: InputSource::Path(PathBuf::from("input.png")),
                output: OutputTarget::Path(PathBuf::from("output.jpg")),
                options: TransformOptions {
                    width: Some(100),
                    fit: Some(Fit::Contain),
                    ..TransformOptions::default()
                }
            })
        );
    }

    #[test]
    fn parse_args_supports_convert_url_and_output() {
        let command = parse_args(vec![
            "truss".to_string(),
            "convert".to_string(),
            "--url".to_string(),
            "http://example.com/image.png".to_string(),
            "-o".to_string(),
            "output.jpg".to_string(),
        ])
        .expect("parse convert url");

        assert_eq!(
            command,
            Command::Convert(ConvertCommand {
                input: InputSource::Url("http://example.com/image.png".to_string()),
                output: OutputTarget::Path(PathBuf::from("output.jpg")),
                options: TransformOptions::default(),
            })
        );
    }

    #[test]
    fn parse_args_supports_sign_for_path_sources() {
        let command = parse_args(vec![
            "truss".to_string(),
            "sign".to_string(),
            "--base-url".to_string(),
            "https://cdn.example.com".to_string(),
            "--path".to_string(),
            "/image.png".to_string(),
            "--key-id".to_string(),
            "public-dev".to_string(),
            "--secret".to_string(),
            "secret-value".to_string(),
            "--expires".to_string(),
            "4102444800".to_string(),
            "--format".to_string(),
            "jpeg".to_string(),
        ])
        .expect("parse sign path");

        assert_eq!(
            command,
            Command::Sign(SignCommand {
                base_url: "https://cdn.example.com".to_string(),
                source: SignedUrlSource::Path {
                    path: "/image.png".to_string(),
                    version: None
                },
                key_id: "public-dev".to_string(),
                secret: "secret-value".to_string(),
                expires: 4_102_444_800,
                options: TransformOptions {
                    format: Some(MediaType::Jpeg),
                    ..TransformOptions::default()
                }
            })
        );
    }

    #[test]
    fn parse_args_supports_sign_for_url_sources() {
        let command = parse_args(vec![
            "truss".to_string(),
            "sign".to_string(),
            "--base-url".to_string(),
            "https://cdn.example.com".to_string(),
            "--url".to_string(),
            "https://origin.example.com/image.png".to_string(),
            "--version".to_string(),
            "v2".to_string(),
            "--key-id".to_string(),
            "public-dev".to_string(),
            "--secret".to_string(),
            "secret-value".to_string(),
            "--expires".to_string(),
            "4102444800".to_string(),
            "--width".to_string(),
            "120".to_string(),
        ])
        .expect("parse sign url");

        assert_eq!(
            command,
            Command::Sign(SignCommand {
                base_url: "https://cdn.example.com".to_string(),
                source: SignedUrlSource::Url {
                    url: "https://origin.example.com/image.png".to_string(),
                    version: Some("v2".to_string())
                },
                key_id: "public-dev".to_string(),
                secret: "secret-value".to_string(),
                expires: 4_102_444_800,
                options: TransformOptions {
                    width: Some(120),
                    ..TransformOptions::default()
                }
            })
        );
    }

    #[test]
    fn parse_args_rejects_missing_convert_output() {
        let error = parse_args(vec![
            "truss".to_string(),
            "convert".to_string(),
            "input.png".to_string(),
        ])
        .expect_err("missing output should fail");

        assert_eq!(error.exit_code, 2);
        assert_eq!(error.message, "`convert` requires `--output`");
    }

    #[test]
    fn parse_args_rejects_unimplemented_inspect_url() {
        let error = parse_args(vec![
            "truss".to_string(),
            "inspect".to_string(),
            "--url".to_string(),
            "https://example.com/image.png".to_string(),
        ])
        .expect("inspect https url should parse");

        assert_eq!(
            error,
            Command::Inspect(super::InspectCommand {
                input: InputSource::Url("https://example.com/image.png".to_string())
            })
        );
    }

    #[test]
    fn parse_args_rejects_invalid_convert_url_scheme() {
        let error = parse_args(vec![
            "truss".to_string(),
            "convert".to_string(),
            "--url".to_string(),
            "ftp://example.com/image.png".to_string(),
            "-o".to_string(),
            "out.png".to_string(),
        ])
        .expect_err("convert invalid scheme should fail");

        assert_eq!(error.exit_code, 2);
        assert_eq!(error.message, "`--url` requires an http:// or https:// URL");
    }

    #[test]
    fn run_with_io_prints_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "--help".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("truss convert <INPUT> -o <OUTPUT>"));
        assert!(output.contains("truss sign --base-url <URL>"));
        assert!(output.contains("truss <INPUT> -o <OUTPUT> [OPTIONS]"));
        assert!(output.contains("--storage-root <PATH>"));
        assert!(output.contains("--public-base-url <URL>"));
        assert!(output.contains("--signed-url-key-id <KEY_ID>"));
        assert!(output.contains("--signed-url-secret <SECRET>"));
        assert!(output.contains("--allow-insecure-url-sources"));
    }

    #[test]
    fn run_with_io_converts_without_explicit_subcommand() {
        let input_path = temp_file_path("implicit-convert-input");
        let output_path = temp_file_path("implicit-convert-output").with_extension("jpg");
        fs::write(&input_path, png_bytes()).expect("write input file");

        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                input_path.display().to_string(),
                "-o".to_string(),
                output_path.display().to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let output_bytes = fs::read(&output_path).expect("read output file");
        let artifact = sniff_artifact(RawArtifact::new(output_bytes, None)).expect("sniff output");

        let _ = fs::remove_file(&input_path);
        let _ = fs::remove_file(&output_path);

        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn run_with_io_inspects_a_file() {
        let path = temp_file_path("inspect");
        fs::write(&path, png_bytes()).expect("write temp file");

        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "inspect".to_string(),
                path.display().to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let _ = fs::remove_file(&path);

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());

        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("\"format\": \"png\""));
        assert!(output.contains("\"mime\": \"image/png\""));
        assert!(output.contains("\"width\": 4"));
        assert!(output.contains("\"height\": 3"));
        assert!(output.contains("\"hasAlpha\": true"));
        assert!(output.contains("\"isAnimated\": false"));
    }

    #[test]
    fn run_with_io_inspects_a_url() {
        let (url, handle) = spawn_http_server(png_bytes(), "image/png");
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "inspect".to_string(),
                "--url".to_string(),
                url,
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        handle.join().expect("join server thread");

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        assert!(String::from_utf8(stdout)
            .expect("utf8 stdout")
            .contains("\"format\": \"png\""));
    }

    #[test]
    fn run_with_io_converts_a_file_and_infers_output_format_from_extension() {
        let input_path = temp_file_path("convert-input");
        let output_path = temp_file_path("convert-output").with_extension("jpg");
        fs::write(&input_path, png_bytes()).expect("write input file");

        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "convert".to_string(),
                input_path.display().to_string(),
                "-o".to_string(),
                output_path.display().to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let output_bytes = fs::read(&output_path).expect("read output file");
        let artifact = sniff_artifact(RawArtifact::new(output_bytes, None)).expect("sniff output");

        let _ = fs::remove_file(&input_path);
        let _ = fs::remove_file(&output_path);

        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn run_with_io_converts_stdin_to_stdout() {
        let mut stdin = Cursor::new(png_bytes());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "convert".to_string(),
                "-".to_string(),
                "-o".to_string(),
                "-".to_string(),
                "--format".to_string(),
                "png".to_string(),
                "--width".to_string(),
                "8".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(
            exit_code,
            0,
            "stderr was: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(stderr.is_empty());

        let artifact = sniff_artifact(RawArtifact::new(stdout, None)).expect("sniff stdout output");

        assert_eq!(artifact.media_type, MediaType::Png);
        assert_eq!(artifact.metadata.width, Some(8));
    }

    #[test]
    fn run_with_io_converts_a_url_to_a_file() {
        let (url, handle) = spawn_http_server(png_bytes(), "image/png");
        let output_path = temp_file_path("convert-url-output").with_extension("png");
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "convert".to_string(),
                "--url".to_string(),
                url,
                "-o".to_string(),
                output_path.display().to_string(),
                "--width".to_string(),
                "8".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        handle.join().expect("join server thread");

        let output_bytes = fs::read(&output_path).expect("read output file");
        let artifact = sniff_artifact(RawArtifact::new(output_bytes, None)).expect("sniff output");
        let _ = fs::remove_file(&output_path);

        assert_eq!(exit_code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(artifact.media_type, MediaType::Png);
        assert_eq!(artifact.metadata.width, Some(8));
    }

    #[test]
    fn run_with_io_reports_input_errors() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "inspect".to_string(),
                "missing-file.png".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 3);
        assert!(stdout.is_empty());
        assert!(String::from_utf8(stderr)
            .expect("utf8 stderr")
            .contains("failed to read missing-file.png"));
    }

    #[test]
    fn run_with_io_reports_decode_errors() {
        let mut stdin = Cursor::new(vec![1, 2, 3, 4]);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "inspect".to_string(), "-".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 4);
        assert!(stdout.is_empty());
        assert!(String::from_utf8(stderr)
            .expect("utf8 stderr")
            .contains("unknown file signature"));
    }
}
