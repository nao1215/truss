use crate::adapters::server::{self, ServerConfig, SignedUrlSource, sign_public_url};
use crate::{
    Fit, MediaType, Position, RawArtifact, Rgba8, Rotation, TransformOptions, TransformRequest,
    WatermarkInput, sniff_artifact, transform_raster, transform_svg,
};
use clap::{CommandFactory, Parser, Subcommand};
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

const MAX_REMOTE_BYTES: u64 = 32 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Exit codes — kept in sync with help text
// ---------------------------------------------------------------------------

/// Successful completion.
const EXIT_SUCCESS: u8 = 0;
/// Usage error (bad arguments, missing required flags).
const EXIT_USAGE: u8 = 1;
/// I/O error (file not found, permission denied, network failure).
const EXIT_IO: u8 = 2;
/// Input error (unsupported format, corrupt file).
const EXIT_INPUT: u8 = 3;
/// Transform error (encode failure, size limit exceeded, deadline).
const EXIT_TRANSFORM: u8 = 4;
/// Runtime error (bind failure, stdout write failure).
const EXIT_RUNTIME: u8 = 5;

// ---------------------------------------------------------------------------
// Help text — split by topic (hand-crafted for rich output)
// ---------------------------------------------------------------------------

fn help_top_level() -> String {
    format!(
        "\
truss {version} - an image transformation tool and server

Converts, resizes, and re-encodes images (JPEG, PNG, WebP, AVIF, BMP, SVG).
Can also run as an HTTP image-transform server.

USAGE:
  truss <COMMAND> [OPTIONS]
  truss <INPUT> -o <OUTPUT> [OPTIONS]   (implicit convert)
  truss --bind <ADDR> [OPTIONS]         (implicit serve)

COMMANDS:
  convert       Convert and transform an image file
  inspect       Show metadata (format, dimensions, alpha) of an image
  serve         Start the HTTP image-transform server
  sign          Generate a signed public URL for the server
  completions   Generate shell completion scripts
  help          Show help for a command (e.g. truss help convert)

OPTIONS:
  -V, --version   Print version information

EXAMPLES:
  truss photo.png -o photo.jpg --width 800
  truss inspect photo.jpg
  truss serve --bind 0.0.0.0:8080 --storage-root /var/images
  truss sign --base-url https://cdn.example.com --path /hero.jpg \\
    --key-id mykey --secret s3cret --expires 1700000000
  truss completions bash > ~/.local/share/bash-completion/completions/truss

Run 'truss help <command>' for more information on a specific command.

EXIT CODES:
  0  Success
  1  Usage error (bad arguments)
  2  I/O error (file not found, permission denied, network failure)
  3  Input error (unsupported format, corrupt file)
  4  Transform error (encode failure, size limit exceeded, deadline)
  5  Runtime error (bind failure, stdout write failure)

Sponsor: https://github.com/sponsors/nao1215
",
        version = env!("CARGO_PKG_VERSION"),
    )
}

const HELP_CONVERT: &str = "\
truss convert - convert and transform an image file

USAGE:
  truss convert <INPUT> -o <OUTPUT> [OPTIONS]
  truss convert --url <URL> -o <OUTPUT> [OPTIONS]
  truss convert - -o - --format jpeg    (stdin to stdout)

  The 'convert' subcommand can be omitted:
    truss <INPUT> -o <OUTPUT> [OPTIONS]
    truss --url <URL> -o <OUTPUT> [OPTIONS]

  Use -- to separate options from file paths starting with -:
    truss convert -- -input.png -o out.jpg
    truss convert input.png -o -- -output.jpg

OPTIONS:
  -o, --output <OUTPUT>    Output file path, or - for stdout (required)
      --url <URL>          Fetch input from an HTTP(S) URL
      --width <PX>         Target width in pixels
      --height <PX>        Target height in pixels
      --fit <MODE>         How to fit into target dimensions
                           contain: scale down to fit entirely (default)
                           cover:   scale to fill, cropping excess
                           fill:    stretch to exact dimensions
                           inside:  like contain, but never upscale
      --position <POS>     Crop anchor for cover mode (default: center)
                           center, top, right, bottom, left,
                           top-left, top-right, bottom-left, bottom-right
      --format <FMT>       Output format: jpeg, png, webp, avif, bmp, svg
                           (default: inferred from output extension)
      --quality <1-100>    Encoding quality for lossy formats
      --background <COLOR> Background color as RRGGBB or RRGGBBAA hex
      --rotate <DEG>       Rotate: 0, 90, 180, 270
      --auto-orient        Apply EXIF orientation and reset tag (default)
      --no-auto-orient     Skip EXIF orientation correction
      --strip-metadata     Remove all metadata (default)
      --keep-metadata      Preserve EXIF, ICC, and other supported metadata
      --preserve-exif      Preserve EXIF only (strip ICC and others)

EXAMPLES:
  truss photo.png -o photo.jpg --width 800
  truss --url https://example.com/img.png -o out.webp --format webp --quality 75
  cat photo.png | truss convert - -o - --format jpeg > photo.jpg
  truss photo.png -o thumb.png --width 200 --height 200 --fit cover
  truss diagram.svg -o safe.svg
  truss diagram.svg -o diagram.png --width 1024
";

const HELP_INSPECT: &str = "\
truss inspect - show metadata of an image

USAGE:
  truss inspect <FILE>
  truss inspect --url <URL>
  truss inspect -               (read from stdin)

  Use -- to separate options from file paths starting with -:
    truss inspect -- -weird-name.png

OUTPUT:
  Prints JSON with format, MIME type, dimensions, alpha, and animation info.

EXAMPLES:
  truss inspect photo.jpg
  truss inspect --url https://example.com/photo.jpg
  cat photo.png | truss inspect -
";

const HELP_SERVE: &str = "\
truss serve - start the HTTP image-transform server

USAGE:
  truss serve [OPTIONS]

  Server flags can also be used at the top level:
    truss --bind 0.0.0.0:8080 --storage-root /var/images

OPTIONS:
      --bind <ADDR>                   Listen address (default: 127.0.0.1:8080)
      --storage-root <PATH>           Root directory for path-based sources
      --public-base-url <URL>         External base URL for signed URLs
      --signed-url-key-id <KEY_ID>    Key identifier for signed public URLs
      --signed-url-secret <SECRET>    Shared secret for HMAC verification
      --allow-insecure-url-sources    Allow private-network URLs (dev/test only)

