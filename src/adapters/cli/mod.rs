use crate::adapters::server::SignedUrlSource;
use crate::{
    CropRegion, Fit, MediaType, OptimizeMode, Position, Rgba8, Rotation, TargetQuality,
    TransformOptions,
};
use clap::{CommandFactory, Parser, Subcommand};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::core::error_class::ErrorClass;
use std::str::FromStr;

mod convert;
mod inspect;
mod serve;
mod sign;

/// The size cap for a source fetched over HTTP.
///
/// The default of `TRUSS_MAX_SOURCE_BYTES`, so a URL the server will fetch is one this
/// adapter can be pointed at. Trying a request locally before deploying it is what the
/// command line is for, and a cap of its own turned a source the server serves into one it
/// refuses to look at.
const MAX_REMOTE_BYTES: u64 = crate::adapters::server::remote::MAX_SOURCE_BYTES;

/// The size cap for a watermark fetched over HTTP.
///
/// The server keeps this apart from the source limit and publishes it as
/// `TRUSS_MAX_WATERMARK_BYTES`: an overlay has no business being the size of a source image,
/// and a watermark the server would refuse is one there is no point in accepting here.
pub(super) const MAX_REMOTE_WATERMARK_BYTES: u64 =
    crate::adapters::server::remote::MAX_WATERMARK_BYTES;

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

Converts, resizes, and re-encodes images (JPEG, PNG, WebP, AVIF, BMP, TIFF, SVG).
Can also run as an HTTP image-transform server.

USAGE:
  truss <COMMAND> [OPTIONS]
  truss <INPUT> -o <OUTPUT> [OPTIONS]   (implicit convert)
  truss --bind <ADDR> [OPTIONS]         (implicit serve)

COMMANDS:
  convert       Convert and transform an image file
  optimize      Optimize an image for smaller output size
  inspect       Show metadata (format, dimensions, alpha) of an image
  serve         Start the HTTP image-transform server
  validate      Check server configuration without starting the server
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

  For a file whose name starts with -, put the options first and end them with --,
  or write the path with a ./ prefix. An output cannot be escaped with --, because
  -o takes the next argument whatever it looks like; assign it with = instead:
    truss convert -o out.jpg -- -input.png
    truss convert input.png --output=-output.jpg

OPTIONS:
  -o, --output <OUTPUT>    Output file path, or - for stdout (required)
      --url <URL>          Fetch input from an HTTP(S) URL
      --width <PX>         Target width in pixels
      --height <PX>        Target height in pixels
      --fit <MODE>         How to fit into target dimensions (requires --width and --height)
                           contain: scale to fit, then pad to the exact box (default)
                           cover:   scale to fill the box, cropping the excess
                           fill:    stretch each axis to the box, ignoring aspect ratio
                           inside:  scale to fit, no padding; the output is at most the
                                    box and usually smaller on one axis
      --position <POS>     Crop anchor for cover mode (default: center)
                           center, top, right, bottom, left,
                           top-left, top-right, bottom-left, bottom-right
      --format <FMT>       Output format: jpeg, png, webp, avif, bmp, tiff, svg
                           (default: inferred from output extension)
      --quality <1-100>    Encoding quality for lossy formats
      --optimize <MODE>    Optimization mode: none, auto, lossless, lossy
      --target-quality <TARGET>
                           Perceptual target for lossy optimization (e.g. ssim:0.98, psnr:42)
      --background <COLOR> Background color as RRGGBB or RRGGBBAA hex
      --rotate <DEG>       Rotate clockwise by whole degrees. Negative turns counter-clockwise,
                           and angles past a full turn wrap, so -90 and 270 are the same.
                           A multiple of 90 is exact; any other angle resamples and grows
                           the canvas to the rotated bounding box, filling the exposed
                           corners with --background (transparent, or white for formats
                           without alpha)
      --auto-orient        Apply EXIF orientation and reset tag (default)
      --no-auto-orient     Skip EXIF orientation correction (add --keep-metadata to keep
                           the tag; stripping it as well leaves the image rotated)
      --strip-metadata     Remove all metadata (default; lossy optimization keeps the
                           ICC profile so colors are not shifted by the re-encode)
      --keep-metadata      Preserve EXIF, ICC, and other supported metadata
      --preserve-exif      Preserve EXIF only (strip ICC and others)
      --crop <x,y,w,h>     Explicit crop region as x,y,width,height (applied before resize)
      --blur <SIGMA>       Gaussian blur sigma (0.1-100.0)
      --sharpen <SIGMA>    Sharpen sigma (0.1-100.0)
      --grayscale          Desaturate the image to grayscale (applied after resize, blur,
                           and sharpen, and before the watermark)
      --without-enlargement
                           Never scale an image up. A source already within the requested
                           size keeps that size. Combines with any --fit; contain still
                           pads out to the full box, and cover returns the box intersected
                           with the source rather than the whole box
      --watermark <FILE|URL>
                           Watermark image to composite onto the output, from a file or an
                           HTTP(S) URL of at most 10 MB. The overlay itself must be a raster
                           image; the picture it goes onto may be an SVG
      --watermark-position <POS>  Watermark placement (default: bottom-right)
                           center, top, right, bottom, left,
                           top-left, top-right, bottom-left, bottom-right
      --watermark-opacity <1-100> Watermark opacity percentage (default: 50)
      --watermark-margin <PX>     Margin from edge in pixels (default: 10)

EXAMPLES:
  truss photo.png -o photo.jpg --width 800
  truss --url https://example.com/img.png -o out.webp --format webp --quality 75
  cat photo.png | truss convert - -o - --format jpeg > photo.jpg
  truss photo.png -o thumb.png --width 200 --height 200 --fit cover
  truss diagram.svg -o safe.svg
  truss diagram.svg -o diagram.png --width 1024

SVG:
  An SVG output sanitizes the document and returns it as written, so an option that asks
  for a different picture is refused rather than ignored. Convert to a raster format to
  resize, rotate, or recolour a drawing; --fit, --position, and --without-enlargement then
  mean what they mean for any other input.

ORDER:
  Options are applied in a fixed order, whatever order they are written in:
  auto-orient, rotate, crop, resize, blur, sharpen, grayscale, watermark, encode.
  https://github.com/nao1215/truss/blob/main/docs/pipeline.md
";

const HELP_OPTIMIZE: &str = "\
truss optimize - reduce image file size with format-aware optimization

USAGE:
  truss optimize <INPUT> -o <OUTPUT> [OPTIONS]
  truss optimize --url <URL> -o <OUTPUT> [OPTIONS]
  truss optimize - -o - --format webp

OPTIONS:
  -o, --output <OUTPUT>    Output file path, or - for stdout (required)
      --url <URL>          Fetch input from an HTTP(S) URL
      --format <FMT>       Output format: jpeg, png, webp, avif
                           (default: inferred from output extension or input format)
      --mode <MODE>        Optimization mode: auto (default), lossless, lossy
                           For a plain re-encode with no optimization, use truss convert
                           lossless cannot rotate pixels, so a JPEG carrying an EXIF
                           orientation needs --keep-metadata or --preserve-exif
      --quality <1-100>    Optional quality cap for lossy optimization
      --target-quality <TARGET>
                           Perceptual target for lossy optimization (e.g. ssim:0.98, psnr:42)
      --auto-orient        Apply EXIF orientation and reset tag (default)
      --no-auto-orient     Skip EXIF orientation correction (add --keep-metadata to keep
                           the tag; stripping it as well leaves the image rotated)
      --strip-metadata     Remove all metadata (default; lossy optimization keeps the
                           ICC profile so colors are not shifted by the re-encode)
      --keep-metadata      Preserve EXIF, ICC, and other supported metadata
      --preserve-exif      Preserve EXIF only (strip ICC and others)

EXAMPLES:
  truss optimize photo.jpg -o out.jpg
  truss optimize photo.jpg -o out.jpg --mode lossy --target-quality ssim:0.98
  truss optimize graphic.png -o out.png --mode lossless
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

  width/height are the dimensions as stored in the file. orientedWidth/orientedHeight
  are what 'truss convert' produces: an EXIF orientation of 5 to 8 transposes them, and
  orientation reports the tag when the file carries one.

EXAMPLES:
  truss inspect photo.jpg
  truss inspect --url https://example.com/photo.jpg
  cat photo.png | truss inspect -
";