ENVIRONMENT VARIABLES:
  TRUSS_BIND_ADDR                     Listen address override
  TRUSS_STORAGE_ROOT                  Storage root override
  TRUSS_PUBLIC_BASE_URL               Public base URL override
  TRUSS_BEARER_TOKEN                  Private API authentication token
  TRUSS_SIGNED_URL_KEY_ID             Signing key identifier
  TRUSS_SIGNED_URL_SECRET             Signing shared secret
  TRUSS_ALLOW_INSECURE_URL_SOURCES    Enable insecure URL sources
  TRUSS_CACHE_ROOT                    On-disk transform cache directory

EXAMPLES:
  truss serve --bind 0.0.0.0:8080 --storage-root /var/images
  truss serve --bind 127.0.0.1:3000 --signed-url-key-id mykey --signed-url-secret s3cret
";

const HELP_SIGN: &str = "\
truss sign - generate a signed public URL

USAGE:
  truss sign --base-url <URL> --path <PATH> \\
    --key-id <KEY_ID> --secret <SECRET> --expires <UNIX_SECS> [OPTIONS]
  truss sign --base-url <URL> --url <URL> \\
    --key-id <KEY_ID> --secret <SECRET> --expires <UNIX_SECS> [OPTIONS]

REQUIRED:
      --base-url <URL>     CDN base URL for the signed request
      --path <PATH>        Image path on the server (mutually exclusive with --url)
      --url <URL>          Remote image URL to transform (mutually exclusive with --path)
      --key-id <KEY_ID>    Signing key identifier
      --secret <SECRET>    HMAC shared secret
      --expires <UNIX_SECS> Expiration as Unix timestamp

OPTIONAL:
      --version <VALUE>    Cache-busting version tag
      --width, --height, --fit, --position, --format, --quality,
      --background, --rotate, --auto-orient, --no-auto-orient,
      --strip-metadata, --keep-metadata, --preserve-exif

EXAMPLES:
  truss sign --base-url https://cdn.example.com \\
    --path /photos/hero.jpg --key-id mykey --secret s3cret \\
    --expires 1700000000 --width 640 --format webp
";

const HELP_COMPLETIONS: &str = "\
truss completions - generate shell completion scripts

USAGE:
  truss completions <SHELL>

SHELLS:
  bash, zsh, fish, elvish, powershell

EXAMPLES:
  truss completions bash > ~/.local/share/bash-completion/completions/truss
  truss completions zsh > ~/.zfunc/_truss
  truss completions fish > ~/.config/fish/completions/truss.fish
";

const HELP_VERSION: &str = "\
truss version - print version information

USAGE:
  truss version
  truss -V
  truss --version
";

// ---------------------------------------------------------------------------
// Clap derive structs
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(
    name = "truss",
    about = "an image transformation tool and server",
    disable_help_subcommand = true,
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliSubcommand>,
}

#[derive(Subcommand)]
enum CliSubcommand {
    /// Convert and transform an image file
    #[command(disable_help_flag = true)]
    Convert(ClapConvertArgs),
    /// Show metadata (format, dimensions, alpha) of an image
    #[command(disable_help_flag = true)]
    Inspect(ClapInspectArgs),
    /// Start the HTTP image-transform server
    #[command(disable_help_flag = true)]
    Serve(ClapServeArgs),
    /// Generate a signed public URL for the server
    #[command(disable_help_flag = true)]
    Sign(ClapSignArgs),
    /// Show help for a command
    Help { topic: Option<String> },
    /// Print version information
    Version,
    /// Generate shell completion scripts
    #[command(disable_help_flag = true)]
    Completions {
        #[arg(value_enum)]
        shell: Option<clap_complete::Shell>,
        /// Print help
        #[arg(long)]
        help: bool,
    },
}

#[derive(clap::Args)]
struct ClapConvertArgs {
    /// Input file path, or - for stdin
    #[arg(allow_hyphen_values = true)]
    input: Option<String>,
    /// Output file path, or - for stdout
    #[arg(short = 'o', long = "output", allow_hyphen_values = true)]
    output: Option<String>,
    /// Fetch input from an HTTP(S) URL
    #[arg(long)]
    url: Option<String>,
    /// Target width in pixels
    #[arg(long)]
    width: Option<u32>,
    /// Target height in pixels
    #[arg(long)]
    height: Option<u32>,
    /// How to fit into target dimensions (contain, cover, fill, inside)
    #[arg(long, value_parser = parse_fit)]
    fit: Option<Fit>,
    /// Crop anchor for cover mode
    #[arg(long, value_parser = parse_position)]
    position: Option<Position>,
    /// Output format (jpeg, png, webp, avif, bmp, svg)
    #[arg(long, value_parser = parse_media_type)]
    format: Option<MediaType>,
    /// Encoding quality for lossy formats (1-100)
    #[arg(long)]
    quality: Option<u8>,
    /// Background color as RRGGBB or RRGGBBAA hex
    #[arg(long, value_parser = parse_background)]
    background: Option<Rgba8>,
    /// Rotate: 0, 90, 180, 270
    #[arg(long, value_parser = parse_rotation)]
    rotate: Option<Rotation>,
    /// Apply EXIF orientation and reset tag
    #[arg(long)]
    auto_orient: bool,
    /// Skip EXIF orientation correction
    #[arg(long)]
    no_auto_orient: bool,
    /// Remove all metadata
    #[arg(long)]
    strip_metadata: bool,
    /// Preserve EXIF, ICC, and other supported metadata
    #[arg(long)]
    keep_metadata: bool,
    /// Preserve EXIF only (strip ICC and others)
    #[arg(long)]
    preserve_exif: bool,
    /// Apply Gaussian blur (sigma: 0.1-100.0)
    #[arg(long)]
    blur: Option<f32>,
    /// Watermark image file path
    #[arg(long)]
    watermark: Option<PathBuf>,
    /// Watermark position (default: bottom-right)
    #[arg(long, value_parser = parse_position)]
    watermark_position: Option<Position>,
    /// Watermark opacity 1-100 (default: 50)
    #[arg(long)]
    watermark_opacity: Option<u8>,
    /// Watermark margin in pixels (default: 10)
    #[arg(long)]
    watermark_margin: Option<u32>,
    /// Show help for convert
    #[arg(short = 'h', long = "help")]
    help: bool,
}

#[derive(clap::Args)]
struct ClapInspectArgs {
    /// Input file path, or - for stdin
    #[arg(allow_hyphen_values = true)]
    input: Option<String>,
    /// Fetch input from an HTTP(S) URL
    #[arg(long)]
    url: Option<String>,
    /// Show help for inspect
    #[arg(short = 'h', long = "help")]
    help: bool,
}

#[derive(clap::Args)]
struct ClapServeArgs {
    /// Listen address (e.g. 0.0.0.0:8080)
    #[arg(long)]
    bind: Option<String>,
    /// Root directory for path-based sources
    #[arg(long)]
    storage_root: Option<PathBuf>,
    /// External base URL for signed URLs
    #[arg(long, value_parser = parse_url_value)]
    public_base_url: Option<String>,
    /// Key identifier for signed public URLs
    #[arg(long)]
    signed_url_key_id: Option<String>,
    /// Shared secret for HMAC verification
    #[arg(long)]
    signed_url_secret: Option<String>,
    /// Allow private-network URLs (dev/test only)
    #[arg(long)]
    allow_insecure_url_sources: bool,
    /// Show help for serve
    #[arg(short = 'h', long = "help")]
    help: bool,
}

#[derive(clap::Args)]
struct ClapSignArgs {
    /// CDN base URL for the signed request
    #[arg(long, value_parser = parse_url_value)]
    base_url: Option<String>,
    /// Image path on the server
    #[arg(long)]
    path: Option<String>,
    /// Remote image URL to transform
    #[arg(long, value_parser = parse_url_value)]
    url: Option<String>,
    /// Cache-busting version tag
    #[arg(long)]
    version: Option<String>,
    /// Signing key identifier
    #[arg(long)]
    key_id: Option<String>,
    /// HMAC shared secret
    #[arg(long)]
    secret: Option<String>,
    /// Expiration as Unix timestamp
    #[arg(long)]
    expires: Option<u64>,
    /// Target width in pixels
    #[arg(long)]
    width: Option<u32>,
    /// Target height in pixels
    #[arg(long)]
    height: Option<u32>,
    /// How to fit into target dimensions
    #[arg(long, value_parser = parse_fit)]
    fit: Option<Fit>,
    /// Crop anchor for cover mode
    #[arg(long, value_parser = parse_position)]
    position: Option<Position>,
    /// Output format
    #[arg(long, value_parser = parse_media_type)]
    format: Option<MediaType>,
    /// Encoding quality for lossy formats
    #[arg(long)]
    quality: Option<u8>,
    /// Background color as RRGGBB or RRGGBBAA hex
    #[arg(long, value_parser = parse_background)]
    background: Option<Rgba8>,
    /// Rotate: 0, 90, 180, 270
    #[arg(long, value_parser = parse_rotation)]
    rotate: Option<Rotation>,
    /// Apply EXIF orientation
    #[arg(long)]
    auto_orient: bool,
    /// Skip EXIF orientation correction
    #[arg(long)]
    no_auto_orient: bool,
    /// Remove all metadata
    #[arg(long)]
    strip_metadata: bool,
    /// Preserve EXIF, ICC, and other metadata
    #[arg(long)]
    keep_metadata: bool,
    /// Preserve EXIF only
    #[arg(long)]
    preserve_exif: bool,
    /// Apply Gaussian blur (sigma: 0.1-100.0)
    #[arg(long)]
    blur: Option<f32>,
    /// Show help for sign
    #[arg(short = 'h', long = "help")]
    help: bool,
}

// ---------------------------------------------------------------------------
// Clap value parsers for custom types
// ---------------------------------------------------------------------------

fn parse_fit(s: &str) -> Result<Fit, String> {
    Fit::from_str(s)
}

fn parse_position(s: &str) -> Result<Position, String> {
    Position::from_str(s)
}

fn parse_media_type(s: &str) -> Result<MediaType, String> {
    MediaType::from_str(s)
}

fn parse_rotation(s: &str) -> Result<Rotation, String> {
    Rotation::from_str(s)
}

fn parse_background(s: &str) -> Result<Rgba8, String> {
    Rgba8::from_hex(s)
}

fn parse_url_value(s: &str) -> Result<String, String> {
    let parsed = url::Url::parse(s).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(s.to_string()),
        _ => Err(format!("requires an http:// or https:// URL, got '{s}'")),
    }
}

// ---------------------------------------------------------------------------
// Usage strings (reused in errors)
// ---------------------------------------------------------------------------

fn convert_usage() -> &'static str {
    "usage: truss convert <INPUT> -o <OUTPUT> [OPTIONS]"
}

fn inspect_usage() -> &'static str {
    "usage: truss inspect <FILE|--url URL|->"
}

fn serve_usage() -> &'static str {
    "usage: truss serve [--bind ADDR] [--storage-root PATH] [OPTIONS]"
}

fn sign_usage() -> &'static str {
    "usage: truss sign --base-url <URL> (--path <PATH>|--url <URL>) --key-id <ID> --secret <SECRET> --expires <UNIX_SECS>"
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Command types (internal)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelpTopic {
    TopLevel,
    Convert,
    Inspect,
    Serve,
    Sign,
    Completions,
    Version,
}

#[derive(Debug, Clone, PartialEq)]
enum Command {
    Help(HelpTopic),
    Version,
    Serve(ServeCommand),
    Inspect(InspectCommand),
    Convert(ConvertCommand),
    Sign(SignCommand),
    Completions(clap_complete::Shell),
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

#[derive(Debug, Clone, PartialEq)]
struct ConvertCommand {
    input: InputSource,
    output: OutputTarget,
    options: TransformOptions,
    watermark_path: Option<PathBuf>,
    watermark_position: Option<Position>,
    watermark_opacity: Option<u8>,
    watermark_margin: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
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

// ---------------------------------------------------------------------------
// Structured error
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
    exit_code: u8,
    message: String,
    usage: Option<String>,
    hint: Option<String>,
}

// ---------------------------------------------------------------------------
// Core dispatch
// ---------------------------------------------------------------------------

fn run_with_io<I, R, W, E>(args: I, stdin: &mut R, stdout: &mut W, stderr: &mut E) -> u8
where
    I: IntoIterator<Item = String>,
    R: Read,
    W: Write,
    E: Write,
{
    match parse_args(args) {
        Ok(Command::Help(topic)) => {
            let text = match topic {
                HelpTopic::TopLevel => help_top_level(),
                HelpTopic::Convert => HELP_CONVERT.to_string(),
                HelpTopic::Inspect => HELP_INSPECT.to_string(),
                HelpTopic::Serve => HELP_SERVE.to_string(),
                HelpTopic::Sign => HELP_SIGN.to_string(),
                HelpTopic::Completions => HELP_COMPLETIONS.to_string(),
                HelpTopic::Version => HELP_VERSION.to_string(),
            };
            match stdout.write_all(text.as_bytes()) {
                Ok(_) => EXIT_SUCCESS,
                Err(_) => EXIT_RUNTIME,
            }
        }
        Ok(Command::Version) => match writeln!(stdout, "truss {}", env!("CARGO_PKG_VERSION")) {
            Ok(_) => EXIT_SUCCESS,
            Err(_) => EXIT_RUNTIME,
        },
        Ok(Command::Serve(command)) => match execute_serve(command) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Inspect(command)) => match execute_inspect(command, stdin, stdout) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Convert(command)) => match execute_convert(command, stdin, stdout) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Sign(command)) => match execute_sign(command, stdout) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Completions(shell)) => match generate_completions(shell, stdout) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Err(error) => write_error(stderr, error),
    }
}