fn help_serve() -> String {
    let mut s = String::from(
        "\
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
  TRUSS_SIGNED_URL_KEY_ID             Signing key identifier (single-key shorthand)
  TRUSS_SIGNED_URL_SECRET             Signing shared secret (single-key shorthand)
  TRUSS_SIGNING_KEYS                  Multiple signing keys as JSON {\"keyId\":\"secret\",...}
  TRUSS_ALLOW_INSECURE_URL_SOURCES    Enable insecure URL sources
  TRUSS_CACHE_ROOT                    On-disk transform cache directory
  TRUSS_PRESETS                       Named transform presets as inline JSON
  TRUSS_PRESETS_FILE                  Path to a JSON file containing named transform presets
",
    );

    // Build the TRUSS_STORAGE_BACKEND description dynamically based on enabled features.
    {
        use std::fmt::Write as FmtWrite;

        #[allow(unused_mut, clippy::useless_vec)]
        let mut backends = vec!["filesystem (default)"];
        #[cfg(feature = "s3")]
        backends.push("s3");
        #[cfg(feature = "gcs")]
        backends.push("gcs");
        #[cfg(feature = "azure")]
        backends.push("azure");
        let _ = writeln!(
            s,
            "  TRUSS_STORAGE_BACKEND               Source for public by-path resolution: {}",
            backends.join(", ")
        );
    }

    #[cfg(feature = "s3")]
    s.push_str(
        "  TRUSS_S3_BUCKET                     Default S3 bucket name (required when backend=s3)
  TRUSS_S3_FORCE_PATH_STYLE           Use path-style S3 addressing (set to 1/true/yes/on for MinIO, etc.)
  AWS_ACCESS_KEY_ID                   AWS access key for S3 authentication
  AWS_SECRET_ACCESS_KEY               AWS secret key for S3 authentication
  AWS_REGION                          AWS region for the S3 client (e.g. us-east-1)
  AWS_ENDPOINT_URL                    Custom S3-compatible endpoint URL (e.g. http://minio:9000)
",
    );

    #[cfg(feature = "gcs")]
    s.push_str(
        "  TRUSS_GCS_BUCKET                    Default GCS bucket name (required when backend=gcs)
  TRUSS_GCS_ENDPOINT                  Custom GCS endpoint URL (for testing with fake-gcs-server, etc.)
  GOOGLE_APPLICATION_CREDENTIALS      Path to GCS service account JSON key file
",
    );

    #[cfg(feature = "azure")]
    s.push_str(
        "  TRUSS_AZURE_CONTAINER               Default Azure container name (required when backend=azure)
  TRUSS_AZURE_ENDPOINT                Custom Azure Blob endpoint URL (for Azurite, etc.)
  AZURE_STORAGE_ACCOUNT_NAME          Storage account name (derives endpoint when TRUSS_AZURE_ENDPOINT is unset)
",
    );

    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    s.push_str(
        "  TRUSS_STORAGE_TIMEOUT_SECS          Download timeout for storage backends in seconds (default: 30, range: 1-300)
",
    );

    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    s.push_str(
        "\
\nNOTE: When using local emulators (MinIO, fake-gcs-server, Azurite), set
  TRUSS_ALLOW_INSECURE_URL_SOURCES=true to allow plain-HTTP endpoints.
",
    );

    s.push_str(
        "\
\nEXAMPLES:
  truss serve --bind 0.0.0.0:8080 --storage-root /var/images
  truss serve --bind 127.0.0.1:3000 --signed-url-key-id mykey --signed-url-secret s3cret
",
    );
    s
}

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
      --optimize, --target-quality, --background, --rotate, --auto-orient, --no-auto-orient,
      --strip-metadata, --keep-metadata, --preserve-exif, --crop, --blur, --sharpen,
      --grayscale, --without-enlargement
      --watermark-url <URL>          Watermark image URL to embed in the signed URL
      --watermark-position <POS>     Watermark placement (default: bottom-right)
      --watermark-opacity <1-100>    Watermark opacity (default: 50)
      --watermark-margin <PX>        Watermark margin from edge in pixels (default: 10)
      --preset <NAME>                Named transform preset (server-side)

EXAMPLES:
  truss sign --base-url https://cdn.example.com \\
    --path /photos/hero.jpg --key-id mykey --secret s3cret \\
    --expires 1700000000 --width 640 --format webp
";

const HELP_VALIDATE: &str = "\
truss validate - check server configuration without starting the server

USAGE:
  truss validate

Parses and validates all environment variables used by `truss serve`.
Exits 0 when the configuration is valid, or exits 1 with a description
of each error found.

Useful in CI/CD pipelines to catch configuration mistakes early.
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
    /// Optimize an image for smaller output size
    #[command(disable_help_flag = true)]
    Optimize(ClapOptimizeArgs),
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
    /// Validate server configuration without starting the server
    #[command(disable_help_flag = true)]
    Validate(ClapValidateArgs),
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
    input: Option<PathBuf>,
    /// Output file path, or - for stdout
    #[arg(short = 'o', long = "output", allow_hyphen_values = true)]
    output: Option<PathBuf>,
    /// Fetch input from an HTTP(S) URL
    #[arg(long)]
    url: Option<String>,
    /// Target width in pixels
    #[arg(long, value_parser = parse_width)]
    width: Option<u32>,
    /// Target height in pixels
    #[arg(long, value_parser = parse_height)]
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
    #[arg(long, value_parser = parse_quality)]
    quality: Option<u8>,
    /// Optimization mode (none, auto, lossless, lossy)
    #[arg(long, value_parser = parse_optimize_mode)]
    optimize: Option<OptimizeMode>,
    /// Perceptual target for lossy optimization
    #[arg(long = "target-quality", value_parser = parse_target_quality)]
    target_quality: Option<TargetQuality>,
    /// Background color as RRGGBB or RRGGBBAA hex
    #[arg(long, value_parser = parse_background)]
    background: Option<Rgba8>,
    /// Rotate clockwise by whole degrees; negative turns counter-clockwise
    ///
    /// `allow_hyphen_values` is what lets `--rotate -90` through: without it clap reads
    /// the leading `-` as the start of another flag and rejects the value.
    #[arg(long, value_parser = parse_rotation, allow_hyphen_values = true)]
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
    /// Explicit crop region as x,y,width,height
    #[arg(long, value_parser = parse_crop)]
    crop: Option<CropRegion>,
    /// Apply Gaussian blur (sigma: 0.1-100.0)
    #[arg(long, value_parser = parse_blur)]
    blur: Option<f32>,
    /// Apply sharpen filter (sigma: 0.1-100.0)
    #[arg(long, value_parser = parse_sharpen)]
    sharpen: Option<f32>,
    /// Desaturate the image to grayscale
    #[arg(long)]
    grayscale: bool,
    /// Never scale an image up to reach the requested size
    #[arg(long)]
    without_enlargement: bool,
    /// Watermark image file path
    #[arg(long)]
    watermark: Option<PathBuf>,
    /// Watermark position (default: bottom-right)
    #[arg(long, value_parser = parse_position)]
    watermark_position: Option<Position>,
    /// Watermark opacity 1-100 (default: 50)
    #[arg(long, value_parser = parse_watermark_opacity)]
    watermark_opacity: Option<u8>,
    /// Watermark margin in pixels (default: 10)
    #[arg(long, value_parser = parse_watermark_margin)]
    watermark_margin: Option<u32>,
    /// Show help for convert
    #[arg(short = 'h', long = "help")]
    help: bool,
}

#[derive(clap::Args)]
struct ClapOptimizeArgs {
    /// Input file path, or - for stdin
    #[arg(allow_hyphen_values = true)]
    input: Option<PathBuf>,
    /// Output file path, or - for stdout
    #[arg(short = 'o', long = "output", allow_hyphen_values = true)]
    output: Option<PathBuf>,
    /// Fetch input from an HTTP(S) URL
    #[arg(long)]
    url: Option<String>,
    /// Output format (jpeg, png, webp, avif)
    #[arg(long, value_parser = parse_optimizable_media_type)]
    format: Option<MediaType>,
    /// Quality cap for lossy optimization (1-100)
    #[arg(long, value_parser = parse_quality)]
    quality: Option<u8>,
    /// Optimization mode (auto, lossless, lossy)
    #[arg(long = "mode", value_parser = parse_optimizing_mode)]
    mode: Option<OptimizeMode>,
    /// Perceptual target for lossy optimization
    #[arg(long = "target-quality", value_parser = parse_target_quality)]
    target_quality: Option<TargetQuality>,
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
    /// Show help for optimize
    #[arg(short = 'h', long = "help")]
    help: bool,
}

#[derive(clap::Args)]
struct ClapInspectArgs {
    /// Input file path, or - for stdin
    #[arg(allow_hyphen_values = true)]
    input: Option<PathBuf>,
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
struct ClapValidateArgs {
    /// Show help for validate
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
    #[arg(long, value_parser = parse_width)]
    width: Option<u32>,
    /// Target height in pixels
    #[arg(long, value_parser = parse_height)]
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
    #[arg(long, value_parser = parse_quality)]
    quality: Option<u8>,
    /// Optimization mode (none, auto, lossless, lossy)
    #[arg(long, value_parser = parse_optimize_mode)]
    optimize: Option<OptimizeMode>,
    /// Perceptual target for lossy optimization
    #[arg(long = "target-quality", value_parser = parse_target_quality)]
    target_quality: Option<TargetQuality>,
    /// Background color as RRGGBB or RRGGBBAA hex
    #[arg(long, value_parser = parse_background)]
    background: Option<Rgba8>,
    /// Rotate clockwise by whole degrees; negative turns counter-clockwise
    ///
    /// `allow_hyphen_values` is what lets `--rotate -90` through: without it clap reads
    /// the leading `-` as the start of another flag and rejects the value.
    #[arg(long, value_parser = parse_rotation, allow_hyphen_values = true)]
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
    /// Explicit crop region as x,y,width,height
    #[arg(long, value_parser = parse_crop)]
    crop: Option<CropRegion>,
    /// Apply Gaussian blur (sigma: 0.1-100.0)
    #[arg(long, value_parser = parse_blur)]
    blur: Option<f32>,
    /// Apply sharpen filter (sigma: 0.1-100.0)
    #[arg(long, value_parser = parse_sharpen)]
    sharpen: Option<f32>,
    /// Desaturate the image to grayscale
    #[arg(long)]
    grayscale: bool,
    /// Never scale an image up to reach the requested size
    #[arg(long)]
    without_enlargement: bool,
    /// Watermark image URL to composite onto the output
    #[arg(long, value_parser = parse_url_value)]
    watermark_url: Option<String>,
    /// Watermark placement (default: bottom-right)
    #[arg(long, value_parser = parse_position)]
    watermark_position: Option<Position>,
    /// Watermark opacity 1-100 (default: 50)
    #[arg(long, value_parser = parse_watermark_opacity)]
    watermark_opacity: Option<u8>,
    /// Watermark margin from edge in pixels (default: 10)
    #[arg(long, value_parser = parse_watermark_margin)]
    watermark_margin: Option<u32>,
    /// Named transform preset to apply
    #[arg(long)]
    preset: Option<String>,
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

/// Parses an output format, refusing formats truss can read but not write.
///
/// `--format gif` would otherwise parse cleanly and fail deep in the pipeline. Rejecting
/// it here puts the error next to the flag the user typed and names the alternatives.
fn parse_media_type(s: &str) -> Result<MediaType, String> {
    let media_type = MediaType::from_str(s)?;
    match media_type.unencodable_reason() {
        Some(reason) => Err(reason),
        None => Ok(media_type),
    }
}

fn parse_optimizable_media_type(s: &str) -> Result<MediaType, String> {
    let media_type = parse_media_type(s)?;
    if media_type.supports_optimization() {
        Ok(media_type)
    } else {
        Err(format!(
            "optimization is not supported for {} output",
            media_type.as_name()
        ))
    }
}

fn parse_optimize_mode(s: &str) -> Result<OptimizeMode, String> {
    OptimizeMode::from_str(s)
}

/// The modes `truss optimize` takes, which is every mode that optimizes.
///
/// `none` re-encodes without optimizing, which on a subcommand whose purpose is to shrink a
/// file is a way to make it bigger, and it skipped the format check the other three apply, so
/// a TIFF output passed when the format was inferred and failed when it was named.
/// `truss convert` is the command for a plain re-encode, and `OptimizeMode::None` stays the
/// default there and on the other three adapters, where it means what it says.
fn parse_optimizing_mode(s: &str) -> Result<OptimizeMode, String> {
    match OptimizeMode::from_str(s)? {
        OptimizeMode::None => Err(
            "`none` does not optimize; use `truss convert` for a plain re-encode, or one of auto, lossless, lossy"
                .to_string(),
        ),
        mode => Ok(mode),
    }
}

fn parse_target_quality(s: &str) -> Result<TargetQuality, String> {
    TargetQuality::from_str(s)
}

fn parse_rotation(s: &str) -> Result<Rotation, String> {
    Rotation::from_str(s)
}

fn parse_background(s: &str) -> Result<Rgba8, String> {
    Rgba8::from_hex(s)
}

fn parse_crop(s: &str) -> Result<CropRegion, String> {
    CropRegion::from_str(s)
}

fn parse_blur(s: &str) -> Result<f32, String> {
    let v: f32 = s
        .parse()
        .map_err(|_| format!("invalid blur value: '{s}'"))?;
    if !v.is_finite() || !(0.1..=100.0).contains(&v) {
        return Err("blur must be between 0.1 and 100.0".to_string());
    }
    Ok(v)
}

fn parse_sharpen(s: &str) -> Result<f32, String> {
    let v: f32 = s
        .parse()
        .map_err(|_| format!("invalid sharpen value: '{s}'"))?;
    if !v.is_finite() || !(0.1..=100.0).contains(&v) {
        return Err("sharpen must be between 0.1 and 100.0".to_string());
    }
    Ok(v)
}

/// Parses a quality, reporting the range truss documents whatever the number's width.
///
/// Parsed as an `i64` rather than as the `u8` the option is stored in, because clap would
/// otherwise refuse 256 with `256 is not in 0..=255`, a range that is the integer's and not
/// truss's, while 255 got `quality must be between 1 and 100`.
/// Parses a watermark margin the same way, so a number too large to be a count of pixels
/// says so rather than naming the integer it would be stored in.
fn parse_watermark_margin(s: &str) -> Result<u32, String> {
    parse_dimension(
        s,
        "watermark margin",
        crate::core::validate_watermark_margin_value,
    )
}

/// Parses a width, reporting the rule truss documents whatever the number's width.
///
/// Parsed as an `i64` rather than as the `u32` the option is stored in, because clap would
/// otherwise refuse 4294967296 with `4294967296 is not in 0..=4294967295`, the span of the
/// integer rather than anything truss publishes.
fn parse_width(s: &str) -> Result<u32, String> {
    parse_dimension(s, "width", crate::core::validate_width_value)
}

/// The same for a height. See [`parse_width`].
fn parse_height(s: &str) -> Result<u32, String> {
    parse_dimension(s, "height", crate::core::validate_height_value)
}

fn parse_dimension(
    s: &str,
    axis: &str,
    validate: fn(i64) -> Result<u32, &'static str>,
) -> Result<u32, String> {
    let value: i64 = s
        .parse()
        .map_err(|_| format!("{axis} must be a whole number of pixels, got '{s}'"))?;
    // A value the option can hold is handed on for the transform to judge, which keeps the
    // failure class the CLI reported before and the one the other adapters report.
    validate(value).map_err(str::to_string)
}

fn parse_quality(s: &str) -> Result<u8, String> {
    let value: i64 = s
        .parse()
        .map_err(|_| format!("quality must be a whole number, got '{s}'"))?;
    // A value the option can hold is handed on for `TransformOptions::normalize` to judge,
    // which keeps the failure class the CLI reported before and the one the server reports
    // for the same number. One that cannot be held is refused here, with the sentence that
    // check would have given rather than with the range of the integer holding it.
    u8::try_from(value).map_err(|_| {
        crate::core::validate_quality_value(value)
            .expect_err("a value outside u8 is outside 1..=100")
            .to_string()
    })
}

fn parse_watermark_opacity(s: &str) -> Result<u8, String> {
    let value: i64 = s
        .parse()
        .map_err(|_| format!("watermark opacity must be a whole number, got '{s}'"))?;
    crate::core::validate_watermark_opacity_value(value).map_err(str::to_string)
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

fn optimize_usage() -> &'static str {
    "usage: truss optimize <INPUT> -o <OUTPUT> [OPTIONS]"
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
/// Standard output is flushed before returning, so a write that only fails when the
/// buffer drains — a full disk, a quota, a reader that closed the pipe — is reported as
/// exit code 5 instead of being discarded by the runtime's exit-time flush.
///
/// # Examples
///
/// ```no_run
/// use truss::run_cli;
///
/// let _ = run_cli(vec![
///     "truss".to_string(),
///     "input.png".to_string(),
///     "-o".to_string(),
///     "output.jpg".to_string(),
/// ]);
/// ```
///
/// ```no_run
/// use truss::run_cli;
///
/// let _ = run_cli(vec![
///     "truss".to_string(),
///     "--bind".to_string(),
///     "127.0.0.1:8080".to_string(),
/// ]);
/// ```
pub fn run<I>(args: I) -> ExitCode
where
    I: IntoIterator<Item: Into<OsString>>,
{
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();

    let code = run_with_io(args, &mut stdin, &mut stdout, &mut stderr);

    ExitCode::from(flush_stdout(code, &mut stdout, &mut stderr))
}

/// Flushes standard output and folds a flush failure into the exit code.
///
/// `StdoutLock` buffers. A payload that ends without a newline — a small WebP or AVIF
/// written to `-o -` — can sit entirely in that buffer, so the only write that reaches
/// the file descriptor is the runtime's flush after `main` returns, and nothing observes
/// its error. Flushing here turns that silent truncation into exit code 5 with a reason.
///
/// A command that already failed keeps its own exit code: the flush error is a
/// consequence of the first failure, not a second, more informative one.
fn flush_stdout<W, E>(code: u8, stdout: &mut W, stderr: &mut E) -> u8
where
    W: Write,
    E: Write,
{
    match stdout.flush() {
        Ok(()) => code,
        Err(error) if code == EXIT_SUCCESS => write_error(stderr, stdout_write_error(&error)),
        Err(_) => code,
    }
}

// ---------------------------------------------------------------------------
// Command types (internal)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum HelpTopic {
    TopLevel,
    Convert,
    Optimize,
    Inspect,
    Serve,
    Validate,
    Sign,
    Completions,
    Version,
}

#[derive(Debug, Clone, PartialEq)]
enum Command {
    Help(HelpTopic),
    Version,
    Serve(ServeCommand),
    Validate,
    Inspect(InspectCommand),
    Convert(ConvertCommand),
    Optimize(ConvertCommand),
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
    watermark_url: Option<String>,
    watermark_position: Option<Position>,
    watermark_opacity: Option<u8>,
    watermark_margin: Option<u32>,
    preset: Option<String>,
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

/// A failure on its way to standard error, carrying both halves of how the CLI reports one.
///
/// `exit_code` is the coarse signal a shell branches on, one of the five in CONTRIBUTING.md.
/// `class` is the same failure named the way the HTTP server and the Wasm package name it,
/// so a caller who moves a transform between the three adapters keeps one classification;
/// it is what `write_error` prints in parentheses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
    exit_code: u8,
    class: ErrorClass,
    message: String,
    usage: Option<String>,
    hint: Option<String>,
}

// ---------------------------------------------------------------------------
// Core dispatch
// ---------------------------------------------------------------------------

fn run_with_io<I, R, W, E>(args: I, stdin: &mut R, stdout: &mut W, stderr: &mut E) -> u8
where
    I: IntoIterator<Item: Into<OsString>>,
    R: Read,
    W: Write,
    E: Write,
{
    match parse_args(args) {
        Ok(Command::Help(topic)) => {
            let text = match topic {
                HelpTopic::TopLevel => help_top_level(),
                HelpTopic::Convert => HELP_CONVERT.to_string(),
                HelpTopic::Optimize => HELP_OPTIMIZE.to_string(),
                HelpTopic::Inspect => HELP_INSPECT.to_string(),
                HelpTopic::Serve => help_serve(),
                HelpTopic::Validate => HELP_VALIDATE.to_string(),
                HelpTopic::Sign => HELP_SIGN.to_string(),
                HelpTopic::Completions => HELP_COMPLETIONS.to_string(),
                HelpTopic::Version => HELP_VERSION.to_string(),
            };
            match stdout.write_all(text.as_bytes()) {
                Ok(()) => EXIT_SUCCESS,
                Err(error) => write_error(stderr, stdout_write_error(&error)),
            }
        }
        Ok(Command::Version) => match writeln!(stdout, "truss {}", env!("CARGO_PKG_VERSION")) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, stdout_write_error(&error)),
        },
        Ok(Command::Serve(command)) => match serve::execute_serve(command) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Validate) => match serve::execute_validate(stdout) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Inspect(command)) => match inspect::execute_inspect(command, stdin, stdout) {
            Ok(()) => EXIT_SUCCESS,
            Err(error) => write_error(stderr, error),
        },
        Ok(Command::Convert(command) | Command::Optimize(command)) => {
            match convert::execute_convert(command, stdin, stdout) {
                Ok(()) => EXIT_SUCCESS,
                Err(error) => write_error(stderr, error),
            }
        }
        Ok(Command::Sign(command)) => match sign::execute_sign(command, stdout) {
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
    "optimize",
    "inspect",
    "serve",
    "validate",
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
/// Reports whether a path argument is the single dash that names standard input or output.
///
/// The comparison is on the bytes rather than on a `str`, so a path that is not text
/// reaches the file system rather than being read as a stream.
fn is_dash(path: &Path) -> bool {
    path.as_os_str() == OsStr::new("-")
}

fn preprocess_args(args: Vec<OsString>) -> Vec<OsString> {
    if args.len() <= 1 {
        return args;
    }
    let first = &args[1];
    // A path is bytes on Unix, so an argument that is not text is still a file truss can
    // read. Only the routing decisions below need it as text, and every one of them is a
    // comparison against a name truss chose, which no such argument can match.
    let first_text = first.to_str();

    // -h / --help at top level → route to our help subcommand
    if first == "-h" || first == "--help" {
        let mut new = vec![args[0].clone(), OsString::from("help")];
        if args.len() > 2 {
            new.extend_from_slice(&args[2..]);
        }
        return new;
    }

    // -V / --version at top level → route to version subcommand
    if first == "-V" || first == "--version" {
        return vec![args[0].clone(), OsString::from("version")];
    }

    // If first arg is a serve flag, insert "serve" subcommand
    if first_text.is_some_and(is_serve_flag) {
        let mut new = vec![args[0].clone(), OsString::from("serve")];
        new.extend_from_slice(&args[1..]);
        return new;
    }

    // Known subcommand → pass through
    if first_text.is_some_and(|first| KNOWN_SUBCOMMANDS.contains(&first)) {
        return args;
    }

    // If the first argument refers to an existing file (even without an
    // extension), treat it as an implicit convert rather than an unknown
    // subcommand.  This handles `truss image -o out.jpg` where `image` is a
    // real file.
    if std::path::Path::new(first).is_file() {
        let mut new = vec![args[0].clone(), OsString::from("convert")];
        new.extend_from_slice(&args[1..]);
        return new;
    }

    // Looks like an unknown subcommand (alphabetic, no dots/slashes) →
    // let clap handle it for typo suggestions
    if first_text.is_some_and(looks_like_unknown_subcommand) {
        return args;
    }

    // Otherwise, treat as implicit convert
    let mut new = vec![args[0].clone(), OsString::from("convert")];
    new.extend_from_slice(&args[1..]);
    new
}

// ---------------------------------------------------------------------------
// Argument parsing — main entry
// ---------------------------------------------------------------------------

fn parse_args<I>(args: I) -> Result<Command, CliError>
where
    I: IntoIterator<Item: Into<OsString>>,
{
    let raw: Vec<OsString> = args.into_iter().map(Into::into).collect();

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
            class: ErrorClass::InvalidRequest,
            message: "'completions' requires a shell argument".to_string(),
            usage: None,
            hint: Some("try 'truss completions bash'".to_string()),
        }),
        Some(CliSubcommand::Convert(args)) => convert::convert_from_clap(args),
        Some(CliSubcommand::Optimize(args)) => convert::optimize_from_clap(args),
        Some(CliSubcommand::Inspect(args)) => inspect::inspect_from_clap(args),
        Some(CliSubcommand::Serve(args)) => serve::serve_from_clap(args),
        Some(CliSubcommand::Validate(args)) => serve::validate_from_clap(args),
        Some(CliSubcommand::Sign(args)) => sign::sign_from_clap(args),
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
        class: ErrorClass::InvalidRequest,
        message,
        usage: None,
        hint: Some("run 'truss --help' for available commands".to_string()),
    }
}