// ---------------------------------------------------------------------------
// Argument preprocessing for implicit convert / serve
// ---------------------------------------------------------------------------

const KNOWN_SUBCOMMANDS: &[&str] = &[
    "convert",
    "inspect",
    "serve",
    "sign",
    "help",
    "completions",
    "version",
];

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

/// Returns `true` when a token looks like it was meant to be a subcommand name
/// (starts with a letter, no path separators, no file extension).
fn looks_like_unknown_subcommand(value: &str) -> bool {
    if value.starts_with('-') || value.starts_with('/') || value.starts_with('.') {
        return false;
    }
    if value.contains('.') || value.contains('/') || value.contains('\\') {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Pre-processes raw args to handle implicit convert and implicit serve
/// before handing off to clap.
fn preprocess_args(args: Vec<String>) -> Vec<String> {
    if args.len() <= 1 {
        return args;
    }
    let first = &args[1];

    // -h / --help at top level → route to our help subcommand
    if first == "-h" || first == "--help" {
        let mut new = vec![args[0].clone(), "help".to_string()];
        if args.len() > 2 {
            new.extend_from_slice(&args[2..]);
        }
        return new;
    }

    // -V / --version at top level → route to version subcommand
    if first == "-V" || first == "--version" {
        return vec![args[0].clone(), "version".to_string()];
    }

    // If first arg is a serve flag, insert "serve" subcommand
    if is_serve_flag(first) {
        let mut new = vec![args[0].clone(), "serve".to_string()];
        new.extend_from_slice(&args[1..]);
        return new;
    }

    // Known subcommand → pass through
    if KNOWN_SUBCOMMANDS.contains(&first.as_str()) {
        return args;
    }

    // If the first argument refers to an existing file (even without an
    // extension), treat it as an implicit convert rather than an unknown
    // subcommand.  This handles `truss image -o out.jpg` where `image` is a
    // real file.
    if std::path::Path::new(first).is_file() {
        let mut new = vec![args[0].clone(), "convert".to_string()];
        new.extend_from_slice(&args[1..]);
        return new;
    }

    // Looks like an unknown subcommand (alphabetic, no dots/slashes) →
    // let clap handle it for typo suggestions
    if looks_like_unknown_subcommand(first) {
        return args;
    }

    // Otherwise, treat as implicit convert
    let mut new = vec![args[0].clone(), "convert".to_string()];
    new.extend_from_slice(&args[1..]);
    new
}

// ---------------------------------------------------------------------------
// Argument parsing — main entry
// ---------------------------------------------------------------------------

fn parse_args<I>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item = String>,
{
    let raw: Vec<String> = args.into_iter().collect();

    // Bare invocation → top-level help (exit 0)
    if raw.len() <= 1 {
        return Ok(Command::Help(HelpTopic::TopLevel));
    }

    let preprocessed = preprocess_args(raw);
    let cli = Cli::try_parse_from(&preprocessed).map_err(map_clap_error)?;

    match cli.command {
        None => Ok(Command::Help(HelpTopic::TopLevel)),
        Some(CliSubcommand::Help { topic }) => parse_help_topic(topic),
        Some(CliSubcommand::Version) => Ok(Command::Version),
        Some(CliSubcommand::Completions { help: true, .. }) => {
            Ok(Command::Help(HelpTopic::Completions))
        }
        Some(CliSubcommand::Completions {
            shell: Some(shell), ..
        }) => Ok(Command::Completions(shell)),
        Some(CliSubcommand::Completions {
            shell: None,
            help: false,
        }) => Err(CliError {
            exit_code: EXIT_USAGE,
            message: "'completions' requires a shell argument".to_string(),
            usage: None,
            hint: Some("try 'truss completions bash'".to_string()),
        }),
        Some(CliSubcommand::Convert(args)) => convert_from_clap(args),
        Some(CliSubcommand::Inspect(args)) => inspect_from_clap(args),
        Some(CliSubcommand::Serve(args)) => serve_from_clap(args),
        Some(CliSubcommand::Sign(args)) => sign_from_clap(args),
    }
}

/// Maps a clap error into a structured `CliError`.
fn map_clap_error(err: clap::Error) -> CliError {
    let raw = err.to_string();
    // clap renders "error: ..." — strip that prefix since write_error adds its own
    let message = raw
        .strip_prefix("error: ")
        .unwrap_or(&raw)
        .trim()
        .to_string();

    CliError {
        exit_code: EXIT_USAGE,
        message,
        usage: None,
        hint: Some("run 'truss --help' for available commands".to_string()),
    }
}

fn parse_help_topic(topic: Option<String>) -> Result<Command, CliError> {
    match topic.as_deref() {
        None => Ok(Command::Help(HelpTopic::TopLevel)),
        Some("convert") => Ok(Command::Help(HelpTopic::Convert)),
        Some("inspect") => Ok(Command::Help(HelpTopic::Inspect)),
        Some("serve") => Ok(Command::Help(HelpTopic::Serve)),
        Some("sign") => Ok(Command::Help(HelpTopic::Sign)),
        Some("completions") => Ok(Command::Help(HelpTopic::Completions)),
        Some("version") => Ok(Command::Help(HelpTopic::Version)),
        Some(other) => Err(CliError {
            exit_code: EXIT_USAGE,
            message: format!("unknown help topic '{other}'"),
            usage: None,
            hint: Some(
                "available topics: convert, inspect, serve, sign, completions, version".to_string(),
            ),
        }),
    }
}

// ---------------------------------------------------------------------------
// Clap → internal Command conversions
// ---------------------------------------------------------------------------

fn convert_from_clap(args: ClapConvertArgs) -> Result<Command, CliError> {
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
        blur: args.blur,
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

fn inspect_from_clap(args: ClapInspectArgs) -> Result<Command, CliError> {
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

fn serve_from_clap(args: ClapServeArgs) -> Result<Command, CliError> {
    if args.help {
        return Ok(Command::Help(HelpTopic::Serve));
    }

    Ok(Command::Serve(ServeCommand {
        bind_addr: args.bind,
        storage_root: args.storage_root,
        public_base_url: args.public_base_url,
        signed_url_key_id: args.signed_url_key_id,
        signed_url_secret: args.signed_url_secret,
        allow_insecure_url_sources: args.allow_insecure_url_sources,
    }))
}

fn sign_from_clap(args: ClapSignArgs) -> Result<Command, CliError> {
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
        blur: args.blur,
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
    }))
}

/// Collects shared transform fields from clap args into `TransformOptions`.
struct TransformFields {
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    position: Option<Position>,
    format: Option<MediaType>,
    quality: Option<u8>,
    background: Option<Rgba8>,
    rotate: Option<Rotation>,
    auto_orient: bool,
    no_auto_orient: bool,
    strip_metadata: bool,
    keep_metadata: bool,
    preserve_exif: bool,
    blur: Option<f32>,
}

impl TransformFields {
    fn into_options(self) -> Result<TransformOptions, crate::TransformError> {
        let defaults = TransformOptions::default();
        let auto_orient = if self.no_auto_orient {
            false
        } else if self.auto_orient {
            true
        } else {
            defaults.auto_orient
        };
        let (strip_metadata, preserve_exif) = crate::resolve_metadata_flags(
            if self.strip_metadata {
                Some(true)
            } else {
                None
            },
            if self.keep_metadata { Some(true) } else { None },
            if self.preserve_exif { Some(true) } else { None },
        )?;
        Ok(TransformOptions {
            width: self.width,
            height: self.height,
            fit: self.fit,
            position: self.position,
            format: self.format,
            quality: self.quality,
            background: self.background,
            rotate: self.rotate.unwrap_or(defaults.rotate),
            auto_orient,
            strip_metadata,
            preserve_exif,
            blur: self.blur,
            deadline: None,
        })
    }
}

fn validate_url(url: &str, flag: &str) -> Result<(), CliError> {
    let parsed = url::Url::parse(url).map_err(|e| CliError {
        exit_code: EXIT_USAGE,
        message: format!("'{flag}' is not a valid URL: {e}"),
        usage: None,
        hint: Some(format!("got '{url}'")),
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(CliError {
            exit_code: EXIT_USAGE,
            message: format!("'{flag}' requires an http:// or https:// URL"),
            usage: None,
            hint: Some(format!("got '{url}'")),
        }),
    }
}

/// Generates shell completion scripts for the given shell.
fn generate_completions<W: Write>(
    shell: clap_complete::Shell,
    stdout: &mut W,
) -> Result<(), CliError> {
    let mut cmd = Cli::command();

    // Add implicit-convert positional argument and common flags so that shell
    // completions expose the shorthand forms documented in the help text
    // (e.g. `truss photo.png -o out.jpg`, `truss --bind 0.0.0.0:8080`).
    cmd = cmd
        .arg(
            clap::Arg::new("INPUT")
                .help("Input image file (implicit convert)")
                .value_hint(clap::ValueHint::FilePath),
        )
        .arg(
            clap::Arg::new("output")
                .short('o')
                .long("output")
                .help("Output file path (implicit convert)")
                .value_hint(clap::ValueHint::FilePath),
        )
        .arg(
            clap::Arg::new("bind")
                .long("bind")
                .help("Listen address (implicit serve)"),
        )
        .arg(
            clap::Arg::new("storage-root")
                .long("storage-root")
                .help("Root directory for path-based sources (implicit serve)")
                .value_hint(clap::ValueHint::DirPath),
        );

    clap_complete::generate(shell, &mut cmd, "truss", stdout);
    Ok(())
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

fn execute_serve(command: ServeCommand) -> Result<(), CliError> {
    let bind_addr = command.bind_addr.clone().unwrap_or_else(server::bind_addr);
    let config = resolve_server_config(command)?;
    let listener = TcpListener::bind(&bind_addr).map_err(|error| {
        runtime_error(
            EXIT_RUNTIME,
            &format!("failed to bind {bind_addr}: {error}"),
        )
    })?;
    let listen_addr = listener.local_addr().map_err(|error| {
        runtime_error(
            EXIT_RUNTIME,
            &format!("failed to read listener address: {error}"),
        )
    })?;
    let mut stdout = io::stdout().lock();

    // Server startup summary
    writeln!(stdout, "truss listening on http://{listen_addr}").map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
    })?;
    writeln!(stdout, "  storage root: {}", config.storage_root.display()).map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
    })?;

    // Signed URL verification status
    let signed_url_enabled =
        config.signed_url_key_id.is_some() && config.signed_url_secret.is_some();
    writeln!(
        stdout,
        "  signed URL verification: {}",
        if signed_url_enabled {
            "enabled"
        } else {
            "disabled"
        }
    )
    .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")))?;

    // Bearer token status (never show the value)
    writeln!(
        stdout,
        "  private API bearer token: {}",
        if config.bearer_token.is_some() {
            "configured"
        } else {
            "not set"
        }
    )
    .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")))?;

    // Cache status
    writeln!(
        stdout,
        "  cache: {}",
        if config.cache_root.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    )
    .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")))?;

    if let Some(ref public_base_url) = config.public_base_url {
        writeln!(stdout, "  public base URL: {public_base_url}").map_err(|error| {
            runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
        })?;
    }
    if config.allow_insecure_url_sources {
        writeln!(stdout, "  insecure URL sources: enabled").map_err(|error| {
            runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
        })?;
    }
    stdout.flush().map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to flush stdout: {error}"))
    })?;

    server::serve_with_config(listener, &config)
        .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("server runtime failed: {error}")))
}

fn resolve_server_config(command: ServeCommand) -> Result<ServerConfig, CliError> {
    let mut config = ServerConfig::from_env().map_err(|error| {
        runtime_error(
            EXIT_RUNTIME,
            &format!("failed to load server configuration: {error}"),
        )
    })?;

    if let Some(storage_root) = command.storage_root {
        config.storage_root = storage_root.canonicalize().map_err(|error| {
            runtime_error(
                EXIT_RUNTIME,
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
            return Err(CliError {
                exit_code: EXIT_USAGE,
                message: "--signed-url-key-id and --signed-url-secret must be provided together"
                    .to_string(),
                usage: Some(serve_usage().to_string()),
                hint: Some("run 'truss serve --help' for serve options".to_string()),
            });
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
        .map_err(|error| runtime_error(EXIT_INPUT, &error.to_string()))?;
    let json = render_inspection_json(&artifact);

    stdout.write_all(json.as_bytes()).map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write output: {error}"))
    })?;

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
        .map_err(|error| runtime_error(EXIT_INPUT, &error.to_string()))?;

    let mut options = command.options;
    if options.format.is_none() {
        options.format = infer_output_format(&command.output).or(Some(input.media_type));
    }

    let watermark = if let Some(ref wm_path) = command.watermark_path {
        let wm_bytes = fs::read(wm_path).map_err(|error| {
            runtime_error(
                EXIT_INPUT,
                &format!("failed to read watermark file: {error}"),
            )
        })?;
        let wm_artifact = sniff_artifact(RawArtifact::new(wm_bytes, None))
            .map_err(|error| runtime_error(EXIT_INPUT, &error.to_string()))?;
        Some(WatermarkInput {
            image: wm_artifact,
            position: command.watermark_position.unwrap_or(Position::BottomRight),
            opacity: command.watermark_opacity.unwrap_or(50),
            margin: command.watermark_margin.unwrap_or(10),
        })
    } else {
        None
    };

    let result = if input.media_type == MediaType::Svg {
        transform_svg(TransformRequest::new(input, options))
    } else {
        let mut request = TransformRequest::new(input, options);
        request.watermark = watermark;
        transform_raster(request)
    }
    .map_err(map_transform_error)?;

    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }

    write_output_bytes(command.output, &result.artifact.bytes, stdout)
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
    .map_err(|reason| runtime_error(EXIT_TRANSFORM, &reason))?;

    writeln!(stdout, "{url}").map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write output: {error}"))
    })?;

    Ok(())
}