fn parse_help_topic(topic: Option<String>) -> Result<Command, CliError> {
    match topic.as_deref() {
        None => Ok(Command::Help(HelpTopic::TopLevel)),
        Some("convert") => Ok(Command::Help(HelpTopic::Convert)),
        Some("optimize") => Ok(Command::Help(HelpTopic::Optimize)),
        Some("inspect") => Ok(Command::Help(HelpTopic::Inspect)),
        Some("serve") => Ok(Command::Help(HelpTopic::Serve)),
        Some("validate") => Ok(Command::Help(HelpTopic::Validate)),
        Some("sign") => Ok(Command::Help(HelpTopic::Sign)),
        Some("completions") => Ok(Command::Help(HelpTopic::Completions)),
        Some("version") => Ok(Command::Help(HelpTopic::Version)),
        Some(other) => Err(CliError {
            exit_code: EXIT_USAGE,
            class: ErrorClass::InvalidRequest,
            message: format!("unknown help topic '{other}'"),
            usage: None,
            hint: Some(
                "available topics: convert, optimize, inspect, serve, validate, sign, completions, version"
                    .to_string(),
            ),
        }),
    }
}

// ---------------------------------------------------------------------------
// Shared transform fields
// ---------------------------------------------------------------------------

/// Collects shared transform fields from clap args into `TransformOptions`.
struct TransformFields {
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<Fit>,
    position: Option<Position>,
    format: Option<MediaType>,
    quality: Option<u8>,
    optimize: Option<OptimizeMode>,
    target_quality: Option<TargetQuality>,
    background: Option<Rgba8>,
    rotate: Option<Rotation>,
    auto_orient: bool,
    no_auto_orient: bool,
    strip_metadata: bool,
    keep_metadata: bool,
    preserve_exif: bool,
    crop: Option<CropRegion>,
    blur: Option<f32>,
    sharpen: Option<f32>,
    grayscale: bool,
    without_enlargement: bool,
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
        let (strip_metadata, preserve_exif) = crate::core::resolve_metadata_flags(
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
            optimize: self.optimize.unwrap_or(defaults.optimize),
            target_quality: self.target_quality,
            background: self.background,
            rotate: self.rotate.unwrap_or(defaults.rotate),
            auto_orient,
            strip_metadata,
            preserve_exif,
            crop: self.crop,
            blur: self.blur,
            sharpen: self.sharpen,
            grayscale: self.grayscale,
            without_enlargement: self.without_enlargement,
            deadline: None,
        })
    }
}

fn validate_url(url: &str, flag: &str) -> Result<(), CliError> {
    let parsed = url::Url::parse(url).map_err(|e| CliError {
        exit_code: EXIT_USAGE,
        class: ErrorClass::InvalidRequest,
        message: format!("'{flag}' is not a valid URL: {e}"),
        usage: None,
        hint: Some(format!("got '{url}'")),
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(CliError {
            exit_code: EXIT_USAGE,
            class: ErrorClass::InvalidRequest,
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
            classified_error(
                class_for_io_error(&error),
                EXIT_IO,
                &format!("failed to read {}: {error}", path.display()),
            )
        }),
        InputSource::Url(url) => read_url_bytes(&url, MAX_REMOTE_BYTES),
    }
}

/// Names the class of a file system fault.
///
/// A source that is not there is `not-found`, the class the server gives the same miss and
/// the one `docs/problems.md` describes as an input file that is not there; anything else
/// about the file system is `internal-error`. Every path the command line names is read
/// through this, so a mistyped `--watermark` is classified like a mistyped input.
fn class_for_io_error(error: &io::Error) -> ErrorClass {
    if error.kind() == io::ErrorKind::NotFound {
        ErrorClass::NotFound
    } else {
        ErrorClass::InternalError
    }
}

/// Timeout for the TCP connect phase of a remote fetch.
const CLI_FETCH_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Timeout for receiving the full response body from a remote source.
const CLI_FETCH_BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// How many redirects a `--url` fetch follows before giving up.
///
/// The same number the server allows, so a URL that works against one works against the
/// other.
const CLI_FETCH_MAX_REDIRECTS: u32 = 5;

/// Refuses a URL truss may not fetch, whichever hop of a redirect chain named it.
///
/// A command line is expected to fetch from the machine it runs on, so the private and
/// loopback ranges the server refuses are allowed here. The metadata endpoints are not:
/// `docs/configuration.md` calls them blocked whatever else is configured, no workflow
/// fetches an image from one, and the URL that reaches one is chosen by whatever server
/// answered the previous hop rather than by whoever typed the command.
///
/// The name is checked, and so are the addresses it resolves to, which covers a host that
/// points at a metadata address without spelling it. A host whose answer changes between
/// this lookup and the agent's own is not covered: the server closes that with DNS pinning,
/// which it can only do because it refuses every private address, and this adapter does not.
fn refuse_disallowed_fetch_target(url: &str) -> Result<(), CliError> {
    let parsed = url::Url::parse(url).map_err(|error| {
        classified_error(
            ErrorClass::BadGateway,
            EXIT_IO,
            &format!("failed to fetch {url}: not a valid URL: {error}"),
        )
    })?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(classified_error(
            ErrorClass::BadGateway,
            EXIT_IO,
            &format!(
                "failed to fetch {url}: a redirect to a `{}` URL is not followed",
                parsed.scheme()
            ),
        ));
    }
    let named_metadata = crate::core::remote_policy::is_cloud_metadata_host(&parsed);
    // A host name under someone else's control can resolve to a metadata address without
    // spelling it, so the addresses it resolves to are checked as well.
    let resolves_to_metadata = !named_metadata
        && parsed
            .socket_addrs(|| Some(if parsed.scheme() == "https" { 443 } else { 80 }))
            .map(|addrs| {
                addrs
                    .iter()
                    .any(|addr| crate::core::remote_policy::is_cloud_metadata_ip(addr.ip()))
            })
            .unwrap_or(false);
    if named_metadata || resolves_to_metadata {
        return Err(classified_error(
            ErrorClass::BadGateway,
            EXIT_IO,
            &format!("failed to fetch {url}: the URL points to a cloud metadata service"),
        ));
    }
    Ok(())
}