fn map_transform_error(error: crate::TransformError) -> CliError {
    match error {
        crate::TransformError::InvalidOptions(reason) => runtime_error(EXIT_USAGE, &reason),
        crate::TransformError::InvalidInput(reason) => runtime_error(EXIT_INPUT, &reason),
        crate::TransformError::UnsupportedInputMediaType(reason)
        | crate::TransformError::DecodeFailed(reason)
        | crate::TransformError::EncodeFailed(reason)
        | crate::TransformError::CapabilityMissing(reason)
        | crate::TransformError::LimitExceeded(reason) => runtime_error(EXIT_TRANSFORM, &reason),
        crate::TransformError::UnsupportedOutputMediaType(media_type) => runtime_error(
            EXIT_TRANSFORM,
            &format!("unsupported output media type: {media_type}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// I/O helpers
// ---------------------------------------------------------------------------

fn read_input_bytes<R>(input: InputSource, stdin: &mut R) -> Result<Vec<u8>, CliError>
where
    R: Read,
{
    match input {
        InputSource::Stdin => {
            let mut bytes = Vec::new();
            stdin.read_to_end(&mut bytes).map_err(|error| {
                runtime_error(EXIT_IO, &format!("failed to read stdin: {error}"))
            })?;
            Ok(bytes)
        }
        InputSource::Path(path) => fs::read(&path).map_err(|error| {
            runtime_error(
                EXIT_IO,
                &format!("failed to read {}: {error}", path.display()),
            )
        }),
        InputSource::Url(url) => read_url_bytes(&url),
    }
}

/// Timeout for the TCP connect phase of a remote fetch.
const CLI_FETCH_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Timeout for receiving the full response body from a remote source.
const CLI_FETCH_BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn read_url_bytes(url: &str) -> Result<Vec<u8>, CliError> {
    let config = ureq::config::Config::builder()
        .timeout_connect(Some(CLI_FETCH_CONNECT_TIMEOUT))
        .timeout_recv_body(Some(CLI_FETCH_BODY_TIMEOUT))
        .http_status_as_error(false)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let response = agent
        .get(url)
        .call()
        .map_err(|error| runtime_error(EXIT_IO, &format!("failed to fetch {url}: {error}")))?;

    let status = response.status().as_u16();
    if status >= 400 {
        return Err(runtime_error(
            EXIT_IO,
            &format!("failed to fetch {url}: HTTP {status}"),
        ));
    }

    if response
        .headers()
        .get("Content-Length")
        .and_then(|v: &ureq::http::HeaderValue| v.to_str().ok())
        .and_then(|value: &str| value.parse::<u64>().ok())
        .is_some_and(|len| len > MAX_REMOTE_BYTES)
    {
        return Err(runtime_error(
            EXIT_IO,
            &format!("failed to fetch {url}: response exceeds {MAX_REMOTE_BYTES} bytes"),
        ));
    }

    let mut reader = response
        .into_body()
        .into_reader()
        .take(MAX_REMOTE_BYTES + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| runtime_error(EXIT_IO, &format!("failed to fetch {url}: {error}")))?;

    if bytes.len() as u64 > MAX_REMOTE_BYTES {
        return Err(runtime_error(
            EXIT_IO,
            &format!("failed to fetch {url}: response exceeds {MAX_REMOTE_BYTES} bytes"),
        ));
    }

    Ok(bytes)
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
    if value { "true" } else { "false" }
}

// ---------------------------------------------------------------------------
// Error constructors
// ---------------------------------------------------------------------------

fn convert_error(message: &str) -> CliError {
    CliError {
        exit_code: EXIT_USAGE,
        message: message.to_string(),
        usage: Some(convert_usage().to_string()),
        hint: Some("run 'truss convert --help' for convert options".to_string()),
    }
}

fn sign_error(message: &str) -> CliError {
    CliError {
        exit_code: EXIT_USAGE,
        message: message.to_string(),
        usage: Some(sign_usage().to_string()),
        hint: Some("run 'truss sign --help' for sign options".to_string()),
    }
}

fn runtime_error(exit_code: u8, message: &str) -> CliError {
    CliError {
        exit_code,
        message: message.to_string(),
        usage: None,
        hint: None,
    }
}

fn write_error<E>(stderr: &mut E, error: CliError) -> u8
where
    E: Write,
{
    let _ = writeln!(stderr, "error: {}", error.message);
    if let Some(usage) = &error.usage {
        let _ = writeln!(stderr, "{usage}");
    }
    if let Some(hint) = &error.hint {
        let _ = writeln!(stderr, "hint: {hint}");
    }
    error.exit_code
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{
        Command, ConvertCommand, HelpTopic, InputSource, OutputTarget, ServeCommand, SignCommand,
        parse_args, preprocess_args, resolve_server_config, run_with_io,
    };
    use crate::{Fit, MediaType, RawArtifact, SignedUrlSource, TransformOptions, sniff_artifact};
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

    // ===== Mandatory test 1: bare invocation shows top-level help and succeeds =====

    #[test]
    fn bare_invocation_shows_top_level_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("COMMANDS:"));
        assert!(output.contains("convert"));
        assert!(output.contains("inspect"));
        assert!(output.contains("serve"));
        assert!(output.contains("sign"));
    }

    // ===== Mandatory test 2: --help shows top-level help =====

    #[test]
    fn dash_dash_help_shows_top_level_help() {
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
        assert!(output.contains("COMMANDS:"));
        assert!(output.contains("EXIT CODES:"));
    }

    // ===== Mandatory test 3: `truss help` shows top-level help =====

    #[test]
    fn help_command_shows_top_level_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "help".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("COMMANDS:"));
    }

    // ===== Mandatory test 4: `truss help convert` shows convert help =====

    #[test]
    fn help_convert_shows_convert_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "help".to_string(),
                "convert".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("truss convert"));
        assert!(output.contains("--output"));
        assert!(output.contains("--width"));
        assert!(!output.contains("--bind")); // Should NOT contain serve options
    }

    // ===== Mandatory test 5: `truss convert --help` shows convert help =====

    #[test]
    fn convert_dash_help_shows_convert_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "convert".to_string(),
                "--help".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("truss convert"));
        assert!(output.contains("--output"));
    }

    // ===== Mandatory test 6: `truss serve --help` shows serve help =====

    #[test]
    fn serve_dash_help_shows_serve_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "serve".to_string(),
                "--help".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("truss serve"));
        assert!(output.contains("--bind"));
        assert!(output.contains("--storage-root"));
        assert!(output.contains("ENVIRONMENT VARIABLES:"));
        assert!(!output.contains("--width")); // Should NOT contain convert options
    }

    // ===== Mandatory test 7: `truss sign --help` shows sign help =====

    #[test]
    fn sign_dash_help_shows_sign_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "sign".to_string(),
                "--help".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("truss sign"));
        assert!(output.contains("--base-url"));
        assert!(output.contains("--key-id"));
        assert!(output.contains("--expires"));
    }

    // ===== Mandatory test 8: convert missing --output shows usage and hint =====

    #[test]
    fn convert_missing_output_shows_usage_and_hint() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "convert".to_string(),
                "input.png".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        let output = String::from_utf8(stderr).expect("utf8 stderr");
        assert!(output.contains("error:"), "should contain error: {output}");
        assert!(output.contains("usage:"), "should contain usage: {output}");
        assert!(output.contains("hint:"), "should contain hint: {output}");
    }

    // ===== Mandatory test 9: inspect missing input shows usage and hint =====

    #[test]
    fn inspect_missing_input_shows_usage_and_hint() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "inspect".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        let output = String::from_utf8(stderr).expect("utf8 stderr");
        assert!(output.contains("error:"), "should contain error: {output}");
        assert!(output.contains("usage:"), "should contain usage: {output}");
        assert!(output.contains("hint:"), "should contain hint: {output}");
    }

    // ===== Mandatory test 10: sign missing args shows usage and hint =====

    #[test]
    fn sign_missing_args_shows_usage_and_hint() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "sign".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        let output = String::from_utf8(stderr).expect("utf8 stderr");
        assert!(output.contains("error:"), "should contain error: {output}");
        assert!(output.contains("usage:"), "should contain usage: {output}");
        assert!(output.contains("hint:"), "should contain hint: {output}");
    }

    // ===== Mandatory test 11: -- allows -foo.png as input =====

    #[test]
    fn double_dash_allows_leading_dash_input() {
        let result = parse_args(vec![
            "truss".to_string(),
            "convert".to_string(),
            "-o".to_string(),
            "out.jpg".to_string(),
            "--".to_string(),
            "-foo.png".to_string(),
        ]);

        assert_eq!(
            result.unwrap(),
            Command::Convert(ConvertCommand {
                input: InputSource::Path(PathBuf::from("-foo.png")),
                output: OutputTarget::Path(PathBuf::from("out.jpg")),
                options: TransformOptions::default(),
                watermark_path: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
            })
        );
    }

    // ===== Mandatory test 12: implicit convert with leading-dash output =====
    // Note: With clap, `-o -- -out.jpg` is not supported the same way.
    // Instead, use `-o=-out.jpg` or `--output=-out.jpg`.

    // ===== Mandatory test 13: top-level serve flags still work =====

    #[test]
    fn top_level_serve_flags_parse_correctly() {
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

    // ===== Mandatory test 14: implicit convert still works =====

    #[test]
    fn implicit_convert_still_works() {
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
                },
                watermark_path: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
            })
        );
    }

    // ===== Mandatory test 15: exit codes are consistent =====

    #[test]
    fn exit_code_help_is_zero() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with_io(
            vec!["truss".to_string(), "--help".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
    }

    #[test]
    fn exit_code_usage_error_is_one() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with_io(
            vec![
                "truss".to_string(),
                "convert".to_string(),
                "input.png".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 1);
    }

    #[test]
    fn exit_code_io_error() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with_io(
            vec![
                "truss".to_string(),
                "inspect".to_string(),
                "missing-file.png".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 2);
    }

    #[test]
    fn exit_code_input_error() {
        let mut stdin = Cursor::new(vec![1, 2, 3, 4]);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with_io(
            vec!["truss".to_string(), "inspect".to_string(), "-".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 3);
    }

    // ===== Additional test: unknown subcommand =====

    #[test]
    fn unknown_subcommand_exits_with_usage_error() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "converrt".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        let output = String::from_utf8(stderr).expect("utf8 stderr");
        assert!(output.contains("error:"), "should contain error: {output}");
    }

    // ===== Additional test: inspect --help =====

    #[test]
    fn inspect_dash_help_shows_inspect_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "inspect".to_string(),
                "--help".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("truss inspect"));
        assert!(output.contains("--url"));
    }

    // ===== Additional test: help inspect =====

    #[test]
    fn help_inspect_shows_inspect_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "inspect".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Inspect));
    }

    // ===== Additional test: help serve =====

    #[test]
    fn help_serve_shows_serve_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "serve".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Serve));
    }

    // ===== Additional test: help sign =====

    #[test]
    fn help_sign_shows_sign_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "sign".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Sign));
    }

    // ===== Additional test: -h works as --help =====

    #[test]
    fn dash_h_shows_top_level_help() {
        let result = parse_args(vec!["truss".to_string(), "-h".to_string()]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::TopLevel));
    }

    // ===== Existing test updates =====

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

        assert_eq!(error.exit_code, 1);
        assert!(
            error
                .message
                .contains("--signed-url-key-id and --signed-url-secret must be provided together")
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

        assert_eq!(error.exit_code, 1);
        assert!(
            error
                .message
                .contains("requires an http:// or https:// URL"),
            "message: {}",
            error.message,
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
                },
                watermark_path: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
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
                watermark_path: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
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

        assert_eq!(error.exit_code, 1);
        assert!(error.message.contains("requires -o"));
    }

    #[test]
    fn parse_args_supports_inspect_https_url() {
        let command = parse_args(vec![
            "truss".to_string(),
            "inspect".to_string(),
            "--url".to_string(),
            "https://example.com/image.png".to_string(),
        ])
        .expect("inspect https url should parse");

        assert_eq!(
            command,
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

        assert_eq!(error.exit_code, 1);
        assert!(
            error
                .message
                .contains("requires an http:// or https:// URL")
        );
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
        assert!(
            String::from_utf8(stdout)
                .expect("utf8 stdout")
                .contains("\"format\": \"png\"")
        );
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

        assert_eq!(exit_code, 2);
        assert!(stdout.is_empty());
        assert!(
            String::from_utf8(stderr)
                .expect("utf8 stderr")
                .contains("failed to read missing-file.png")
        );
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

        assert_eq!(exit_code, 3);
        assert!(stdout.is_empty());
        assert!(
            String::from_utf8(stderr)
                .expect("utf8 stderr")
                .contains("unknown file signature")
        );
    }

    // ===== Additional tests for -- with convert =====

    #[test]
    fn double_dash_input_with_options_before() {
        // truss convert -o out.jpg --width 100 -- --leading-dash.png
        let result = parse_args(vec![
            "truss".to_string(),
            "convert".to_string(),
            "-o".to_string(),
            "out.jpg".to_string(),
            "--width".to_string(),
            "100".to_string(),
            "--".to_string(),
            "--leading-dash.png".to_string(),
        ]);

        assert_eq!(
            result.unwrap(),
            Command::Convert(ConvertCommand {
                input: InputSource::Path(PathBuf::from("--leading-dash.png")),
                output: OutputTarget::Path(PathBuf::from("out.jpg")),
                options: TransformOptions {
                    width: Some(100),
                    ..TransformOptions::default()
                },
                watermark_path: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
            })
        );
    }

    // ===== Additional test: inspect -- allows leading dash path =====

    #[test]
    fn inspect_double_dash_allows_leading_dash() {
        let result = parse_args(vec![
            "truss".to_string(),
            "inspect".to_string(),
            "--".to_string(),
            "-weird-name.png".to_string(),
        ]);

        assert_eq!(
            result.unwrap(),
            Command::Inspect(super::InspectCommand {
                input: InputSource::Path(PathBuf::from("-weird-name.png"))
            })
        );
    }

    // ===== Completions subcommand =====

    #[test]
    fn completions_bash_produces_output() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "completions".to_string(),
                "bash".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        // Bash completions should contain the program name
        assert!(
            output.contains("truss"),
            "bash completions should mention truss"
        );
    }

    #[test]
    fn completions_zsh_produces_output() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "completions".to_string(),
                "zsh".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(!stdout.is_empty());
    }

    #[test]
    fn completions_fish_produces_output() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "completions".to_string(),
                "fish".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(!stdout.is_empty());
    }

    // ===== Version subcommand =====

    #[test]
    fn dash_dash_version_prints_version() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "--version".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(
            output.starts_with("truss "),
            "version output should start with 'truss ': {output}"
        );
        assert!(
            output.contains(env!("CARGO_PKG_VERSION")),
            "should contain package version: {output}"
        );
    }

    #[test]
    fn dash_v_prints_version() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec!["truss".to_string(), "-V".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn help_includes_version_and_sponsor() {
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
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(
            output.contains(env!("CARGO_PKG_VERSION")),
            "help should include version: {output}"
        );
        assert!(
            output.contains("Sponsor:"),
            "help should include sponsor link: {output}"
        );
        assert!(
            output.contains("github.com/sponsors/nao1215"),
            "help should include GitHub Sponsors URL: {output}"
        );
    }

    // ===== Fix: extensionless file treated as implicit convert =====

    #[test]
    fn preprocess_args_extensionless_file_is_implicit_convert() {
        // Create a temp file without extension
        let dir = temp_dir("extensionless");
        let file_path = dir.join("image");
        fs::write(&file_path, png_bytes()).expect("write extensionless fixture");

        // Use bare filename and set cwd to the temp dir so preprocess_args
        // sees a relative name without path separators.
        let original_dir = std::env::current_dir().expect("get cwd");
        std::env::set_current_dir(&dir).expect("set cwd to temp dir");

        let args = vec![
            "truss".to_string(),
            "image".to_string(),
            "-o".to_string(),
            "out.jpg".to_string(),
        ];
        let result = preprocess_args(args);

        std::env::set_current_dir(&original_dir).expect("restore cwd");

        assert_eq!(
            result[1], "convert",
            "extensionless file should trigger implicit convert"
        );
        assert_eq!(result[2], "image", "bare file name should follow convert");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preprocess_args_nonexistent_extensionless_is_unknown_subcommand() {
        // A name that doesn't exist on disk should pass through (clap handles typo suggestion)
        let args = vec![
            "truss".to_string(),
            "nonexistent_subcommand_xyz".to_string(),
        ];
        let result = preprocess_args(args.clone());
        assert_eq!(
            result, args,
            "non-existent extensionless name should pass through unchanged"
        );
    }

    // ===== Exit code: InvalidOptions maps to EXIT_USAGE (1) =====

    #[test]
    fn exit_code_invalid_options_is_usage_error() {
        // quality=0 triggers InvalidOptions via normalize()
        let png_bytes = {
            let mut img = image::RgbaImage::new(1, 1);
            img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
            let mut buf = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
            image::ImageEncoder::write_image(
                encoder,
                img.as_raw(),
                1,
                1,
                image::ColorType::Rgba8.into(),
            )
            .unwrap();
            buf
        };
        let mut stdin = Cursor::new(png_bytes);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with_io(
            vec![
                "truss".to_string(),
                "convert".to_string(),
                "-".to_string(),
                "-o".to_string(),
                "-".to_string(),
                "--format".to_string(),
                "jpeg".to_string(),
                "--quality".to_string(),
                "0".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 1, "InvalidOptions should exit with code 1 (usage)");
    }

    // ===== Help: completions topic =====

    #[test]
    fn help_completions_shows_completions_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "completions".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Completions));
    }

    #[test]
    fn help_version_shows_version_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "version".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Version));
    }

    #[test]
    fn completions_dash_help_shows_completions_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "completions".to_string(),
            "--help".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Completions));
    }

    #[test]
    fn completions_without_shell_exits_with_usage_error() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with_io(
            vec!["truss".to_string(), "completions".to_string()],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 1, "completions without shell arg should exit 1");
    }

    // ===== Completions: implicit args are present =====

    #[test]
    fn completions_bash_includes_implicit_args() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let code = run_with_io(
            vec![
                "truss".to_string(),
                "completions".to_string(),
                "bash".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, 0);
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(
            output.contains("--output"),
            "bash completions should include --output for implicit convert"
        );
        assert!(
            output.contains("--bind"),
            "bash completions should include --bind for implicit serve"
        );
    }

    // ===== Help text: exit code 5 is documented =====

    #[test]
    fn help_exit_codes_includes_runtime() {
        let text = super::help_top_level();
        assert!(
            text.contains("5  Runtime error"),
            "help text should document exit code 5"
        );
    }

    // ===== Unknown help topic hint lists all topics =====

    #[test]
    fn unknown_help_topic_hint_lists_all_topics() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "nonexistent".to_string(),
        ]);
        let err = result.unwrap_err();
        let hint = err.hint.unwrap();
        assert!(hint.contains("completions"), "hint should list completions");
        assert!(hint.contains("version"), "hint should list version");
    }
}