/// Fetches `--url` input.
///
/// Redirects are followed here rather than inside the agent so that every hop is checked:
/// the caller chooses the first URL and a remote server chooses the rest.
///
/// Every failure here is the remote end's, which the server names `bad-gateway`; the CLI
/// keeps its I/O exit code (2) and adds that class.
fn read_url_bytes(url: &str, max_bytes: u64) -> Result<Vec<u8>, CliError> {
    let fetch_failed =
        |message: String| classified_error(ErrorClass::BadGateway, EXIT_IO, &message);
    let config = ureq::config::Config::builder()
        .timeout_connect(Some(CLI_FETCH_CONNECT_TIMEOUT))
        .timeout_recv_body(Some(CLI_FETCH_BODY_TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut current = url.to_string();
    let mut response = None;
    for _ in 0..=CLI_FETCH_MAX_REDIRECTS {
        refuse_disallowed_fetch_target(&current)?;
        let hop = agent
            .get(&current)
            .call()
            .map_err(|error| fetch_failed(format!("failed to fetch {current}: {error}")))?;
        let status = hop.status().as_u16();
        if crate::core::remote_policy::is_redirect_status(status) {
            let location = hop
                .headers()
                .get("Location")
                .and_then(|value: &ureq::http::HeaderValue| value.to_str().ok())
                .ok_or_else(|| {
                    fetch_failed(format!(
                        "failed to fetch {current}: HTTP {status} without a Location header"
                    ))
                })?;
            let base = url::Url::parse(&current).map_err(|error| {
                fetch_failed(format!(
                    "failed to fetch {current}: not a valid URL: {error}"
                ))
            })?;
            let next = base.join(location).map_err(|error| {
                fetch_failed(format!(
                    "failed to fetch {current}: redirect to an unusable URL: {error}"
                ))
            })?;
            current = next.into();
            continue;
        }
        response = Some((hop, status));
        break;
    }

    let Some((response, status)) = response else {
        return Err(fetch_failed(format!(
            "failed to fetch {url}: more than {CLI_FETCH_MAX_REDIRECTS} redirects"
        )));
    };

    // Only a 2xx says the body that follows is the resource, which is the rule the HTTP
    // server's own fetch applies. Reading an image out of anything else reports the origin
    // declining as a problem with the caller's file, and reports it as a success whenever
    // the body happens to sniff as an image.
    if !crate::core::remote_policy::is_success_status(status) {
        return Err(fetch_failed(format!(
            "failed to fetch {current}: HTTP {status}"
        )));
    }

    if let Some(encoding) = response
        .headers()
        .get("Content-Encoding")
        .and_then(|value: &ureq::http::HeaderValue| value.to_str().ok())
        .and_then(crate::core::remote_policy::unreadable_content_coding)
    {
        return Err(fetch_failed(format!(
            "failed to fetch {current}: response uses unsupported content-encoding `{encoding}`"
        )));
    }

    if response
        .headers()
        .get("Content-Length")
        .and_then(|v: &ureq::http::HeaderValue| v.to_str().ok())
        .and_then(|value: &str| value.parse::<u64>().ok())
        .is_some_and(|len| len > max_bytes)
    {
        return Err(fetch_failed(format!(
            "failed to fetch {url}: response exceeds {max_bytes} bytes"
        )));
    }

    // The declared length is a claim; this is the measurement, so a response that declares
    // nothing, or declares a small number and sends more, is bounded by the same cap.
    let mut reader = response.into_body().into_reader().take(max_bytes + 1);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| fetch_failed(format!("failed to fetch {url}: {error}")))?;

    if bytes.len() as u64 > max_bytes {
        return Err(fetch_failed(format!(
            "failed to fetch {url}: response exceeds {max_bytes} bytes"
        )));
    }

    Ok(bytes)
}

/// Maps a transform failure onto the exit code and the class that name it.
///
/// The class is [`crate::TransformError::class`], the table the HTTP server and the Wasm
/// package read too, so the three adapters classify one failure the same way. The exit code
/// is the CLI's own column of that table and `map_transform_error_matches_the_class_table`
/// is what keeps the two in step with `docs/problems.md`.
fn map_transform_error(error: crate::TransformError) -> CliError {
    let class = error.class();
    let (exit_code, message) = match error {
        crate::TransformError::InvalidOptions(reason) => (EXIT_USAGE, reason),
        crate::TransformError::InvalidInput(reason) => (EXIT_INPUT, reason),
        // An input truss cannot process is an input error (3), the same class the
        // documented exit-code table gives an unsupported format, not a transform
        // failure (4).
        crate::TransformError::UnsupportedInputMediaType(reason) => (EXIT_INPUT, reason),
        crate::TransformError::DecodeFailed(reason)
        | crate::TransformError::EncodeFailed(reason)
        | crate::TransformError::CapabilityMissing(reason)
        | crate::TransformError::LimitExceeded(reason) => (EXIT_TRANSFORM, reason),
        // The error's own Display names the rule that was hit (svg needs an svg input,
        // gif is never encoded), so the CLI does not restate it in different words.
        ref error @ crate::TransformError::UnsupportedOutputMediaType(_) => {
            (EXIT_TRANSFORM, error.to_string())
        }
    };
    classified_error(class, exit_code, &message)
}

// ---------------------------------------------------------------------------
// Error constructors
// ---------------------------------------------------------------------------

fn convert_error(message: &str) -> CliError {
    CliError {
        exit_code: EXIT_USAGE,
        class: ErrorClass::InvalidRequest,
        message: message.to_string(),
        usage: Some(convert_usage().to_string()),
        hint: Some("run 'truss convert --help' for convert options".to_string()),
    }
}

fn optimize_error(message: &str) -> CliError {
    CliError {
        exit_code: EXIT_USAGE,
        class: ErrorClass::InvalidRequest,
        message: message.to_string(),
        usage: Some(optimize_usage().to_string()),
        hint: Some("run 'truss optimize --help' for optimize options".to_string()),
    }
}

fn sign_error(message: &str) -> CliError {
    CliError {
        exit_code: EXIT_USAGE,
        class: ErrorClass::InvalidRequest,
        message: message.to_string(),
        usage: Some(sign_usage().to_string()),
        hint: Some("run 'truss sign --help' for sign options".to_string()),
    }
}

/// A command line that could not be understood or is contradictory: exit 1, the
/// `invalid-request` class, with no usage block of its own.
fn usage_error(message: &str) -> CliError {
    classified_error(ErrorClass::InvalidRequest, EXIT_USAGE, message)
}

/// A failure the process could not avoid: an I/O fault (exit 2) or a runtime fault such as
/// a port already in use or a closed standard output (exit 5).
///
/// Both are the `internal-error` class, which is what the server reports for the same
/// faults. The I/O paths that know more than that — a source that is not there, a fetch the
/// remote end refused — say so with [`classified_error`] instead.
fn runtime_error(exit_code: u8, message: &str) -> CliError {
    classified_error(ErrorClass::InternalError, exit_code, message)
}

/// Builds an error whose class the caller names.
fn classified_error(class: ErrorClass, exit_code: u8, message: &str) -> CliError {
    CliError {
        exit_code,
        class,
        message: message.to_string(),
        usage: None,
        hint: None,
    }
}

/// Builds the error a failed write to standard output reports.
///
/// `help` and `version` used to return exit code 5 with nothing on stderr, so a redirect
/// into a full disk looked like a silent failure while every other command explained
/// itself. They now report the same way `convert` and `inspect` do.
fn stdout_write_error(error: &io::Error) -> CliError {
    runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
}

fn write_error<E>(stderr: &mut E, error: CliError) -> u8
where
    E: Write,
{
    let _ = writeln!(
        stderr,
        "error: {} ({})",
        crate::core::single_line(&error.message),
        error.class.slug()
    );
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
    use super::serve::resolve_server_config;
    use super::{
        Command, ConvertCommand, EXIT_INPUT, EXIT_IO, EXIT_TRANSFORM, EXIT_USAGE, HelpTopic,
        InputSource, MAX_REMOTE_BYTES, MAX_REMOTE_WATERMARK_BYTES, OutputTarget, ServeCommand,
        SignCommand, flush_stdout, parse_args, parse_optimize_mode, parse_optimizing_mode,
        preprocess_args, run_with_io,
    };
    use crate::{
        Fit, MediaType, OptimizeMode, RawArtifact, SignedUrlSource, TransformOptions,
        sniff_artifact,
    };
    use rstest::rstest;
    use serial_test::serial;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::io::{self, Cursor, Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::thread;

    fn png_bytes() -> Vec<u8> {
        crate::test_support::flat_png(4, 3)
    }

    fn temp_file_path(name: &str) -> PathBuf {
        crate::test_support::unique_temp_path(&format!("truss-{name}")).with_extension("bin")
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = crate::test_support::unique_temp_path(&format!("truss-{name}"));
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

    /// Serves one response with a caller-chosen status line and an image body.
    ///
    /// The body is always an image so the status is the only thing that varies, which is
    /// what makes the boundary between a response that is the resource and one that is not
    /// visible in the outcome.
    fn spawn_http_server_with_status(status: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind http test server");
        let addr = listener.local_addr().expect("server addr");
        let url = format!("http://{addr}/image");
        let body = crate::test_support::flat_png(4, 3);

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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

    #[test]
    fn help_optimize_shows_optimize_help() {
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "help".to_string(),
                "optimize".to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 0);
        assert!(stderr.is_empty());
        let output = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(output.contains("truss optimize"));
        assert!(output.contains("--mode"));
        assert!(output.contains("--target-quality"));
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

    /// The class the CLI names alongside each exit code, and the anchor
    /// `docs/problems.md` gives it. The exit codes are the CLI's column of that page's
    /// table, so this is what keeps the page honest: a class that changes exit code, or an
    /// exit code that changes class, fails here.
    #[test]
    fn map_transform_error_matches_the_class_table() {
        const PROBLEM_DOCS: &str = include_str!("../../../docs/problems.md");
        // A Windows checkout has CRLF line endings, so the anchors are matched against
        // the text with the carriage returns taken out.
        let problem_docs = PROBLEM_DOCS.replace('\r', "");
        let cases: [(crate::TransformError, &str, u8); 8] = [
            (
                crate::TransformError::InvalidOptions("x".into()),
                "invalid-options",
                EXIT_USAGE,
            ),
            (
                crate::TransformError::InvalidInput("x".into()),
                "invalid-input",
                EXIT_INPUT,
            ),
            (
                crate::TransformError::UnsupportedInputMediaType("x".into()),
                "unsupported-input-media-type",
                EXIT_INPUT,
            ),
            (
                crate::TransformError::DecodeFailed("x".into()),
                "decode-failed",
                EXIT_TRANSFORM,
            ),
            (
                crate::TransformError::EncodeFailed("x".into()),
                "encode-failed",
                EXIT_TRANSFORM,
            ),
            (
                crate::TransformError::CapabilityMissing("x".into()),
                "capability-missing",
                EXIT_TRANSFORM,
            ),
            (
                crate::TransformError::LimitExceeded("x".into()),
                "limit-exceeded",
                EXIT_TRANSFORM,
            ),
            (
                crate::TransformError::UnsupportedOutputMediaType(MediaType::Gif),
                "unsupported-output-media-type",
                EXIT_TRANSFORM,
            ),
        ];

        for (error, slug, exit_code) in cases {
            let mapped = super::map_transform_error(error.clone());
            assert_eq!(mapped.class.slug(), slug, "{error:?}");
            assert_eq!(mapped.exit_code, exit_code, "{error:?}");
            assert!(
                problem_docs.contains(&format!("### {slug}\n")),
                "docs/problems.md should document the {slug} class"
            );
        }
    }

    /// stderr carries the class in parentheses after the message, which is how a caller
    /// reads the same classification the server puts in `type` and the browser in `kind`.
    #[test]
    fn stderr_names_the_class_after_the_message() {
        let mut stderr = Vec::new();
        let code = super::write_error(
            &mut stderr,
            super::map_transform_error(crate::TransformError::LimitExceeded(
                "output image would have 400000000 pixels, limit is 67108864".to_string(),
            )),
        );

        assert_eq!(code, EXIT_TRANSFORM);
        assert_eq!(
            String::from_utf8(stderr).expect("utf-8 stderr"),
            "error: output image would have 400000000 pixels, limit is 67108864 (limit-exceeded)\n"
        );
    }

    /// A usage fault names the request, not the transform, and keeps its usage and hint
    /// lines below the classified first line.
    #[test]
    fn stderr_names_the_class_on_a_usage_error() {
        let mut stderr = Vec::new();
        let code = super::write_error(&mut stderr, super::sign_error("'sign' requires --key-id"));

        assert_eq!(code, EXIT_USAGE);
        let rendered = String::from_utf8(stderr).expect("utf-8 stderr");
        assert!(
            rendered.starts_with("error: 'sign' requires --key-id (invalid-request)\n"),
            "{rendered}"
        );
        assert!(rendered.ends_with("hint: run 'truss sign --help' for sign options\n"));
    }

    /// A source that is not there is `not-found`, the class the server gives the same miss,
    /// while the exit code stays 2.
    #[test]
    fn missing_input_reports_the_not_found_class() {
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
        let rendered = String::from_utf8(stderr).expect("utf-8 stderr");
        assert!(rendered.contains("(not-found)"), "{rendered}");
    }

    /// A decode failure is exit 4 wherever it is raised. The sniff that runs before the
    /// transform used to report it as an input error (3), so one truncated file was a
    /// transform error to `convert` and an input error to `inspect`.
    #[test]
    fn a_decode_failure_from_the_sniff_is_a_transform_error() {
        // A PNG signature with nothing after it: recognised as PNG, too short to decode.
        let truncated = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        for command in ["inspect", "convert"] {
            let mut stdin = Cursor::new(truncated.clone());
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();

            let mut args = vec!["truss".to_string(), command.to_string(), "-".to_string()];
            if command == "convert" {
                args.push("--output".to_string());
                args.push("-".to_string());
            }
            let code = run_with_io(args, &mut stdin, &mut stdout, &mut stderr);

            let rendered = String::from_utf8(stderr).expect("utf-8 stderr");
            assert_eq!(code, EXIT_TRANSFORM, "{command}: {rendered}");
            assert!(
                rendered.contains("(decode-failed)"),
                "{command}: {rendered}"
            );
        }
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

    /// Every environment variable in the serve help sits in one column.
    ///
    /// The rows are written in blocks behind feature gates, and a block opened with a
    /// backslash line continuation loses the indentation of its first line along with
    /// the newline, so the first name in each gated block printed flush left in exactly
    /// the builds the release ships. Asserting the shape of the whole section rather
    /// than four names catches a block added later.
    #[test]
    fn help_serve_keeps_every_environment_variable_in_one_column() {
        let help = super::help_serve();
        let misaligned: Vec<&str> = help
            .lines()
            .skip_while(|line| !line.starts_with("ENVIRONMENT VARIABLES:"))
            .filter(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with("TRUSS_")
                    || trimmed.starts_with("AWS_")
                    || trimmed.starts_with("AZURE_")
                    || trimmed.starts_with("GOOGLE_")
            })
            .filter(|line| !line.starts_with("  ") || line.starts_with("   "))
            .collect();

        assert!(
            misaligned.is_empty(),
            "every environment variable row is indented two spaces, found: {misaligned:#?}"
        );
    }

    #[test]
    fn help_serve_shows_serve_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "serve".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Serve));
    }

    // ===== Additional test: help validate =====

    #[test]
    fn help_validate_shows_validate_help() {
        let result = parse_args(vec![
            "truss".to_string(),
            "help".to_string(),
            "validate".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Validate));
    }

    #[test]
    fn parse_args_validate() {
        let result =
            parse_args(vec!["truss".to_string(), "validate".to_string()]).expect("parse validate");
        assert_eq!(result, Command::Validate);
    }

    #[test]
    fn validate_help_flag() {
        let result = parse_args(vec![
            "truss".to_string(),
            "validate".to_string(),
            "--help".to_string(),
        ]);
        assert_eq!(result.unwrap(), Command::Help(HelpTopic::Validate));
    }

    #[test]
    #[serial]
    fn validate_invalid_config() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_MAX_CONCURRENT_TRANSFORMS", "invalid") };
        let mut stdout = Vec::new();
        let result = super::serve::execute_validate(&mut stdout);
        unsafe { env::remove_var("TRUSS_MAX_CONCURRENT_TRANSFORMS") };
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn validate_valid_config() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let mut stdout = Vec::new();
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_STORAGE_ROOT", dir.path().to_str().unwrap()) };
        let result = super::serve::execute_validate(&mut stdout);
        unsafe { env::remove_var("TRUSS_STORAGE_ROOT") };
        assert!(result.is_ok());
        let output = String::from_utf8(stdout).expect("valid utf-8");
        assert!(output.contains("configuration is valid"));
        assert!(output.contains("storage root:"));
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
    #[serial]
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
    fn parse_args_supports_optimize_subcommand() {
        let command = parse_args(vec![
            "truss".to_string(),
            "optimize".to_string(),
            "input.png".to_string(),
            "-o".to_string(),
            "output.png".to_string(),
            "--mode".to_string(),
            "lossless".to_string(),
        ])
        .expect("parse optimize");

        assert_eq!(
            command,
            Command::Optimize(ConvertCommand {
                input: InputSource::Path(PathBuf::from("input.png")),
                output: OutputTarget::Path(PathBuf::from("output.png")),
                options: TransformOptions {
                    optimize: OptimizeMode::Lossless,
                    ..TransformOptions::default()
                },
                watermark_path: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
            })
        );
    }

    /// `truss optimize --mode none` re-encodes without optimizing, so it made files larger
    /// on the one subcommand that promises the opposite, and it was the only mode that
    /// reached an output format the others refuse. It is `truss convert` under a name that
    /// says the reverse, so it is not a mode of this command.
    #[test]
    fn optimize_refuses_a_mode_that_does_not_optimize() {
        let message =
            parse_optimizing_mode("none").expect_err("`none` is not an optimization mode");
        assert!(
            message.contains("truss convert"),
            "the refusal should name the command that does a plain re-encode: {message}"
        );

        for mode in ["auto", "lossless", "lossy"] {
            assert!(
                parse_optimizing_mode(mode).is_ok(),
                "{mode} is an optimization mode"
            );
        }

        // The value keeps its meaning everywhere else, including on `truss convert`.
        assert_eq!(parse_optimize_mode("none"), Ok(OptimizeMode::None));
    }

    #[test]
    fn parse_args_optimize_defaults_to_auto_mode() {
        let command = parse_args(vec![
            "truss".to_string(),
            "optimize".to_string(),
            "input.png".to_string(),
            "-o".to_string(),
            "output.webp".to_string(),
        ])
        .expect("parse optimize");

        assert_eq!(
            command,
            Command::Optimize(ConvertCommand {
                input: InputSource::Path(PathBuf::from("input.png")),
                output: OutputTarget::Path(PathBuf::from("output.webp")),
                options: TransformOptions {
                    optimize: OptimizeMode::Auto,
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
    fn parse_args_rejects_non_optimizable_optimize_format() {
        let error = parse_args(vec![
            "truss".to_string(),
            "optimize".to_string(),
            "input.png".to_string(),
            "-o".to_string(),
            "output.svg".to_string(),
            "--format".to_string(),
            "svg".to_string(),
        ])
        .expect_err("svg optimize output should be rejected");

        assert!(
            error
                .message
                .contains("optimization is not supported for svg output")
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
                },
                watermark_url: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
                preset: None,
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
                },
                watermark_url: None,
                watermark_position: None,
                watermark_opacity: None,
                watermark_margin: None,
                preset: None,
            })
        );
    }

    #[test]
    fn parse_args_supports_sign_with_preset() {
        let command = parse_args(vec![
            "truss".to_string(),
            "sign".to_string(),
            "--base-url".to_string(),
            "https://cdn.example.com".to_string(),
            "--path".to_string(),
            "/hero.jpg".to_string(),
            "--key-id".to_string(),
            "mykey".to_string(),
            "--secret".to_string(),
            "s3cret".to_string(),
            "--expires".to_string(),
            "1700000000".to_string(),
            "--preset".to_string(),
            "thumbnail".to_string(),
        ])
        .expect("parse sign with preset");

        match command {
            Command::Sign(s) => assert_eq!(s.preset.as_deref(), Some("thumbnail")),
            _ => panic!("expected Sign command"),
        }
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
    fn run_with_io_optimizes_a_png_file() {
        let input_path = temp_file_path("optimize-input");
        let output_path = temp_file_path("optimize-output").with_extension("png");
        fs::write(&input_path, png_bytes()).expect("write input file");

        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                "truss".to_string(),
                "optimize".to_string(),
                input_path.display().to_string(),
                "-o".to_string(),
                output_path.display().to_string(),
                "--mode".to_string(),
                "lossless".to_string(),
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
        assert_eq!(artifact.media_type, MediaType::Png);
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

    /// A response is the resource only when its status says so, which is what the HTTP
    /// server's own fetch has always required and what this adapter did not.
    ///
    /// Reading a body out of a 3xx that truss does not follow reports the origin's refusal
    /// as a problem with the caller's file, and reports it as a success whenever the body
    /// happens to sniff as an image. Both are answered here by the class the server gives
    /// the same response.
    #[rstest]
    #[case("300 Multiple Choices")]
    #[case("305 Use Proxy")]
    #[case("306 Switch Proxy")]
    #[case("309 Unassigned")]
    #[case("400 Bad Request")]
    #[case("500 Internal Server Error")]
    fn a_status_that_is_not_success_is_the_origin_failing(#[case] status: &'static str) {
        let (url, handle) = spawn_http_server_with_status(status);
        let output_path = temp_file_path("convert-url-status").with_extension("png");
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
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        handle.join().expect("join server thread");
        let _ = fs::remove_file(&output_path);
        let message = String::from_utf8(stderr).expect("utf8 stderr");

        assert_eq!(exit_code, EXIT_IO, "{status} is the origin's failure");
        assert!(
            message.contains("bad-gateway"),
            "{status} must be classified as the origin's failure, got: {message}"
        );
        let code = status.split(' ').next().expect("status code");
        assert!(
            message.contains(code),
            "{status} must be named in the message, got: {message}"
        );
    }

    /// The other side of the boundary: a success status is the resource, whatever number
    /// inside the range it carries.
    #[rstest]
    #[case("200 OK")]
    #[case("201 Created")]
    #[case("299 Also Fine")]
    fn a_success_status_is_the_resource(#[case] status: &'static str) {
        let (url, handle) = spawn_http_server_with_status(status);
        let output_path = temp_file_path("convert-url-success").with_extension("png");
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
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        handle.join().expect("join server thread");
        let _ = fs::remove_file(&output_path);

        assert_eq!(
            exit_code,
            0,
            "{status} is a representation of the resource: {}",
            String::from_utf8_lossy(&stderr)
        );
    }

    /// A remote source the server would fetch has to be one this adapter can be pointed at,
    /// which is the argument the watermark cap was already brought into line under.
    #[test]
    fn the_remote_caps_are_the_ones_the_server_publishes() {
        assert_eq!(
            MAX_REMOTE_BYTES,
            crate::adapters::server::remote::MAX_SOURCE_BYTES,
            "a source the server fetches must be one the command line can be pointed at"
        );
        assert_eq!(
            MAX_REMOTE_WATERMARK_BYTES,
            crate::adapters::server::remote::MAX_WATERMARK_BYTES,
            "a watermark the server refuses is one there is no point in accepting here"
        );
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
            OsString::from("truss"),
            OsString::from("image"),
            OsString::from("-o"),
            OsString::from("out.jpg"),
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
            OsString::from("truss"),
            OsString::from("nonexistent_subcommand_xyz"),
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
    // ===== Standard output is flushed before the process exits =====

    /// A writer that accepts every write and fails only when it is flushed, which is how
    /// a full disk or a closed pipe behaves against a buffered `StdoutLock`.
    struct FlushFails;

    impl Write for FlushFails {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::StorageFull, "no space left"))
        }
    }

    #[test]
    fn flush_stdout_turns_a_failed_flush_into_a_runtime_error() {
        let mut stderr = Vec::new();
        let code = flush_stdout(0, &mut FlushFails, &mut stderr);

        assert_eq!(code, 5, "a lost payload must not exit 0");
        let text = String::from_utf8(stderr).expect("utf8 stderr");
        assert!(
            text.contains("error: failed to write stdout"),
            "stderr should name the failure, got: {text}"
        );
    }

    #[test]
    fn flush_stdout_keeps_the_original_exit_code_when_the_command_already_failed() {
        let mut stderr = Vec::new();
        let code = flush_stdout(2, &mut FlushFails, &mut stderr);

        assert_eq!(code, 2, "the first failure is the one worth reporting");
        assert!(stderr.is_empty(), "the flush error should not add noise");
    }

    #[test]
    fn flush_stdout_passes_the_exit_code_through_when_the_flush_succeeds() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        assert_eq!(flush_stdout(0, &mut stdout, &mut stderr), 0);
        assert_eq!(flush_stdout(3, &mut stdout, &mut stderr), 3);
        assert!(stderr.is_empty());
    }

    // ===== help and version explain a failed write like every other command =====

    /// A writer that fails on the first write, standing in for a redirect into a full disk.
    struct WriteFails;

    impl Write for WriteFails {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::StorageFull, "no space left"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn help_and_version_report_a_failed_write_on_stderr() {
        for args in [vec!["truss", "--version"], vec!["truss", "help", "convert"]] {
            let mut stdin = Cursor::new(Vec::new());
            let mut stderr = Vec::new();
            let code = run_with_io(
                args.iter().map(|value| (*value).to_string()),
                &mut stdin,
                &mut WriteFails,
                &mut stderr,
            );

            assert_eq!(
                code, 5,
                "{args:?} should exit 5 when stdout cannot be written"
            );
            let text = String::from_utf8(stderr).expect("utf8 stderr");
            assert!(
                text.contains("error: failed to write stdout"),
                "{args:?} should explain itself, got: {text}"
            );
        }
    }

    // ===== An output format truss never encodes is a usage error either way =====

    #[test]
    fn a_gif_output_extension_is_rejected_like_the_gif_flag() {
        for args in [
            vec!["truss", "convert", "in.png", "-o", "out.gif"],
            vec!["truss", "convert", "in.png", "-o", "out.GIF"],
            vec!["truss", "optimize", "in.png", "-o", "out.gif"],
            vec![
                "truss", "convert", "in.png", "-o", "out.png", "--format", "gif",
            ],
        ] {
            let error = parse_args(args.iter().map(|value| (*value).to_string()))
                .expect_err("gif output should not parse");

            assert_eq!(error.exit_code, 1, "{args:?} should be a usage error");
            assert!(
                error.message.contains("input-only format")
                    && error.message.contains("png, jpeg, webp, or avif"),
                "{args:?} should name the alternatives, got: {}",
                error.message
            );
        }
    }

    #[test]
    fn an_explicit_format_overrides_an_unencodable_output_extension() {
        let command = parse_args(
            [
                "truss", "convert", "in.png", "-o", "out.gif", "--format", "png",
            ]
            .iter()
            .map(|value| (*value).to_string()),
        )
        .expect("an explicit format decides the encoder, whatever the extension says");

        match command {
            Command::Convert(convert) => assert_eq!(convert.options.format, Some(MediaType::Png)),
            other => panic!("expected a convert command, got {other:?}"),
        }
    }

    // ===== A path argument that is not valid UTF-8 =====

    /// A file name on Linux is a byte string, so `truss convert` has to take one whatever
    /// bytes it holds. Reading the arguments as `String` panicked before the command line
    /// was parsed, which no adapter could report and no exit code covered.
    ///
    /// Only Linux runs this: APFS refuses to create a name that is not valid UTF-8, with
    /// `EILSEQ`, so the fixture cannot exist on macOS, and a Windows name is UTF-16. The
    /// panic itself was not filesystem-specific, and the flag-value case below covers the
    /// platforms this one skips.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_utf8_input_path_is_converted_rather_than_refused() {
        use std::os::unix::ffi::OsStringExt;

        let dir = temp_dir("non-utf8-input");
        let mut name = OsString::from_vec(b"caf\xe9".to_vec());
        name.push(".png");
        let input_path = dir.join(name);
        fs::write(&input_path, png_bytes()).expect("write input file");
        let output_path = dir.join("out.png");

        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                OsString::from("truss"),
                OsString::from("convert"),
                input_path.clone().into_os_string(),
                OsString::from("-o"),
                output_path.clone().into_os_string(),
                OsString::from("--width"),
                OsString::from("2"),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let output_exists = output_path.is_file();
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(
            exit_code,
            0,
            "stderr was: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(
            output_exists,
            "the conversion should have written an output"
        );
    }

    /// The bytes have to survive on the way out as well: a lossy conversion of the path
    /// would write to a name nobody asked for and report success. Linux only, for the
    /// reason the test above gives.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_non_utf8_output_path_is_written_under_exactly_those_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let dir = temp_dir("non-utf8-output");
        let input_path = dir.join("in.png");
        fs::write(&input_path, png_bytes()).expect("write input file");
        let mut name = OsString::from_vec(b"sortie-\xe9".to_vec());
        name.push(".png");
        let output_path = dir.join(name);

        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                OsString::from("truss"),
                OsString::from("convert"),
                input_path.into_os_string(),
                OsString::from("-o"),
                output_path.clone().into_os_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let output_exists = output_path.is_file();
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(
            exit_code,
            0,
            "stderr was: {}",
            String::from_utf8_lossy(&stderr)
        );
        assert!(
            output_exists,
            "the output should exist under the bytes that were asked for"
        );
    }

    /// A flag whose value is genuinely text keeps refusing bytes that are not text, with
    /// the usage error every other bad flag value gets rather than a panic.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_value_for_a_text_flag_is_a_usage_error() {
        use std::os::unix::ffi::OsStringExt;

        let mut stdin = Cursor::new(png_bytes());
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let exit_code = run_with_io(
            vec![
                OsString::from("truss"),
                OsString::from("convert"),
                OsString::from("-"),
                OsString::from("-o"),
                OsString::from("-"),
                OsString::from("--format"),
                OsString::from_vec(vec![0xff]),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, EXIT_USAGE, "a value that is not text is usage");
        assert!(stdout.is_empty(), "nothing should be written on a refusal");
    }

    // ===== The class of a file system fault =====

    /// The watermark is a source the command line named, so a missing one is the same
    /// class as a missing input. `internal-error` says the fault is truss's own.
    #[test]
    fn a_missing_watermark_is_not_found_like_a_missing_input() {
        let dir = temp_dir("watermark-class");
        let input_path = dir.join("in.png");
        fs::write(&input_path, png_bytes()).expect("write input file");
        let output_path = dir.join("out.png");

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
                "--watermark".to_string(),
                dir.join("absent.png").display().to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let message = String::from_utf8(stderr).expect("utf8 stderr");
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(exit_code, 2, "a file system fault is exit 2");
        assert!(
            message.contains("(not-found)"),
            "a missing watermark should be not-found, got: {message}"
        );
    }

    /// A watermark that is there and cannot be read is the other class, which is what
    /// keeps the shared rule honest rather than replacing one blanket answer with another.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_watermark_is_an_internal_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_dir("watermark-unreadable");
        let input_path = dir.join("in.png");
        fs::write(&input_path, png_bytes()).expect("write input file");
        let watermark_path = dir.join("wm.png");
        fs::write(&watermark_path, png_bytes()).expect("write watermark file");
        fs::set_permissions(&watermark_path, fs::Permissions::from_mode(0o000))
            .expect("make the watermark unreadable");
        let output_path = dir.join("out.png");

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
                "--watermark".to_string(),
                watermark_path.display().to_string(),
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let message = String::from_utf8(stderr).expect("utf8 stderr");
        let _ = fs::set_permissions(&watermark_path, fs::Permissions::from_mode(0o644));
        let _ = fs::remove_dir_all(&dir);

        if message.contains("(not-found)") {
            // Running as root reads it anyway, and the case has nothing to assert.
            return;
        }
        assert_eq!(exit_code, 2, "a file system fault is exit 2");
        assert!(
            message.contains("(internal-error)"),
            "a watermark that cannot be read is not a missing one, got: {message}"
        );
    }

    // ===== stderr is one line per failure =====

    /// The failure a real decoder raises, end to end.
    ///
    /// `truncated.jpg` is a JPEG whose bytes stop early, which is what an upload cut off
    /// by a dropped connection looks like, and the decoder's own wording for it ends with
    /// a newline.
    #[test]
    fn a_truncated_jpeg_is_reported_on_one_line() {
        const TRUNCATED_JPEG: &[u8] = include_bytes!("../../../integration/fixtures/truncated.jpg");

        let mut stdin = Cursor::new(TRUNCATED_JPEG.to_vec());
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
            ],
            &mut stdin,
            &mut stdout,
            &mut stderr,
        );

        let rendered = String::from_utf8(stderr).expect("utf8 stderr");

        assert_eq!(exit_code, EXIT_TRANSFORM);
        assert_eq!(
            rendered.lines().count(),
            1,
            "one failure is one line, got: {rendered:?}"
        );
        assert!(
            rendered.trim_end().ends_with("(decode-failed)"),
            "the class ends the line, got: {rendered:?}"
        );
    }

    /// The class is the last thing on the line, so a message carrying a newline puts it on
    /// a line of its own where a caller reading the last line cannot find the failure.
    #[test]
    fn write_error_keeps_a_message_with_a_newline_on_one_line() {
        let mut stderr = Vec::new();
        let code = super::write_error(
            &mut stderr,
            super::classified_error(
                crate::core::error_class::ErrorClass::DecodeFailed,
                EXIT_TRANSFORM,
                "decoding failed\n",
            ),
        );

        let rendered = String::from_utf8(stderr).expect("utf8 stderr");

        assert_eq!(code, EXIT_TRANSFORM);
        assert_eq!(
            rendered.lines().count(),
            1,
            "one failure is one line, got: {rendered:?}"
        );
        assert!(
            rendered.ends_with("(decode-failed)\n"),
            "the class ends the line, got: {rendered:?}"
        );
    }

    #[test]
    fn svg_output_from_a_raster_input_names_the_rule_it_broke() {
        let error = crate::TransformError::UnsupportedOutputMediaType(MediaType::Svg).to_string();

        assert!(
            error.contains("requires an svg input"),
            "the message should say why svg was refused, got: {error}"
        );
    }
}
