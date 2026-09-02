//! Shared Core types for transformations, validation, and media inspection.

use std::error::Error;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

#[cfg(feature = "avif")]
pub(crate) use avif::avif_clean_aperture;
// Not gated with the decoder: `smaller_passthrough` is compiled in every build and reads
// this, and the container walk needs no decoder anyway.
pub(crate) use avif::avif_carries_metadata;
use avif::{avif_orientation, has_avif_brand, sniff_avif};

// The shared failure vocabulary is only read by the adapters, so a build with none of them
// (`--no-default-features`) leaves it out rather than carrying an unused table.
/// The AVIF container walk, which is long enough to read on its own.
mod avif;
#[cfg(any(feature = "server", feature = "wasm"))]
pub(crate) mod error_class;
/// Gated with the `url` crate the address rules parse with, which the server feature brings
/// in and which the two adapters that fetch are the only users of.
#[cfg(feature = "server")]
pub(crate) mod remote_policy;

/// Maximum number of pixels in the output image (width × height).
///
/// This limit prevents resize operations from producing excessively large
/// output buffers. The value matches the API specification in `docs/openapi.yaml`.
///
/// ```
/// assert_eq!(truss::MAX_OUTPUT_PIXELS, 67_108_864);
/// ```
pub const MAX_OUTPUT_PIXELS: u64 = 67_108_864;

/// Maximum number of decoded pixels allowed for an input image (width × height).
///
/// This limit prevents decompression bombs from consuming unbounded memory.
/// The value matches the API specification in `docs/openapi.yaml`.
///
/// ```
/// assert_eq!(truss::MAX_DECODED_PIXELS, 100_000_000);
/// ```
pub const MAX_DECODED_PIXELS: u64 = 100_000_000;

/// Maximum number of decoded pixels allowed for a watermark image.
///
/// This prevents a single watermark overlay from dominating memory during
/// compositing. The value (4 MP) is generous for typical watermarks.
///
/// ```
/// assert_eq!(truss::MAX_WATERMARK_PIXELS, 4_000_000);
/// ```
pub const MAX_WATERMARK_PIXELS: u64 = 4_000_000;

/// A (width, height) pair that prevents accidental transposition of dimensions.
///
/// Using a named struct instead of separate `u32` parameters ensures call sites
/// cannot silently swap width and height.
///
/// ```
/// use truss::Dimensions;
/// let d = Dimensions::new(1920, 1080);
/// assert_eq!(d.width, 1920);
/// assert_eq!(d.height, 1080);
/// assert_eq!(d.pixel_count(), 1920 * 1080);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[must_use]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

impl Dimensions {
    /// Creates a new dimensions value.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Returns the total pixel count (width × height) as `u64` to avoid overflow.
    #[must_use]
    pub const fn pixel_count(self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

impl fmt::Display for Dimensions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// Raw input bytes before media-type detection has completed.
///
/// # Examples
///
/// ```
/// use truss::{RawArtifact, MediaType};
///
/// let raw = RawArtifact::new(vec![0xFF, 0xD8, 0xFF], Some(MediaType::Jpeg));
/// assert_eq!(raw.declared_media_type, Some(MediaType::Jpeg));
///
/// let unknown = RawArtifact::new(vec![1, 2, 3], None);
/// assert!(unknown.declared_media_type.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawArtifact {
    /// The raw input bytes.
    pub bytes: Vec<u8>,
    /// The media type declared by an adapter, if one is available.
    pub declared_media_type: Option<MediaType>,
}

impl RawArtifact {
    /// Creates a new raw artifact value.
    pub fn new(bytes: Vec<u8>, declared_media_type: Option<MediaType>) -> Self {
        Self {
            bytes,
            declared_media_type,
        }
    }
}

/// A decoded or otherwise classified artifact handled by the Core layer.
///
/// # Examples
///
/// ```
/// use truss::{Artifact, ArtifactMetadata, MediaType};
///
/// let artifact = Artifact::new(
///     vec![0x89, b'P', b'N', b'G'],
///     MediaType::Png,
///     ArtifactMetadata::default(),
/// );
/// assert_eq!(artifact.media_type, MediaType::Png);
/// assert_eq!(artifact.metadata.frame_count, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct Artifact {
    /// The artifact bytes.
    pub bytes: Vec<u8>,
    /// The detected media type for the bytes.
    pub media_type: MediaType,
    /// Additional metadata extracted from the artifact.
    pub metadata: ArtifactMetadata,
}

impl Artifact {
    /// Creates a new artifact value.
    pub fn new(bytes: Vec<u8>, media_type: MediaType, metadata: ArtifactMetadata) -> Self {
        Self {
            bytes,
            media_type,
            metadata,
        }
    }
}

/// Metadata that the Core layer can carry between decode and encode steps.
///
/// # Examples
///
/// ```
/// use truss::{ArtifactMetadata, Dimensions};
///
/// let meta = ArtifactMetadata {
///     width: Some(1920),
///     height: Some(1080),
///     ..ArtifactMetadata::default()
/// };
/// assert_eq!(meta.dimensions(), Some(Dimensions::new(1920, 1080)));
/// assert_eq!(meta.frame_count, 1);
///
/// // With no orientation tag, the oriented dimensions are the stored ones.
/// assert_eq!(meta.oriented_dimensions(), Some(Dimensions::new(1920, 1080)));
///
/// // Orientation 6 is a quarter turn, so a transform swaps the axes.
/// let rotated = ArtifactMetadata {
///     orientation: Some(6),
///     ..meta.clone()
/// };
/// assert_eq!(rotated.oriented_dimensions(), Some(Dimensions::new(1080, 1920)));
///
/// // When either dimension is unknown, dimensions() returns None
/// let partial = ArtifactMetadata { width: Some(100), ..ArtifactMetadata::default() };
/// assert!(partial.dimensions().is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMetadata {
    /// The rendered width in pixels, when known.
    pub width: Option<u32>,
    /// The rendered height in pixels, when known.
    pub height: Option<u32>,
    /// The number of frames contained in the artifact.
    pub frame_count: u32,
    /// The total animation duration, when known.
    pub duration: Option<Duration>,
    /// Whether the artifact contains alpha, when known.
    pub has_alpha: Option<bool>,
    /// The EXIF orientation tag, when the artifact carries one.
    ///
    /// `width` and `height` are the dimensions as stored in the container. A transform
    /// applies this tag by default, and values 5 to 8 transpose the two, so a caller that
    /// records dimensions at upload time and serves derivatives later needs this to know
    /// which way round the result will be. [`ArtifactMetadata::oriented_dimensions`] does
    /// that arithmetic.
    pub orientation: Option<u16>,
}

impl ArtifactMetadata {
    /// Returns the dimensions as a [`Dimensions`] value, if both width and height are known.
    pub fn dimensions(&self) -> Option<Dimensions> {
        match (self.width, self.height) {
            (Some(w), Some(h)) => Some(Dimensions::new(w, h)),
            _ => None,
        }
    }

    /// Returns the dimensions after the EXIF orientation is applied.
    ///
    /// These are the dimensions a transform produces with auto-orientation on, which is the
    /// default. They equal [`ArtifactMetadata::dimensions`] whenever there is no orientation
    /// tag or the tag does not transpose the axes, so a caller can read these unconditionally.
    pub fn oriented_dimensions(&self) -> Option<Dimensions> {
        let dimensions = self.dimensions()?;
        Some(if orientation_transposes(self.orientation) {
            Dimensions::new(dimensions.height, dimensions.width)
        } else {
            dimensions
        })
    }
}

/// Reports whether an EXIF orientation swaps the width and the height.
///
/// Values 5 to 8 include a quarter turn; 1 to 4 do not, and anything else is ignored the way
/// the transform pipeline ignores it.
pub(crate) const fn orientation_transposes(orientation: Option<u16>) -> bool {
    matches!(orientation, Some(5..=8))
}

impl Default for ArtifactMetadata {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            frame_count: 1,
            duration: None,
            has_alpha: None,
            orientation: None,
        }
    }
}

/// Supported media types for the current implementation phase.
///
/// # Examples
///
/// ```
/// use truss::MediaType;
/// use std::str::FromStr;
///
/// let mt = MediaType::from_str("png").unwrap();
/// assert_eq!(mt, MediaType::Png);
/// assert_eq!(mt.as_name(), "png");
/// assert_eq!(mt.as_mime(), "image/png");
/// assert!(!mt.is_lossy());
/// assert!(mt.is_raster());
///
/// assert!(MediaType::Jpeg.is_lossy());
/// assert!(!MediaType::Svg.is_raster());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaType {
    /// JPEG image data.
    Jpeg,
    /// PNG image data.
    Png,
    /// WebP image data.
    Webp,
    /// AVIF image data.
    Avif,
    /// SVG image data.
    Svg,
    /// BMP image data.
    Bmp,
    /// TIFF image data.
    Tiff,
    /// GIF image data.
    ///
    /// GIF is an input-only format: truss decodes it but never encodes it, the same way
    /// it treats a raster input requesting SVG output. Requesting `gif` output returns
    /// [`TransformError::UnsupportedOutputMediaType`].
    Gif,
}

impl MediaType {
    /// Returns the canonical media type name used by the API and CLI.
    #[must_use]
    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Avif => "avif",
            Self::Svg => "svg",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
            Self::Gif => "gif",
        }
    }

    /// Returns the canonical MIME type string.
    #[must_use]
    pub const fn as_mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
            Self::Svg => "image/svg+xml",
            Self::Bmp => "image/bmp",
            Self::Tiff => "image/tiff",
            Self::Gif => "image/gif",
        }
    }

    /// Reports whether the media type is typically encoded with lossy quality controls.
    #[must_use]
    pub const fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Webp | Self::Avif)
    }

    /// Returns `true` if the format participates in the optimization pipeline.
    #[must_use]
    pub const fn supports_optimization(self) -> bool {
        matches!(self, Self::Jpeg | Self::Png | Self::Webp | Self::Avif)
    }

    /// Returns `true` if the format supports lossy optimization controls.
    #[must_use]
    pub const fn supports_lossy_optimization(self) -> bool {
        matches!(self, Self::Jpeg | Self::Webp | Self::Avif)
    }

    /// Returns `true` if the encoded format can carry an embedded ICC profile.
    ///
    /// AVIF signals color through the container's `colr` box rather than a profile truss can
    /// write, and BMP/TIFF/SVG output has no profile path in this pipeline.
    #[must_use]
    pub const fn supports_icc_profile(self) -> bool {
        matches!(self, Self::Jpeg | Self::Png | Self::Webp)
    }

    /// Returns `true` if this is a raster (bitmap) format, `false` for vector formats.
    #[must_use]
    pub const fn is_raster(self) -> bool {
        !matches!(self, Self::Svg)
    }

    /// Returns `true` if truss can encode this format, not merely decode it.
    ///
    /// GIF is decode-only: animation, palette quantization, and frame disposal are a
    /// different problem from the single-frame pipeline, so truss reads GIF input and
    /// writes one of the formats it fully supports. SVG is encodable only in the sense
    /// that an SVG input can be sanitized back out as SVG, which
    /// [`crate::codecs::transform`] handles on its own path.
    #[must_use]
    pub const fn is_encodable(self) -> bool {
        !matches!(self, Self::Gif)
    }

    /// Why this format cannot be an output, or `None` when it can be one.
    ///
    /// A format that parses is not automatically a format truss writes, and the four
    /// adapters used to each carry their own copy of that sentence. This is the one copy:
    /// the CLI rejects the flag value with it, the Wasm package and the HTTP server refuse
    /// the request with it, and all three do so before the input is read rather than after
    /// the picture has been decoded.
    pub(crate) fn unencodable_reason(self) -> Option<String> {
        (!self.is_encodable()).then(|| {
            format!(
                "{} is an input-only format; choose an output format such as png, jpeg, webp, or avif",
                self.as_name()
            )
        })
    }

    /// The output format to use when a request does not name one.
    ///
    /// Normally that is the input's own format, so a transform without an explicit
    /// `format` does not silently re-encode into something else. A decode-only input has
    /// no such option: GIF falls back to PNG, which is lossless and reproduces a palette
    /// and a transparent color index exactly.
    ///
    /// Every adapter that has to resolve a missing format goes through here, so the rule
    /// cannot drift between the CLI, the server, and the WASM build.
    ///
    /// # Examples
    ///
    /// ```
    /// use truss::MediaType;
    ///
    /// assert_eq!(MediaType::Jpeg.default_output(), MediaType::Jpeg);
    /// assert_eq!(MediaType::Gif.default_output(), MediaType::Png);
    /// ```
    #[must_use]
    pub const fn default_output(self) -> Self {
        if self.is_encodable() { self } else { Self::Png }
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_mime())
    }
}

impl FromStr for MediaType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            "png" => Ok(Self::Png),
            "webp" => Ok(Self::Webp),
            "avif" => Ok(Self::Avif),
            "svg" => Ok(Self::Svg),
            "bmp" => Ok(Self::Bmp),
            "tiff" | "tif" => Ok(Self::Tiff),
            "gif" => Ok(Self::Gif),
            _ => Err(format!("unsupported media type `{value}`")),
        }
    }
}

/// A watermark image to composite onto the output.
///
/// The watermark is alpha-composited onto the main image after all other
/// transforms (resize, blur) and before encoding.
///
/// ```
/// use truss::{Artifact, ArtifactMetadata, MediaType, Position, WatermarkInput};
///
/// let wm = WatermarkInput {
///     image: Artifact::new(vec![0], MediaType::Png, ArtifactMetadata::default()),
///     position: Position::BottomRight,
///     opacity: 50,
///     margin: 10,
/// };
/// assert_eq!(wm.opacity, 50);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatermarkInput {
    /// The watermark image (already classified via [`sniff_artifact`]).
    pub image: Artifact,
    /// Where to place the watermark on the main image.
    pub position: Position,
    /// Opacity of the watermark (1–100). Default: 50.
    pub opacity: u8,
    /// Margin in pixels from the nearest edge. Default: 10.
    pub margin: u32,
}

/// A complete transform request for the Core layer.
///
/// # Examples
///
/// ```
/// use truss::{Artifact, ArtifactMetadata, MediaType, TransformOptions, TransformRequest};
///
/// let input = Artifact::new(vec![0], MediaType::Png, ArtifactMetadata::default());
/// let request = TransformRequest::new(input, TransformOptions::default());
/// assert!(request.watermark.is_none());
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct TransformRequest {
    /// The already-resolved input artifact.
    pub input: Artifact,
    /// Raw transform options as provided by an adapter.
    pub options: TransformOptions,
    /// Optional watermark image to composite onto the output.
    pub watermark: Option<WatermarkInput>,
}

impl TransformRequest {
    /// Creates a new transform request.
    pub fn new(input: Artifact, options: TransformOptions) -> Self {
        Self {
            input,
            options,
            watermark: None,
        }
    }

    /// Creates a new transform request with a watermark.
    pub fn with_watermark(
        input: Artifact,
        options: TransformOptions,
        watermark: WatermarkInput,
    ) -> Self {
        Self {
            input,
            options,
            watermark: Some(watermark),
        }
    }

    /// Normalizes the request into a form that does not require adapter-specific defaults.
    pub fn normalize(self) -> Result<NormalizedTransformRequest, TransformError> {
        let options = self.options.normalize(self.input.media_type)?;

        if let Some(ref wm) = self.watermark {
            validate_watermark(wm)?;
        }

        Ok(NormalizedTransformRequest {
            input: self.input,
            options,
            watermark: self.watermark,
        })
    }
}

/// A fully normalized transform request.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedTransformRequest {
    /// The normalized input artifact.
    pub input: Artifact,
    /// Fully normalized transform options.
    pub options: NormalizedTransformOptions,
    /// Optional watermark to composite onto the output.
    pub watermark: Option<WatermarkInput>,
}

/// An explicit crop region applied before resize.
///
/// Crop extracts a rectangular sub-image at the given pixel coordinates.
/// The origin `(x, y)` is the top-left corner and `(width, height)` define
/// the size of the extracted region. Both dimensions must be non-zero.
///
/// # Examples
///
/// ```
/// use truss::CropRegion;
/// use std::str::FromStr;
///
/// let region = CropRegion::from_str("10,20,100,200").unwrap();
/// assert_eq!(region.x, 10);
/// assert_eq!(region.y, 20);
/// assert_eq!(region.width, 100);
/// assert_eq!(region.height, 200);
/// assert_eq!(format!("{region}"), "10,20,100,200");
///
/// // Zero-size regions are rejected
/// assert!(CropRegion::from_str("0,0,0,100").is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRegion {
    /// Horizontal offset from the left edge.
    pub x: u32,
    /// Vertical offset from the top edge.
    pub y: u32,
    /// Width of the crop region.
    pub width: u32,
    /// Height of the crop region.
    pub height: u32,
}

impl FromStr for CropRegion {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() != 4 {
            return Err(format!(
                "crop must be x,y,w,h (four comma-separated integers), got '{s}'"
            ));
        }
        let x = parts[0]
            .parse::<u32>()
            .map_err(|_| format!("crop x must be a non-negative integer, got '{}'", parts[0]))?;
        let y = parts[1]
            .parse::<u32>()
            .map_err(|_| format!("crop y must be a non-negative integer, got '{}'", parts[1]))?;
        let width = parts[2].parse::<u32>().map_err(|_| {
            format!(
                "crop width must be a non-negative integer, got '{}'",
                parts[2]
            )
        })?;
        let height = parts[3].parse::<u32>().map_err(|_| {
            format!(
                "crop height must be a non-negative integer, got '{}'",
                parts[3]
            )
        })?;
        if width == 0 || height == 0 {
            return Err("crop width and height must be greater than zero".to_string());
        }
        Ok(CropRegion {
            x,
            y,
            width,
            height,
        })
    }
}

impl fmt::Display for CropRegion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{},{},{}", self.x, self.y, self.width, self.height)
    }
}

/// Optimization policy applied near the final encoding stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OptimizeMode {
    /// Keep the current encoding behavior with no extra optimization work.
    #[default]
    None,
    /// Pick the most appropriate optimization strategy for the target format.
    Auto,
    /// Only use lossless size-reduction techniques.
    Lossless,
    /// Allow controlled quality loss for smaller output files.
    Lossy,
}

impl OptimizeMode {
    /// Returns the canonical option name used by the API, CLI, and WASM adapter.
    #[must_use]
    pub const fn as_name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Auto => "auto",
            Self::Lossless => "lossless",
            Self::Lossy => "lossy",
        }
    }
}

impl fmt::Display for OptimizeMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_name())
    }
}

impl FromStr for OptimizeMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "none" => Ok(Self::None),
            "auto" => Ok(Self::Auto),
            "lossless" => Ok(Self::Lossless),
            "lossy" => Ok(Self::Lossy),
            _ => Err(format!("unsupported optimize mode `{value}`")),
        }
    }
}

/// Perceptual metric used for lossy optimization quality targeting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityMetric {
    /// Structural similarity index.
    Ssim,
    /// Peak signal-to-noise ratio.
    Psnr,
}

impl QualityMetric {
    /// Returns the canonical metric name used in textual forms such as `ssim:0.98`.
    #[must_use]
    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Ssim => "ssim",
            Self::Psnr => "psnr",
        }
    }
}

impl fmt::Display for QualityMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_name())
    }
}

impl FromStr for QualityMetric {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ssim" => Ok(Self::Ssim),
            "psnr" => Ok(Self::Psnr),
            _ => Err(format!("unsupported target quality metric `{value}`")),
        }
    }
}

/// A perceptual quality target used when binary-searching a lossy encode quality.
///
/// The search is a binary search over `1..=quality` when a `quality` is given and `1..=100`
/// otherwise, and it assumes the score rises with the quality setting. Encoders do not
/// promise that: rate control changes quantization as the setting moves, and a perceptual
/// score against the original can fall a little on the way up. So the quality returned is
/// one whose score meets `value` rather than necessarily the least one that would, and where
/// no probed quality meets it, the top of the range is returned together with a
/// [`TransformWarning::TargetQualityNotReached`] naming the score that encode reached.
/// Guaranteeing the minimum would mean scanning the range at an encode, a decode, and a
/// metric per step, against the handful the search makes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetQuality {
    /// The requested quality metric.
    pub metric: QualityMetric,
    /// The threshold that the encoded output should meet or exceed.
    pub value: f32,
}

impl fmt::Display for TargetQuality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.metric.as_name(), self.value)
    }
}

impl FromStr for TargetQuality {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (metric, raw_value) = value.split_once(':').ok_or_else(|| {
            "targetQuality must be <metric>:<value>, for example ssim:0.98".to_string()
        })?;
        // Spelled the way every other named value in the vocabulary is: `fit`, `position`,
        // `format`, and the optimize mode all match what the caller wrote, so a metric that
        // accepted any case was the one flag whose lesson did not carry to the next.
        let metric = QualityMetric::from_str(metric)?;
        let value = raw_value
            .parse::<f32>()
            .map_err(|_| format!("target quality value must be a number, got `{raw_value}`"))?;

        Ok(Self { metric, value })
    }
}

pub(crate) fn default_lossy_target_quality(media_type: MediaType) -> Option<TargetQuality> {
    let value = match media_type {
        MediaType::Jpeg | MediaType::Webp => 0.985,
        MediaType::Avif => 0.99,
        _ => return None,
    };

    Some(TargetQuality {
        metric: QualityMetric::Ssim,
        value,
    })
}

/// Raw transform options before defaulting and validation has completed.
///
/// Use `TransformOptions::default()` as a starting point and override the fields
/// you need. Call [`TransformOptions::normalize`] to validate and resolve defaults.
///
/// # Examples
///
/// ```
/// use truss::{TransformOptions, MediaType, Rotation};
///
/// let opts = TransformOptions {
///     width: Some(800),
///     height: Some(600),
///     format: Some(MediaType::Webp),
///     quality: Some(80),
///     rotate: Rotation::DEG_90,
///     ..TransformOptions::default()
/// };
/// assert_eq!(opts.width, Some(800));
/// assert_eq!(opts.quality, Some(80));
/// assert_eq!(opts.rotate, Rotation::DEG_90);
/// // strip_metadata defaults to true
/// assert!(opts.strip_metadata);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct TransformOptions {
    /// The desired output width in pixels.
    pub width: Option<u32>,
    /// The desired output height in pixels.
    pub height: Option<u32>,
    /// The requested resize fit mode.
    pub fit: Option<Fit>,
    /// The requested positioning mode.
    pub position: Option<Position>,
    /// The requested output format.
    pub format: Option<MediaType>,
    /// The requested lossy quality.
    pub quality: Option<u8>,
    /// The requested optimization mode.
    pub optimize: OptimizeMode,
    /// Optional perceptual target used by lossy optimization.
    pub target_quality: Option<TargetQuality>,
    /// The requested background color.
    pub background: Option<Rgba8>,
    /// The requested extra rotation.
    pub rotate: Rotation,
    /// Whether EXIF-based auto-orientation should run.
    pub auto_orient: bool,
    /// Whether metadata should be stripped from the output.
    pub strip_metadata: bool,
    /// Whether EXIF metadata should be preserved.
    pub preserve_exif: bool,
    /// Gaussian blur sigma.
    ///
    /// When set, a Gaussian blur with the given sigma is applied after resizing
    /// and before encoding. Valid range is 0.1–100.0.
    pub blur: Option<f32>,
    /// Unsharp-mask (sharpen) sigma.
    ///
    /// When set, an unsharp mask with the given sigma is applied after resizing
    /// and before encoding. Valid range is 0.1–100.0. The sharpening threshold
    /// is fixed at 1.
    pub sharpen: Option<f32>,
    /// Whether the image should be desaturated to grayscale.
    ///
    /// When true, the image is converted to grayscale after resizing, blur, and
    /// sharpening, and before any watermark is composited, so a watermark keeps
    /// its own colors. Luminance is computed with the Rec. 601 weights the
    /// `image` crate uses, and the alpha channel is preserved.
    pub grayscale: bool,
    /// Whether a source smaller than the requested size may be scaled up.
    ///
    /// When true, the resize never enlarges: an image already within the requested
    /// bounds is left at its own size. This is a separate question from [`Fit`], which
    /// decides how the image is arranged relative to the box, so the two combine freely.
    /// `contain` still pads out to the full requested box; only the content inside it
    /// stops growing.
    pub without_enlargement: bool,
    /// Optional explicit crop region applied before resize.
    ///
    /// When set, the image is cropped to the specified rectangle before any resize
    /// operation. The crop region is validated at runtime against the decoded image
    /// dimensions.
    pub crop: Option<CropRegion>,
    /// Optional wall-clock deadline for the transform pipeline.
    ///
    /// When set, the transform checks elapsed time at each pipeline stage and returns
    /// [`TransformError::LimitExceeded`] if the deadline is exceeded. Adapters inject
    /// this value based on their operational requirements — for example, the HTTP server
    /// sets a 30-second deadline while the CLI leaves it as `None` (unlimited).
    ///
    /// The check happens between stages, not inside one. A stage that is already running
    /// runs to its end, so a transform can return after the deadline rather than at it,
    /// by however long its slowest single step takes. Encoding is the step where that
    /// matters, since no encoder truss calls can be interrupted part way.
    pub deadline: Option<Duration>,
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            fit: None,
            position: None,
            format: None,
            quality: None,
            optimize: OptimizeMode::None,
            target_quality: None,
            background: None,
            rotate: Rotation::DEG_0,
            auto_orient: true,
            strip_metadata: true,
            preserve_exif: false,
            blur: None,
            sharpen: None,
            grayscale: false,
            without_enlargement: false,
            crop: None,
            deadline: None,
        }
    }
}

impl TransformOptions {
    /// The first option set on this request that an SVG passthrough cannot honour.
    ///
    /// `fit`, `position`, and `withoutEnlargement` are absent from the list because the rules
    /// above already require a width or a height alongside each of them, so naming the axis
    /// covers those too and names the option the caller has to drop.
    fn svg_passthrough_unsupported_option(&self) -> Option<&'static str> {
        if self.width.is_some() {
            return Some("width");
        }
        if self.height.is_some() {
            return Some("height");
        }
        if !self.rotate.is_identity() {
            return Some("rotate");
        }
        if self.grayscale {
            return Some("grayscale");
        }
        if self.background.is_some() {
            return Some("background");
        }
        None
    }

    /// Checks every rule that the options decide between themselves, with no input.
    ///
    /// [`Self::normalize`] runs this first and then goes on to the rules that need the
    /// resolved output format, which for an absent `format` comes from the input. Callers
    /// that hold no input run this on its own: the HTTP server refuses a request here
    /// rather than after fetching the source, and `truss sign` refuses to put an option
    /// set into a signed URL that no server would serve. Keeping the two apart is what
    /// lets one list of rules answer for callers that have an image and callers that do
    /// not.
    pub(crate) fn validate_without_input(&self) -> Result<(), TransformError> {
        validate_dimension("width", self.width)?;
        validate_dimension("height", self.height)?;
        validate_quality(self.quality)?;
        validate_target_quality(self.target_quality)?;
        validate_blur(self.blur)?;
        validate_sharpen(self.sharpen)?;
        if let Some(crop) = self.crop
            && (crop.width == 0 || crop.height == 0)
        {
            return Err(TransformError::InvalidOptions(
                "crop width and height must be greater than zero".to_string(),
            ));
        }

        let has_bounded_resize = self.width.is_some() && self.height.is_some();

        if self.fit.is_some() && !has_bounded_resize {
            return Err(TransformError::InvalidOptions(
                "fit requires both width and height".to_string(),
            ));
        }

        if self.position.is_some() && !has_bounded_resize {
            return Err(TransformError::InvalidOptions(
                "position requires both width and height".to_string(),
            ));
        }

        // Unlike fit and position, this is meaningful with a single axis too, so it only
        // needs some resize to act on. Rejecting it outright beats silently ignoring a flag
        // the caller clearly meant to have an effect.
        if self.without_enlargement && self.width.is_none() && self.height.is_none() {
            return Err(TransformError::InvalidOptions(
                "withoutEnlargement requires width or height".to_string(),
            ));
        }

        if self.preserve_exif && self.strip_metadata {
            return Err(TransformError::InvalidOptions(
                "preserveExif requires stripMetadata to be false".to_string(),
            ));
        }

        Ok(())
    }

    /// Normalizes and validates the options against the input media type.
    ///
    /// # Errors
    ///
    /// Returns [`TransformError::InvalidOptions`] when the options contradict each other
    /// or the output format they resolve to.
    pub fn normalize(
        self,
        input_media_type: MediaType,
    ) -> Result<NormalizedTransformOptions, TransformError> {
        self.validate_without_input()?;

        let has_bounded_resize = self.width.is_some() && self.height.is_some();

        // An explicit `format: Some(Gif)` is a different request from an absent one, and is
        // rejected by `codecs::transform` rather than quietly rewritten here.
        let format = self
            .format
            .unwrap_or_else(|| input_media_type.default_output());
        let optimize = self.optimize;

        if optimize != OptimizeMode::None && !format.supports_optimization() {
            return Err(TransformError::InvalidOptions(format!(
                "optimization is not supported for {} output",
                format.as_name()
            )));
        }

        if optimize == OptimizeMode::Lossy && !format.supports_lossy_optimization() {
            return Err(TransformError::InvalidOptions(format!(
                "lossy optimization requires jpeg, webp, or avif output, got {}",
                format.as_name()
            )));
        }

        if self.preserve_exif && format == MediaType::Svg {
            return Err(TransformError::InvalidOptions(
                "preserveExif is not supported with SVG output".to_string(),
            ));
        }

        // SVG in and SVG out is a sanitize-only passthrough: the document is returned as its
        // author wrote it, so an option that asks for a different picture cannot be honoured.
        // Refusing it is what the rule above already does, and what `transform_svg` does for
        // blur, sharpen, crop, and watermark; the alternative is a caller who asked for a
        // 64-pixel icon getting the original back with exit code 0.
        if input_media_type == MediaType::Svg
            && format == MediaType::Svg
            && let Some(option) = self.svg_passthrough_unsupported_option()
        {
            return Err(TransformError::InvalidOptions(format!(
                "{option} is not supported with SVG output; choose a raster output format such as png"
            )));
        }

        if self.quality.is_some() && !format.is_lossy() {
            return Err(TransformError::InvalidOptions(
                "quality requires a lossy output format".to_string(),
            ));
        }

        if self.quality.is_some() && optimize == OptimizeMode::Lossless {
            return Err(TransformError::InvalidOptions(
                "quality cannot be combined with optimize=lossless".to_string(),
            ));
        }

        if self.target_quality.is_some()
            && matches!(optimize, OptimizeMode::None | OptimizeMode::Lossless)
        {
            return Err(TransformError::InvalidOptions(
                "targetQuality requires optimize=auto or optimize=lossy".to_string(),
            ));
        }

        if self.target_quality.is_some() && !format.supports_lossy_optimization() {
            return Err(TransformError::InvalidOptions(
                "targetQuality requires jpeg, webp, or avif output".to_string(),
            ));
        }

        let fit = if has_bounded_resize {
            Some(self.fit.unwrap_or(Fit::Contain))
        } else {
            None
        };

        Ok(NormalizedTransformOptions {
            width: self.width,
            height: self.height,
            fit,
            position: self.position.unwrap_or(Position::Center),
            format,
            quality: self.quality,
            optimize,
            target_quality: self.target_quality,
            background: self.background,
            rotate: self.rotate,
            auto_orient: self.auto_orient,
            metadata_policy: normalize_metadata_policy(
                self.strip_metadata,
                self.preserve_exif,
                optimize,
                format,
            ),
            blur: self.blur,
            sharpen: self.sharpen,
            grayscale: self.grayscale,
            without_enlargement: self.without_enlargement,
            crop: self.crop,
            deadline: self.deadline,
        })
    }
}

/// Fully normalized transform options ready for a backend pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedTransformOptions {
    /// The desired output width in pixels.
    pub width: Option<u32>,
    /// The desired output height in pixels.
    pub height: Option<u32>,
    /// The normalized resize fit mode.
    pub fit: Option<Fit>,
    /// The normalized positioning mode.
    pub position: Position,
    /// The resolved output format.
    pub format: MediaType,
    /// The requested lossy quality.
    pub quality: Option<u8>,
    /// The normalized optimization mode.
    pub optimize: OptimizeMode,
    /// Optional perceptual target used by lossy optimization.
    pub target_quality: Option<TargetQuality>,
    /// The requested background color.
    pub background: Option<Rgba8>,
    /// The requested extra rotation.
    pub rotate: Rotation,
    /// Whether EXIF-based auto-orientation should run.
    pub auto_orient: bool,
    /// The normalized metadata handling strategy.
    pub metadata_policy: MetadataPolicy,
    /// Gaussian blur sigma, when requested.
    pub blur: Option<f32>,
    /// Unsharp-mask (sharpen) sigma, when requested.
    pub sharpen: Option<f32>,
    /// Whether the image should be desaturated to grayscale.
    pub grayscale: bool,
    /// Whether a source smaller than the requested size may be scaled up.
    pub without_enlargement: bool,
    /// Optional explicit crop region applied before resize.
    pub crop: Option<CropRegion>,
    /// Optional wall-clock deadline for the transform pipeline.
    pub deadline: Option<Duration>,
}

/// Resize behavior for bounded transforms.
///
/// # Examples
///
/// ```
/// use truss::Fit;
/// use std::str::FromStr;
///
/// let fit = Fit::from_str("cover").unwrap();
/// assert_eq!(fit, Fit::Cover);
/// assert_eq!(fit.as_name(), "cover");
///
/// assert!(Fit::from_str("unknown").is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Scale to fit inside the box, preserving aspect ratio, then pad to the exact box.
    ///
    /// The output is always the requested width and height. When the aspect ratios differ,
    /// the remaining area is filled with the requested background.
    Contain,
    /// Scale to cover the box, preserving aspect ratio, then crop to the exact box.
    Cover,
    /// Stretch each axis to the box independently, without preserving aspect ratio.
    Fill,
    /// Scale to fit inside the box, preserving aspect ratio, and add no padding.
    ///
    /// The output is at most the requested width and height, and is usually smaller on one
    /// axis: a 640x427 source in a 200x200 box becomes 200x133. This is the difference from
    /// [`Fit::Contain`], which pads that same result out to 200x200.
    ///
    /// Whether a smaller source may be scaled up is not part of this mode. That is
    /// [`TransformOptions::without_enlargement`], which applies to every fit.
    Inside,
}

impl Fit {
    /// Returns the canonical option name used by the API and CLI.
    #[must_use]
    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Contain => "contain",
            Self::Cover => "cover",
            Self::Fill => "fill",
            Self::Inside => "inside",
        }
    }
}

impl FromStr for Fit {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "contain" => Ok(Self::Contain),
            "cover" => Ok(Self::Cover),
            "fill" => Ok(Self::Fill),
            "inside" => Ok(Self::Inside),
            _ => Err(format!("unsupported fit mode `{value}`")),
        }
    }
}

/// Positioning behavior for bounded transforms.
///
/// # Examples
///
/// ```
/// use truss::Position;
/// use std::str::FromStr;
///
/// let pos = Position::from_str("bottom-right").unwrap();
/// assert_eq!(pos, Position::BottomRight);
/// assert_eq!(pos.as_name(), "bottom-right");
///
/// assert!(Position::from_str("middle").is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// Center alignment.
    Center,
    /// Top alignment.
    Top,
    /// Right alignment.
    Right,
    /// Bottom alignment.
    Bottom,
    /// Left alignment.
    Left,
    /// Top-left alignment.
    TopLeft,
    /// Top-right alignment.
    TopRight,
    /// Bottom-left alignment.
    BottomLeft,
    /// Bottom-right alignment.
    BottomRight,
}

impl Position {
    /// Returns the canonical option name used by the API and CLI.
    #[must_use]
    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }
}

impl FromStr for Position {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "center" => Ok(Self::Center),
            "top" => Ok(Self::Top),
            "right" => Ok(Self::Right),
            "bottom" => Ok(Self::Bottom),
            "left" => Ok(Self::Left),
            "top-left" => Ok(Self::TopLeft),
            "top-right" => Ok(Self::TopRight),
            "bottom-left" => Ok(Self::BottomLeft),
            "bottom-right" => Ok(Self::BottomRight),
            _ => Err(format!("unsupported position `{value}`")),
        }
    }
}

/// Clockwise rotation in whole degrees, applied after auto-orientation.
///
/// Any integer is accepted and normalized into `0..360`, so a negative angle turns
/// counter-clockwise and a value past a full turn wraps: `-90` and `270` are the same
/// rotation, and so are `370` and `10`.
///
/// Degrees are whole numbers on purpose. The value appears verbatim in the cache key and
/// in the signed-URL canonical string, and a fractional angle would have to round-trip
/// bit-identically through Rust's and JavaScript's float formatting for a signature to
/// verify. Whole degrees sidestep that entirely, and no real caller asks for a fraction of
/// one.
///
/// # Examples
///
/// ```
/// use truss::Rotation;
/// use std::str::FromStr;
///
/// let rot = Rotation::from_str("270").unwrap();
/// assert_eq!(rot.as_degrees(), 270);
///
/// // Negative turns counter-clockwise, and wraps to the same rotation.
/// assert_eq!(Rotation::from_str("-90").unwrap(), rot);
/// // Angles past a full turn wrap too.
/// assert_eq!(Rotation::from_str("630").unwrap(), rot);
///
/// // Any whole angle is allowed, not just quarter turns.
/// assert_eq!(Rotation::from_str("45").unwrap().as_degrees(), 45);
/// assert!(Rotation::from_str("45.5").is_err());
///
/// assert!(Rotation::DEG_0.is_identity());
/// assert_eq!(Rotation::DEG_90.quarter_turns(), Some(1));
/// assert_eq!(Rotation::from_degrees(45).quarter_turns(), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rotation(u16);

impl Rotation {
    /// No rotation.
    pub const DEG_0: Self = Self(0);
    /// A quarter turn clockwise.
    pub const DEG_90: Self = Self(90);
    /// A half turn.
    pub const DEG_180: Self = Self(180);
    /// Three quarter turns clockwise.
    pub const DEG_270: Self = Self(270);

    /// Builds a rotation from any whole number of degrees, normalizing into `0..360`.
    ///
    /// Positive turns clockwise and negative turns counter-clockwise, which is the
    /// convention `--rotate` has always used and the one image tools generally agree on.
    #[must_use]
    pub const fn from_degrees(degrees: i32) -> Self {
        let wrapped = degrees % 360;
        let normalized = if wrapped < 0 { wrapped + 360 } else { wrapped };
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Self(normalized as u16)
    }

    /// Returns the normalized degree value used by the API, the CLI, and the cache key.
    #[must_use]
    pub const fn as_degrees(self) -> u16 {
        self.0
    }

    /// Returns `true` when the rotation leaves the image untouched.
    #[must_use]
    pub const fn is_identity(self) -> bool {
        self.0 == 0
    }

    /// Returns the number of clockwise quarter turns when the angle is a multiple of 90.
    ///
    /// A quarter turn only permutes pixels, so the pipeline keeps that exact path instead
    /// of resampling. Anything else returns `None` and goes through the general rotation.
    #[must_use]
    pub const fn quarter_turns(self) -> Option<u8> {
        if self.0.is_multiple_of(90) {
            #[allow(clippy::cast_possible_truncation)]
            Some((self.0 / 90) as u8)
        } else {
            None
        }
    }
}

impl fmt::Display for Rotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Rotation {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Parsed wide and reduced before it is narrowed. An angle past a full turn wraps,
        // which is what this type documents, so how many turns it is past does not change
        // the answer; parsing straight into `i32` made a large multiple of 360 report that
        // it was not a whole number, which it is.
        match value.parse::<i64>() {
            Ok(degrees) => Ok(Self::from_degrees((degrees % 360) as i32)),
            Err(_) => Err(format!(
                "unsupported rotation `{value}`: expected a whole number of degrees"
            )),
        }
    }
}

/// A simple 8-bit RGBA color.
///
/// # Examples
///
/// ```
/// use truss::Rgba8;
///
/// // Parse a 6-digit hex color (fully opaque)
/// let red = Rgba8::from_hex("ff0000").unwrap();
/// assert_eq!(red, Rgba8 { r: 255, g: 0, b: 0, a: 255 });
///
/// // Parse an 8-digit hex color with alpha
/// let semi = Rgba8::from_hex("00ff0080").unwrap();
/// assert_eq!(semi, Rgba8 { r: 0, g: 255, b: 0, a: 128 });
///
/// // Invalid input is rejected
/// assert!(Rgba8::from_hex("xyz").is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba8 {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
    /// Alpha channel.
    pub a: u8,
}

impl Rgba8 {
    /// Parses a hexadecimal RGB or RGBA color string without a leading `#`.
    ///
    /// Every rejection reads the same sentence, which names the shape truss accepts rather
    /// than repeating the value back. `#ffffff` is the spelling a caller reaches for first,
    /// since CSS, HTML, and every colour picker use it, and `unsupported color \`#ffffff\``
    /// gave them nothing to correct. Naming the rule is what every other option here does.
    pub fn from_hex(value: &str) -> Result<Self, String> {
        fn rule(value: &str) -> String {
            format!(
                "unsupported color `{value}`: a color is six or eight hexadecimal digits with no leading `#`, as in ffffff or ffffffaa"
            )
        }

        if !value.is_ascii() || (value.len() != 6 && value.len() != 8) {
            return Err(rule(value));
        }

        let r = u8::from_str_radix(&value[0..2], 16).map_err(|_| rule(value))?;
        let g = u8::from_str_radix(&value[2..4], 16).map_err(|_| rule(value))?;
        let b = u8::from_str_radix(&value[4..6], 16).map_err(|_| rule(value))?;
        let a = if value.len() == 8 {
            u8::from_str_radix(&value[6..8], 16).map_err(|_| rule(value))?
        } else {
            u8::MAX
        };

        Ok(Self { r, g, b, a })
    }
}

/// Metadata handling after option normalization.
///
/// # Examples
///
/// ```
/// use truss::{TransformOptions, MediaType};
///
/// // Default options normalize to StripAll
/// let opts = TransformOptions::default();
/// let normalized = opts.normalize(MediaType::Png).unwrap();
/// assert_eq!(normalized.metadata_policy, truss::MetadataPolicy::StripAll);
///
/// // Disabling strip_metadata normalizes to KeepAll
/// let opts = TransformOptions { strip_metadata: false, ..TransformOptions::default() };
/// let normalized = opts.normalize(MediaType::Png).unwrap();
/// assert_eq!(normalized.metadata_policy, truss::MetadataPolicy::KeepAll);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataPolicy {
    /// Drop metadata from the output.
    StripAll,
    /// Keep metadata unchanged when possible.
    KeepAll,
    /// Preserve ICC profiles while stripping EXIF and other metadata.
    PreserveIcc,
    /// Preserve EXIF while allowing other metadata policies later.
    PreserveExif,
}

/// Resolves the three-way metadata flag semantics shared by all adapters.
///
/// Adapters accept different flag names (CLI: `--keep-metadata`/`--strip-metadata`/`--preserve-exif`,
/// WASM: `keepMetadata`/`preserveExif`, server: `stripMetadata`/`preserveExif`) but the
/// underlying semantics are identical. This function centralizes the resolution so that
/// every adapter produces the same `(strip_metadata, preserve_exif)` pair for the same
/// logical input.
///
/// # Arguments
///
/// * `strip` — Explicit "strip all metadata" flag, when provided.
/// * `keep` — Explicit "keep all metadata" flag, when provided.
/// * `preserve_exif` — Explicit "preserve EXIF only" flag, when provided.
///
/// # Errors
///
/// Returns [`TransformError::InvalidOptions`] when `keep` and `preserve_exif` are both
/// explicitly `true`, since those policies are mutually exclusive.
///
/// # Examples
///
/// ```
/// use truss::resolve_metadata_flags;
///
/// // Default: strip all metadata
/// let (strip, exif) = resolve_metadata_flags(None, None, None).unwrap();
/// assert!(strip);
/// assert!(!exif);
///
/// // Explicit keep
/// let (strip, exif) = resolve_metadata_flags(None, Some(true), None).unwrap();
/// assert!(!strip);
/// assert!(!exif);
///
/// // Preserve EXIF only
/// let (strip, exif) = resolve_metadata_flags(None, None, Some(true)).unwrap();
/// assert!(!strip);
/// assert!(exif);
///
/// // keep + preserve_exif conflict
/// assert!(resolve_metadata_flags(None, Some(true), Some(true)).is_err());
/// ```
pub fn resolve_metadata_flags(
    strip: Option<bool>,
    keep: Option<bool>,
    preserve_exif: Option<bool>,
) -> Result<(bool, bool), TransformError> {
    let keep = keep.unwrap_or(false);
    let preserve_exif = preserve_exif.unwrap_or(false);

    if keep && preserve_exif {
        return Err(TransformError::InvalidOptions(
            "keepMetadata and preserveExif cannot both be true".to_string(),
        ));
    }

    let strip_metadata = if keep || preserve_exif {
        false
    } else {
        strip.unwrap_or(true)
    };

    Ok((strip_metadata, preserve_exif))
}

/// Errors returned by Core validation or backend execution.
///
/// # Examples
///
/// ```
/// use truss::TransformError;
///
/// let err = TransformError::InvalidOptions("quality must be between 1 and 100".into());
/// assert_eq!(
///     format!("{err}"),
///     "invalid transform options: quality must be between 1 and 100"
/// );
///
/// // TransformError implements std::error::Error
/// let _: &dyn std::error::Error = &err;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TransformError {
    /// The input artifact is structurally invalid.
    InvalidInput(String),
    /// The provided options are contradictory or unsupported.
    InvalidOptions(String),
    /// The input media type cannot be processed.
    UnsupportedInputMediaType(String),
    /// The requested output media type cannot be produced.
    UnsupportedOutputMediaType(MediaType),
    /// Decoding the input artifact failed.
    DecodeFailed(String),
    /// Encoding the output artifact failed.
    EncodeFailed(String),
    /// The current runtime does not provide a required capability.
    CapabilityMissing(String),
    /// The image exceeds a processing limit such as maximum pixel count.
    LimitExceeded(String),
}

/// Folds a message onto a single trimmed line.
///
/// Every adapter presents a failure as one line of text: the CLI writes
/// `error: <message> (<class>)`, the HTTP server puts the message in an RFC 9457 `detail`,
/// and `@nao1215/truss-wasm` hands it to a browser. Some of those messages come from a
/// decoder in a dependency, whose wording truss does not choose and which may end with a
/// newline or hold one in the middle, so the line break is taken out where the message is
/// rendered rather than at each of the thirty places one can enter from.
///
/// A message that is already one trimmed line is returned untouched. `server::cache` does
/// the same to a warning before it goes on an entry's header line, for the same reason.
///
/// A build with neither the server nor the Wasm adapter, which is the library on its own,
/// renders no message and does not compile this.
#[cfg(any(feature = "server", feature = "wasm"))]
pub(crate) fn single_line(message: &str) -> std::borrow::Cow<'_, str> {
    let trimmed = message.trim();
    if !trimmed.contains(breaks_a_line) {
        return std::borrow::Cow::Borrowed(trimmed);
    }
    std::borrow::Cow::Owned(trimmed.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Reports whether a character would move a message off its line.
///
/// A space is what the message already reads as between two words, so only the ones that
/// end the line or move the cursor count.
#[cfg(any(feature = "server", feature = "wasm"))]
fn breaks_a_line(c: char) -> bool {
    (c.is_whitespace() && c != ' ') || c.is_control()
}

impl fmt::Display for TransformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(reason) => write!(f, "invalid input: {reason}"),
            Self::InvalidOptions(reason) => write!(f, "invalid transform options: {reason}"),
            Self::UnsupportedInputMediaType(reason) => {
                write!(f, "unsupported input media type: {reason}")
            }
            // Naming only the media type left the reader to guess why: `svg` is
            // refused for a raster input yet accepted for an SVG one, and `gif` is
            // refused for every input. Say which rule was hit and what to ask for
            // instead, so the CLI, the server, and the WASM build all explain it the
            // same way.
            Self::UnsupportedOutputMediaType(media_type) => match media_type {
                MediaType::Svg => write!(
                    f,
                    "svg output requires an svg input; choose a raster output format such as png, jpeg, webp, or avif"
                ),
                MediaType::Gif => write!(
                    f,
                    "gif is an input-only format; choose an output format such as png, jpeg, webp, or avif"
                ),
                other => write!(f, "unsupported output media type: {other}"),
            },
            Self::DecodeFailed(reason) => write!(f, "decode failed: {reason}"),
            Self::EncodeFailed(reason) => write!(f, "encode failed: {reason}"),
            Self::CapabilityMissing(reason) => write!(f, "missing capability: {reason}"),
            Self::LimitExceeded(reason) => write!(f, "limit exceeded: {reason}"),
        }
    }
}

impl Error for TransformError {}

/// Categories of image metadata that may be present in an artifact.
///
/// Used by [`TransformWarning::MetadataDropped`] to identify which metadata type
/// was silently dropped during a transform operation.
///
/// ```
/// use truss::MetadataKind;
///
/// assert_eq!(format!("{}", MetadataKind::Xmp), "XMP");
/// assert_eq!(format!("{}", MetadataKind::Iptc), "IPTC");
/// assert_eq!(format!("{}", MetadataKind::Exif), "EXIF");
/// assert_eq!(format!("{}", MetadataKind::Icc), "ICC profile");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetadataKind {
    /// XMP (Extensible Metadata Platform) metadata.
    Xmp,
    /// IPTC/IIM (International Press Telecommunications Council) metadata.
    Iptc,
    /// EXIF (Exchangeable Image File Format) metadata.
    Exif,
    /// ICC color profile.
    Icc,
}

impl fmt::Display for MetadataKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xmp => f.write_str("XMP"),
            Self::Iptc => f.write_str("IPTC"),
            Self::Exif => f.write_str("EXIF"),
            Self::Icc => f.write_str("ICC profile"),
        }
    }
}

/// A non-fatal warning emitted during a transform operation.
///
/// Warnings indicate that the transform completed successfully but some aspect of
/// the request could not be fully honored. Adapters should surface these to operators
/// (e.g. CLI prints to stderr, server logs to stderr).
///
/// ```
/// use truss::{MetadataKind, TransformWarning};
///
/// let warning = TransformWarning::MetadataDropped(MetadataKind::Xmp);
/// assert_eq!(
///     format!("{warning}"),
///     "XMP metadata was present in the input but could not be preserved by the output encoder"
/// );
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TransformWarning {
    /// Metadata of the given kind was present in the input but could not be preserved
    /// by the output encoder and was silently dropped.
    MetadataDropped(MetadataKind),
    /// The input carries an EXIF orientation that the output records neither in its pixels
    /// nor in its metadata, so the output displays rotated relative to the input.
    OrientationDropped {
        /// The EXIF orientation value the input carried.
        orientation: u16,
    },
    /// No quality the search probed reached the requested target, so the output scores below
    /// it. Raised only for a target the caller named, never for the one `auto` picks on its
    /// own, and never when the input's own bytes were handed back. The search samples the
    /// quality range rather than walking it, so this says no probed quality reached the
    /// target rather than that none would; see [`TargetQuality`].
    TargetQualityNotReached {
        /// The target that was asked for.
        target: TargetQuality,
        /// The score the returned encode reached, which is the one at `quality`.
        achieved: f32,
        /// The quality of the encode returned: the `quality` cap when one was given,
        /// otherwise 100.
        quality: u8,
    },
}

impl fmt::Display for TransformWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MetadataDropped(kind) => write!(
                f,
                "{kind} metadata was present in the input but could not be preserved by the output encoder"
            ),
            Self::OrientationDropped { orientation } => write!(
                f,
                "the input carries EXIF orientation {orientation}; with autoOrient off and the metadata stripped the output records it neither in its pixels nor in its metadata, so it displays rotated. Keep the metadata to preserve the tag, or leave autoOrient on to apply it to the pixels"
            ),
            Self::TargetQualityNotReached {
                target,
                achieved,
                quality,
            } => {
                let metric = target.metric.as_name();
                if *quality < 100 {
                    write!(
                        f,
                        "the lossy encode did not reach {target} within the quality cap of {quality}: at that quality it reached {metric} {achieved:.3}. Raise the cap or lower the target"
                    )
                } else {
                    write!(
                        f,
                        "the lossy encode did not reach {target} at quality 100, where it reached {metric} {achieved:.3}. The quality range is sampled rather than scanned, so a setting the search did not try may still reach it; lower the target for one it will find"
                    )
                }
            }
        }
    }
}

/// The result of a successful transform, containing the output artifact and any warnings.
///
/// Warnings indicate aspects of the request that could not be fully honored, such as
/// metadata types that were silently dropped because the output encoder does not support them.
#[derive(Debug)]
#[must_use]
pub struct TransformResult {
    /// The transformed output artifact.
    pub artifact: Artifact,
    /// Non-fatal warnings emitted during the transform.
    pub warnings: Vec<TransformWarning>,
}

/// Inspects raw bytes, detects the media type, and extracts best-effort metadata.
///
/// The caller is expected to pass bytes that have already been resolved by an adapter
/// such as the CLI or HTTP server runtime. If a declared media type is provided in the
/// [`RawArtifact`], this function verifies that the declared type matches the detected
/// signature before returning the classified [`Artifact`].
///
/// Detection currently supports JPEG, PNG, WebP, AVIF, and BMP recognition.
/// Width, height, and alpha extraction are best-effort and depend on the underlying format
/// and any container metadata the file exposes.
///
/// # Errors
///
/// Returns [`TransformError::UnsupportedInputMediaType`] when the byte signature does not
/// match a supported format, [`TransformError::InvalidInput`] when the declared media type
/// conflicts with the detected type, and [`TransformError::DecodeFailed`] when a supported
/// format has an invalid or truncated structure.
///
/// # Examples
///
/// ```
/// use truss::{sniff_artifact, MediaType, RawArtifact};
///
/// let png_bytes = vec![
///     0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n',
///     0, 0, 0, 13, b'I', b'H', b'D', b'R',
///     0, 0, 0, 4, 0, 0, 0, 3, 8, 6, 0, 0, 0,
///     0, 0, 0, 0,
/// ];
///
/// let artifact = sniff_artifact(RawArtifact::new(png_bytes, Some(MediaType::Png))).unwrap();
///
/// assert_eq!(artifact.media_type, MediaType::Png);
/// assert_eq!(artifact.metadata.width, Some(4));
/// assert_eq!(artifact.metadata.height, Some(3));
/// ```
///
/// ```ignore
/// use image::codecs::avif::AvifEncoder;
/// use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
/// use truss::{sniff_artifact, MediaType, RawArtifact};
///
/// let image = RgbaImage::from_pixel(3, 2, Rgba([10, 20, 30, 0]));
/// let mut bytes = Vec::new();
/// AvifEncoder::new(&mut bytes)
///     .write_image(&image, 3, 2, ColorType::Rgba8.into())
///     .unwrap();
///
/// let artifact = sniff_artifact(RawArtifact::new(bytes, Some(MediaType::Avif))).unwrap();
///
/// assert_eq!(artifact.media_type, MediaType::Avif);
/// assert_eq!(artifact.metadata.width, Some(3));
/// assert_eq!(artifact.metadata.height, Some(2));
/// assert_eq!(artifact.metadata.has_alpha, Some(true));
/// ```
#[must_use = "this function returns the detected artifact without side effects"]
pub fn sniff_artifact(input: RawArtifact) -> Result<Artifact, TransformError> {
    let (media_type, metadata) = detect_artifact(&input.bytes)?;

    if let Some(declared_media_type) = input.declared_media_type
        && declared_media_type != media_type
    {
        return Err(TransformError::InvalidInput(
            "declared media type does not match detected media type".to_string(),
        ));
    }

    Ok(Artifact::new(input.bytes, media_type, metadata))
}

fn validate_dimension(name: &str, value: Option<u32>) -> Result<(), TransformError> {
    if matches!(value, Some(0)) {
        return Err(TransformError::InvalidOptions(format!(
            "{name} must be greater than zero"
        )));
    }

    Ok(())
}

fn validate_quality(value: Option<u8>) -> Result<(), TransformError> {
    match value {
        Some(value) => validate_quality_value(i64::from(value))
            .map(|_| ())
            .map_err(|message| TransformError::InvalidOptions(message.to_string())),
        None => Ok(()),
    }
}

/// The range a quality has to be in, checked against a number of any width.
///
/// A caller types a number, not a `u8`, and refusing 256 with the span of the integer it
/// would be stored in tells them a limit that is not truss's: `--quality 255` was answered
/// `quality must be between 1 and 100` while `--quality 256` was answered
/// `256 is not in 0..=255`, and the server said `expected u8`. Every adapter parses wide and
/// asks this, so one option has one limit and one sentence.
pub(crate) fn validate_quality_value(value: i64) -> Result<u8, &'static str> {
    match value {
        1..=100 => Ok(value as u8),
        _ => Err("quality must be between 1 and 100"),
    }
}

fn validate_target_quality(value: Option<TargetQuality>) -> Result<(), TransformError> {
    let Some(value) = value else {
        return Ok(());
    };

    if !value.value.is_finite() {
        return Err(TransformError::InvalidOptions(
            "targetQuality must be finite".to_string(),
        ));
    }

    match value.metric {
        QualityMetric::Ssim if !(0.0..=1.0).contains(&value.value) || value.value == 0.0 => {
            Err(TransformError::InvalidOptions(
                "ssim targetQuality must be greater than 0.0 and at most 1.0".to_string(),
            ))
        }
        QualityMetric::Psnr if value.value <= 0.0 => Err(TransformError::InvalidOptions(
            "psnr targetQuality must be greater than 0".to_string(),
        )),
        _ => Ok(()),
    }
}

fn validate_blur(value: Option<f32>) -> Result<(), TransformError> {
    if let Some(sigma) = value
        && !(0.1..=100.0).contains(&sigma)
    {
        return Err(TransformError::InvalidOptions(
            "blur sigma must be between 0.1 and 100.0".to_string(),
        ));
    }

    Ok(())
}

fn validate_sharpen(value: Option<f32>) -> Result<(), TransformError> {
    if let Some(sigma) = value
        && !(0.1..=100.0).contains(&sigma)
    {
        return Err(TransformError::InvalidOptions(
            "sharpen sigma must be between 0.1 and 100.0".to_string(),
        ));
    }

    Ok(())
}

/// Reads a quality of any width and judges it by the range truss publishes.
///
/// Deserializing straight into the `u8` the field holds makes `serde` refuse 256 with
/// `invalid value: integer 256, expected u8`, which names a Rust type and a limit that is
/// not truss's. Both the HTTP payload and the Wasm options object read this, so the two
/// answer the same way.
pub(crate) fn deserialize_quality<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // A value the field can hold is handed on for `normalize` to judge, so 101 reads the
    // same sentence from the same place on every adapter. Only a value too large to be
    // held is refused here, with the sentence that check would have given.
    deserialize_ranged(deserializer, |value| match u8::try_from(value) {
        Ok(quality) => Ok(quality),
        Err(_) => {
            Err(validate_quality_value(value).expect_err("a value outside u8 is outside 1..=100"))
        }
    })
}

/// Reads a width of any width and judges it by [`validate_width_value`].
pub(crate) fn deserialize_width<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_ranged(deserializer, validate_width_value)
}

/// Reads a height of any width and judges it by [`validate_height_value`].
pub(crate) fn deserialize_height<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_ranged(deserializer, validate_height_value)
}

fn deserialize_ranged<'de, D, T>(
    deserializer: D,
    validate: impl FnOnce(i64) -> Result<T, &'static str>,
) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    use serde::de::Error as _;
    match Option::<i64>::deserialize(deserializer)? {
        None => Ok(None),
        Some(value) => validate(value).map(Some).map_err(D::Error::custom),
    }
}

/// Reads a rotation of any width and reduces it to a single turn.
///
/// The option documents that an angle past a full turn wraps, and the CLI takes any whole
/// number of degrees; deserializing into `i32` made the two adapters that do it refuse what
/// the CLI accepts.
pub(crate) fn deserialize_rotation_degrees<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    Ok(Option::<i64>::deserialize(deserializer)?.map(|degrees| (degrees % 360) as i32))
}

/// The rules a width has to satisfy before any image has been read.
///
/// A number that fits is handed back so that the rules needing the image report it where
/// they always did, with the class they always had: zero is `width must be greater than
/// zero` from `normalize`, and a size past `MAX_OUTPUT_PIXELS` is the pixel count the
/// transform reports. Only a number that cannot be a count of pixels at all is refused
/// here, and it says which of the two things is wrong rather than naming the integer it
/// would have been stored in.
pub(crate) fn validate_width_value(value: i64) -> Result<u32, &'static str> {
    dimension_value(
        value,
        "width must be greater than zero",
        "width is too large to be a number of pixels",
    )
}

/// The same for a height. See [`validate_width_value`].
pub(crate) fn validate_height_value(value: i64) -> Result<u32, &'static str> {
    dimension_value(
        value,
        "height must be greater than zero",
        "height is too large to be a number of pixels",
    )
}

/// The rules a watermark margin has to satisfy before any image has been read.
///
/// Zero is a margin, so only a negative number and one too large to be a count of pixels are
/// refused here; a margin that leaves the watermark no room is reported by the pipeline,
/// which names the sizes involved.
#[cfg(any(feature = "cli", feature = "server"))]
pub(crate) fn validate_watermark_margin_value(value: i64) -> Result<u32, &'static str> {
    dimension_value(
        value,
        "watermark margin must not be negative",
        "watermark margin is too large to be a number of pixels",
    )
}

fn dimension_value(
    value: i64,
    not_positive: &'static str,
    too_large: &'static str,
) -> Result<u32, &'static str> {
    match u32::try_from(value) {
        Ok(pixels) => Ok(pixels),
        Err(_) if value <= 0 => Err(not_positive),
        Err(_) => Err(too_large),
    }
}

/// The range a watermark opacity has to be in, checked against a number of any width.
///
/// The sibling of [`validate_quality_value`], and there for the same reason: 256 is not a
/// `u8`, and saying so names an integer type rather than the range truss publishes. Only
/// the two adapters that parse a caller's text need it, so it is gated the way they are.
#[cfg(any(feature = "cli", feature = "server"))]
pub(crate) fn validate_watermark_opacity_value(value: i64) -> Result<u8, &'static str> {
    match value {
        1..=100 => Ok(value as u8),
        _ => Err("watermark opacity must be between 1 and 100"),
    }
}

/// Checks the watermark opacity, which every adapter reads before it has an image.
///
/// The CLI checks it while parsing a flag, the HTTP server before fetching the watermark
/// URL, and the Wasm package while reading its options object, so the rule is needed in
/// four places and the message has to be the same in all of them.
///
/// Returns the message to report, so each adapter keeps its own error type and its own
/// failure class while the sentence the caller reads is written once.
pub(crate) fn validate_watermark_opacity(opacity: u8) -> Result<(), &'static str> {
    if opacity == 0 || opacity > 100 {
        return Err("watermark opacity must be between 1 and 100");
    }

    Ok(())
}

fn validate_watermark(wm: &WatermarkInput) -> Result<(), TransformError> {
    validate_watermark_opacity(wm.opacity)
        .map_err(|message| TransformError::InvalidOptions(message.to_string()))?;

    if !wm.image.media_type.is_raster() {
        return Err(TransformError::InvalidOptions(
            "watermark image must be a raster format".to_string(),
        ));
    }

    Ok(())
}

/// Resolves the metadata flags into the policy the pipeline applies.
///
/// Re-encoding a profile-tagged image renders it in the wrong colors if the profile is
/// dropped, so a strip request is upgraded to "keep the ICC profile only" whenever an
/// optimization was asked for. The pixels surviving the encode does not change that: a
/// lossless optimization writes the same picture and the profile is what says how to read
/// it. `OptimizeMode::None` is left alone, since that is what a plain `truss convert` does
/// and stripping is what its flag says.
///
/// The upgrade is limited to formats that can actually carry a profile: turning it on for a
/// format that cannot made `--strip-metadata` fail with "cannot preserve metadata", which left
/// no flag combination that worked (<https://github.com/nao1215/truss/issues/279>).
fn normalize_metadata_policy(
    strip_metadata: bool,
    preserve_exif: bool,
    optimize: OptimizeMode,
    format: MediaType,
) -> MetadataPolicy {
    if preserve_exif {
        MetadataPolicy::PreserveExif
    } else if strip_metadata && optimize != OptimizeMode::None && format.supports_icc_profile() {
        MetadataPolicy::PreserveIcc
    } else if strip_metadata {
        MetadataPolicy::StripAll
    } else {
        MetadataPolicy::KeepAll
    }
}

fn detect_artifact(bytes: &[u8]) -> Result<(MediaType, ArtifactMetadata), TransformError> {
    if is_png(bytes) {
        return Ok((MediaType::Png, sniff_png(bytes)?));
    }

    if is_jpeg(bytes) {
        return Ok((MediaType::Jpeg, sniff_jpeg(bytes)?));
    }

    if is_webp(bytes) {
        return Ok((MediaType::Webp, sniff_webp(bytes)?));
    }

    if is_avif(bytes) {
        return Ok((MediaType::Avif, sniff_avif(bytes)?));
    }

    if is_bmp(bytes) {
        return Ok((MediaType::Bmp, sniff_bmp(bytes)?));
    }

    if is_tiff(bytes) {
        return Ok((MediaType::Tiff, sniff_tiff(bytes)?));
    }

    if is_gif(bytes) {
        return Ok((MediaType::Gif, sniff_gif(bytes)?));
    }

    // SVG check goes last: it relies on text scanning which is slower than binary
    // magic-number checks and could produce false positives on non-SVG XML.
    if is_svg(bytes) {
        return Ok((MediaType::Svg, sniff_svg(bytes)));
    }

    // The length and nothing else. This message used to carry the first sixteen bytes in
    // hexadecimal, which is a useful thing to say about a file the operator named and a
    // disclosure about a URL fetched on somebody's behalf: the CLI printed it for
    // `--url`, and the server returned it to the caller in the `detail` of its problem
    // body. The core cannot tell the two apart, so it says neither.
    Err(TransformError::UnsupportedInputMediaType(format!(
        "unknown file signature ({} bytes)",
        bytes.len()
    )))
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

fn is_avif(bytes: &[u8]) -> bool {
    bytes.len() >= 16 && &bytes[4..8] == b"ftyp" && has_avif_brand(&bytes[8..])
}

/// Detects SVG by consuming the XML prolog and checking that the root element is
/// `<svg`.
///
/// XML 1.0 defines the prolog as `XMLDecl? Misc* (doctypedecl Misc*)?` with
/// `Misc ::= Comment | PI | S`, so comments and processing instructions are legal
/// on either side of the doctype and in any number. Walking a fixed sequence
/// instead rejects documents real editors produce: Adobe Illustrator writes the
/// declaration, a generator comment, and then a doctype with an internal subset.
fn is_svg(bytes: &[u8]) -> bool {
    svg_root_element(bytes).is_some()
}

/// Returns the document text from the root element onwards, when that root is `<svg`.
///
/// The prolog walk is shared with [`sniff_svg`], which needs the root element's attributes
/// and would otherwise repeat it. Splitting the two apart is what keeps detection and
/// measurement from disagreeing about where the root begins.
fn svg_root_element(bytes: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(bytes).ok()?;

    // Skip UTF-8 BOM if present.
    let mut remaining = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut seen_doctype = false;

    loop {
        remaining = remaining.trim_start();

        if let Some(rest) = remaining.strip_prefix("<!--") {
            let end = rest.find("-->")?;
            remaining = &rest[end + 3..];
            continue;
        }

        // Any processing instruction, including the XML declaration, which is
        // just the one whose target is `xml`.
        if let Some(rest) = remaining.strip_prefix("<?") {
            let end = rest.find("?>")?;
            remaining = &rest[end + 2..];
            continue;
        }

        if !seen_doctype && let Some(rest) = remaining.strip_prefix("<!DOCTYPE") {
            let after = skip_doctype(rest)?;
            seen_doctype = true;
            remaining = after;
            continue;
        }

        break;
    }

    let is_root = remaining.starts_with("<svg")
        && remaining
            .as_bytes()
            .get(4)
            .is_some_and(|&b| b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'>');
    is_root.then_some(remaining)
}

/// Returns the text after a doctype declaration, or `None` when it is unterminated.
///
/// The terminating `>` is not simply the first one: an internal subset is
/// delimited by `[` and `]` and declares entities whose replacement text may
/// contain `>`, and a system identifier is a quoted string that may contain one
/// too.
fn skip_doctype(rest: &str) -> Option<&str> {
    let bytes = rest.as_bytes();
    let mut quote: Option<u8> = None;
    let mut in_subset = false;

    for (index, &byte) in bytes.iter().enumerate() {
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'[' => in_subset = true,
                b']' => in_subset = false,
                b'>' if !in_subset => return Some(&rest[index + 1..]),
                _ => {}
            },
        }
    }

    None
}

/// Extracts SVG metadata. SVGs inherently support transparency.
///
/// The dimensions come from the root element's `width` and `height`, falling back to the
/// `viewBox` extent, which is what SVG defines the intrinsic size to be. They stay unknown
/// when the document gives no absolute answer — a percentage with no `viewBox` to resolve
/// it against, a font-relative unit, nothing declared at all — because a viewport is needed
/// to resolve those and a file on disk has none.
fn sniff_svg(bytes: &[u8]) -> ArtifactMetadata {
    let size = svg_root_element(bytes).and_then(svg_intrinsic_size);
    ArtifactMetadata {
        width: size.map(|(width, _)| width),
        height: size.map(|(_, height)| height),
        frame_count: 1,
        duration: None,
        has_alpha: Some(true),
        orientation: None,
    }
}

/// The intrinsic size an SVG document declares, in pixels.
///
/// SVG resolves an intrinsic size from `width` and `height` when both are absolute lengths,
/// and from the `viewBox` extent otherwise; when one axis is absolute and the other is not,
/// the missing one follows from the `viewBox` aspect ratio. Anything that needs a viewport
/// or a font to resolve — a percentage, `em`, `ex` — has no answer here and returns `None`
/// rather than a guess, which is what the `Option` in `ArtifactMetadata` is for.
fn svg_intrinsic_size(root: &str) -> Option<(u32, u32)> {
    let tag = svg_root_tag(root)?;
    let width = root_attribute(tag, "width").and_then(svg_length_px);
    let height = root_attribute(tag, "height").and_then(svg_length_px);
    let view_box = root_attribute(tag, "viewBox").and_then(parse_view_box);

    let (width, height) = match (width, height, view_box) {
        (Some(width), Some(height), _) => (width, height),
        (Some(width), None, Some((box_width, box_height))) => {
            (width, width * box_height / box_width)
        }
        (None, Some(height), Some((box_width, box_height))) => {
            (height * box_width / box_height, height)
        }
        (None, None, Some(size)) => size,
        _ => return None,
    };

    Some((to_dimension(width)?, to_dimension(height)?))
}

/// Returns the text between `<` and the `>` that closes the root start tag.
///
/// The terminator is not simply the first `>`: an attribute value is a quoted string and may
/// contain one, which is the same trap [`skip_doctype`] works around.
fn svg_root_tag(root: &str) -> Option<&str> {
    let mut quote: Option<u8> = None;
    for (index, &byte) in root.as_bytes().iter().enumerate() {
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return Some(&root[1..index]),
                _ => {}
            },
        }
    }
    None
}

/// Returns the value of one attribute of a start tag, by exact name.
///
/// Matching on the whole name rather than searching for it as a substring is what keeps
/// `stroke-width` from answering for `width`.
fn root_attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let mut index = 0;

    // Step over the element name; attributes start after the first run of whitespace.
    while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
        index += 1;
    }

    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }

        let name_start = index;
        while index < bytes.len()
            && bytes[index] != b'='
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'/'
        {
            index += 1;
        }
        let attribute = &tag[name_start..index];
        if attribute.is_empty() {
            // Nothing was consumed, so skip the character to guarantee progress.
            index += 1;
            continue;
        }

        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let &quote = bytes.get(index)?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        index += 1;
        let value_start = index;
        while index < bytes.len() && bytes[index] != quote {
            index += 1;
        }
        if index >= bytes.len() {
            return None;
        }
        let value = &tag[value_start..index];
        index += 1;

        if attribute == name {
            return Some(value);
        }
    }
}

/// Converts a CSS length to pixels, for the absolute units only.
///
/// The relative units — `%`, `em`, `ex`, `ch`, `rem`, `vw`, `vh` — need a viewport or a font
/// to resolve and have no answer for a file read off disk, so they return `None` and let the
/// `viewBox` answer instead.
fn svg_length_px(value: &str) -> Option<f64> {
    let value = value.trim();
    let split = value
        .find(|c: char| !matches!(c, '0'..='9' | '.' | '+' | '-' | 'e' | 'E'))
        .unwrap_or(value.len());
    let number: f64 = value[..split].parse().ok()?;
    let scale = match value[split..].trim().to_ascii_lowercase().as_str() {
        "" | "px" => 1.0,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "q" => 96.0 / 101.6,
        _ => return None,
    };
    let pixels = number * scale;
    (pixels.is_finite() && pixels > 0.0).then_some(pixels)
}

/// Returns the width and height of a `viewBox`, which is its third and fourth numbers.
fn parse_view_box(value: &str) -> Option<(f64, f64)> {
    let numbers: Vec<f64> = value
        .split([' ', '\t', '\n', '\r', ','])
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [_, _, width, height] = numbers[..] else {
        return None;
    };
    (width.is_finite() && width > 0.0 && height.is_finite() && height > 0.0)
        .then_some((width, height))
}

/// Truncates a resolved length to the pixel count a caller can act on.
///
/// Truncation rather than rounding is deliberate: `usvg` truncates when it turns the same
/// document into a render size, and a reported dimension that disagrees with the one a
/// conversion produces is the drift issue #322 closed for EXIF orientation.
fn to_dimension(value: f64) -> Option<u32> {
    if !(value.is_finite() && value >= 1.0 && value < f64::from(u32::MAX)) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(value as u32)
}

/// Detects BMP files by checking for the "BM" signature at offset 0.
fn is_bmp(bytes: &[u8]) -> bool {
    bytes.len() >= 26 && bytes[0] == 0x42 && bytes[1] == 0x4D
}

/// Extracts BMP metadata from the DIB header.
///
/// The BITMAPINFOHEADER layout (and compatible V4/V5 headers) stores:
/// - width as a signed 32-bit integer at file offset 18
/// - height as a signed 32-bit integer at file offset 22 (negative = top-down)
/// - bits per pixel at file offset 28
fn sniff_bmp(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    if bytes.len() < 30 {
        return Err(TransformError::DecodeFailed(
            "bmp file is too short".to_string(),
        ));
    }

    let width = u32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    let raw_height = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]);
    let height = raw_height.unsigned_abs();
    let bits_per_pixel = u16::from_le_bytes([bytes[28], bytes[29]]);

    let has_alpha = bits_per_pixel == 32;

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: 1,
        duration: None,
        has_alpha: Some(has_alpha),
        orientation: None,
    })
}

/// Detects TIFF files by checking the byte-order marker and magic number.
///
/// Little-endian: `II` + 0x002A, big-endian: `MM` + 0x002A.
fn is_tiff(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && ((bytes[0] == b'I' && bytes[1] == b'I' && bytes[2] == 0x2A && bytes[3] == 0x00)
            || (bytes[0] == b'M' && bytes[1] == b'M' && bytes[2] == 0x00 && bytes[3] == 0x2A))
}

fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

/// Extracts GIF metadata by walking the block stream in the header.
///
/// The Logical Screen Descriptor carries the canvas size, which is also the size the
/// `image` crate reports for the decoded first frame, so a frame smaller than the canvas
/// still lines up. Frames and transparency need a walk over the block stream: `frame_count`
/// is how [`crate::inspect`] reports animation, and refusing to encode an animation depends
/// on getting the count right, so this deliberately walks the whole file rather than
/// stopping at the first image descriptor.
///
/// Transparency is read from the Graphic Control Extension's transparent-color flag. Like
/// [`sniff_png`], which reads the PNG color type, this reports what the container declares
/// rather than scanning pixels.
fn sniff_gif(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    // 6-byte signature + 7-byte Logical Screen Descriptor.
    if bytes.len() < 13 {
        return Err(TransformError::DecodeFailed(
            "gif file is too short".to_string(),
        ));
    }

    let width = u32::from(read_u16_le(&bytes[6..8])?);
    let height = u32::from(read_u16_le(&bytes[8..10])?);
    let packed = bytes[10];

    let mut offset = 13usize;
    // Skip the Global Color Table when the flag in the packed field is set. Its entry
    // count is 2^(N+1), three bytes per entry.
    if packed & 0b1000_0000 != 0 {
        let entries = 1usize << ((packed & 0b0000_0111) + 1);
        offset = offset.saturating_add(entries * 3);
    }

    let mut frame_count = 0u32;
    let mut has_alpha = false;

    while offset < bytes.len() {
        match bytes[offset] {
            // Trailer.
            0x3B => break,
            // Extension introducer.
            0x21 => {
                if offset + 1 >= bytes.len() {
                    break;
                }
                let label = bytes[offset + 1];
                let mut cursor = offset + 2;
                // A Graphic Control Extension declares transparency in bit 0 of the
                // packed field, which is the first byte of its single data sub-block.
                if label == 0xF9
                    && cursor + 2 < bytes.len()
                    && bytes[cursor] >= 1
                    && bytes[cursor + 1] & 0b0000_0001 != 0
                {
                    has_alpha = true;
                }
                cursor = skip_gif_sub_blocks(bytes, cursor)?;
                offset = cursor;
            }
            // Image descriptor: 10 bytes, then an optional Local Color Table, then the
            // LZW minimum code size byte, then the image data sub-blocks.
            0x2C => {
                frame_count = frame_count.saturating_add(1);
                if offset + 10 > bytes.len() {
                    break;
                }
                let local_packed = bytes[offset + 9];
                let mut cursor = offset + 10;
                if local_packed & 0b1000_0000 != 0 {
                    let entries = 1usize << ((local_packed & 0b0000_0111) + 1);
                    cursor = cursor.saturating_add(entries * 3);
                }
                // LZW minimum code size.
                cursor = cursor.saturating_add(1);
                cursor = skip_gif_sub_blocks(bytes, cursor)?;
                offset = cursor;
            }
            other => {
                return Err(TransformError::DecodeFailed(format!(
                    "gif file has an unknown block introducer 0x{other:02x}"
                )));
            }
        }
    }

    if frame_count == 0 {
        return Err(TransformError::DecodeFailed(
            "gif file contains no image data".to_string(),
        ));
    }

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count,
        duration: None,
        has_alpha: Some(has_alpha),
        orientation: None,
    })
}

/// Advances past a GIF sub-block chain, returning the offset just after its terminator.
///
/// A chain is a run of `[length: u8][length bytes]` records ending in a zero-length record.
/// Running off the end means the file is truncated, which is a decode failure rather than a
/// silently short read.
fn skip_gif_sub_blocks(bytes: &[u8], mut offset: usize) -> Result<usize, TransformError> {
    loop {
        if offset >= bytes.len() {
            return Err(TransformError::DecodeFailed(
                "gif file ends inside a data block".to_string(),
            ));
        }
        let len = bytes[offset] as usize;
        offset += 1;
        if len == 0 {
            return Ok(offset);
        }
        offset = offset.saturating_add(len);
    }
}

/// Extracts TIFF metadata by decoding the image header via the `image` crate.
fn sniff_tiff(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    let cursor = std::io::Cursor::new(bytes);
    let decoder = image::codecs::tiff::TiffDecoder::new(cursor)
        .map_err(|e| TransformError::DecodeFailed(format!("tiff decode: {e}")))?;
    let (width, height) = image::ImageDecoder::dimensions(&decoder);
    let color = image::ImageDecoder::color_type(&decoder);
    let has_alpha = matches!(
        color,
        image::ColorType::La8
            | image::ColorType::Rgba8
            | image::ColorType::La16
            | image::ColorType::Rgba16
            | image::ColorType::Rgba32F
    );
    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: 1,
        duration: None,
        has_alpha: Some(has_alpha),
        orientation: exif_orientation(MediaType::Tiff, bytes),
    })
}

fn sniff_png(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    if bytes.len() < 29 {
        return Err(TransformError::DecodeFailed(
            "png file is too short".to_string(),
        ));
    }

    if &bytes[12..16] != b"IHDR" {
        return Err(TransformError::DecodeFailed(
            "png file is missing an IHDR chunk".to_string(),
        ));
    }

    let width = read_u32_be(&bytes[16..20])?;
    let height = read_u32_be(&bytes[20..24])?;
    let color_type = bytes[25];
    let ancillary = png_ancillary_facts(bytes);
    let has_alpha = match color_type {
        // These two carry an alpha channel, and `tRNS` is not allowed alongside one.
        4 | 6 => Some(true),
        // These three have no alpha channel and may still be transparent: the
        // specification puts that transparency in a `tRNS` chunk, as a transparent grey
        // value, a transparent colour, or a per-entry palette alpha table. Reading only
        // IHDR calls a transparent palette PNG opaque while the same picture as a GIF,
        // whose sniffer walks the blocks, is called transparent.
        0 | 2 | 3 => Some(ancillary.has_trns),
        _ => None,
    };

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: ancillary.frame_count,
        duration: None,
        has_alpha,
        orientation: exif_orientation(MediaType::Png, bytes),
    })
}

/// What the chunks after IHDR say about transparency and animation.
struct PngAncillaryFacts {
    has_trns: bool,
    frame_count: u32,
}

/// Walks the chunk list for the two facts IHDR does not carry.
///
/// Both chunks are required to precede the image data, so the walk stops at the first
/// `IDAT` and a file carrying neither pays for a few chunk headers. A length that runs off
/// the end ends the walk with whatever was read: this reports facts about a file the
/// decoder has not seen yet, and a malformed chunk list is the decoder's to refuse.
fn png_ancillary_facts(bytes: &[u8]) -> PngAncillaryFacts {
    let mut facts = PngAncillaryFacts {
        has_trns: false,
        frame_count: 1,
    };

    // Past the 8-byte signature and the IHDR chunk, whose length is fixed at 13.
    let mut offset = 8 + 12 + 13;
    while offset + 8 <= bytes.len() {
        let Ok(length) = read_u32_be(&bytes[offset..offset + 4]) else {
            break;
        };
        let chunk_type = &bytes[offset + 4..offset + 8];
        match chunk_type {
            b"IDAT" | b"IEND" => break,
            b"tRNS" => facts.has_trns = true,
            // An APNG announces its frame count here, before the image data.
            b"acTL" if length >= 4 => {
                if let Ok(frames) = read_u32_be(&bytes[offset + 8..offset + 12]) {
                    facts.frame_count = frames.max(1);
                }
            }
            _ => {}
        }
        let Some(next) = offset
            .checked_add(12)
            .and_then(|next| next.checked_add(length as usize))
        else {
            break;
        };
        offset = next;
    }

    facts
}

/// Reads the EXIF Orientation tag out of any container that can carry one.
///
/// The tag says how the stored pixels are meant to be displayed, so a reader that honours
/// it in one container and not the next makes the container a photo happens to arrive in
/// decide whether the picture comes out upright. Browsers honour it in JPEG, PNG, WebP, and
/// TIFF alike, and honour the AVIF properties that mean the same, so truss reads all five
/// through here, and the sniffers and the transform pipeline both go through this function
/// so what `inspect` reports and what `convert` applies cannot drift.
///
/// A file with no EXIF block, no Orientation field, or an unreadable one reports `None`,
/// which means no transform. Each container is located by walking its headers rather than
/// by decoding it, which is what keeps the common file — the one carrying no metadata at
/// all — from paying for a container scan on every `sniff_artifact` call.
///
/// BMP and GIF have nowhere to put the tag. AVIF signals the same transform without an Exif
/// field, as `irot` and `imir` item properties, and [`avif_orientation`] folds those into
/// the same eight values, so a caller reads one number whatever the container.
pub(crate) fn exif_orientation(media_type: MediaType, bytes: &[u8]) -> Option<u16> {
    let payload = match media_type {
        MediaType::Jpeg => jpeg_exif_payload(bytes)?,
        MediaType::Png => png_exif_payload(bytes)?,
        MediaType::Webp => webp_exif_payload(bytes)?,
        // A TIFF file is an Exif block from byte zero, so it needs no locating.
        MediaType::Tiff => return tiff_orientation(bytes),
        MediaType::Avif => return avif_orientation(bytes),
        MediaType::Bmp | MediaType::Gif | MediaType::Svg => return None,
    };
    exif_orientation_from_payload(payload)
}

/// Returns the contents of a PNG `eXIf` chunk.
///
/// Only the chunk headers are walked, so no compressed data is touched: `sniff_artifact`
/// runs on every server upload and this runs with it. The chunk holds the Exif block
/// directly, but writers that carry the JPEG APP1 prefix over into it are common enough to
/// be worth stripping.
fn png_exif_payload(bytes: &[u8]) -> Option<&[u8]> {
    // Past the 8-byte signature; a shorter input never sniffed as a PNG.
    let mut offset = 8usize;
    while offset + 8 <= bytes.len() {
        let length = usize::try_from(read_u32_be(bytes.get(offset..offset + 4)?).ok()?).ok()?;
        let chunk_type = bytes.get(offset + 4..offset + 8)?;
        let start = offset + 8;
        let end = start.checked_add(length)?;
        if end > bytes.len() {
            return None;
        }
        if chunk_type == b"eXIf" {
            return Some(strip_exif_prefix(bytes.get(start..end)?));
        }
        if chunk_type == b"IEND" {
            return None;
        }
        // Past the payload and its CRC.
        offset = end.checked_add(4)?;
    }
    None
}

/// Returns the contents of a WebP `EXIF` chunk.
///
/// The chunk sits after the image data in an extended container, which is why this walks
/// the file rather than reusing the loop in [`sniff_webp`]: that one stops at the first
/// image chunk, which is what makes it cheap for the common file with no metadata at all.
fn webp_exif_payload(bytes: &[u8]) -> Option<&[u8]> {
    // Past "RIFF", the file size, and "WEBP".
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let chunk_tag = bytes.get(offset..offset + 4)?;
        let size = usize::try_from(read_u32_le(bytes.get(offset + 4..offset + 8)?).ok()?).ok()?;
        let start = offset + 8;
        let end = start.checked_add(size)?;
        if end > bytes.len() {
            return None;
        }
        if chunk_tag == b"EXIF" {
            return Some(strip_exif_prefix(bytes.get(start..end)?));
        }
        // RIFF chunks are padded to an even length.
        offset = end.checked_add(size % 2)?;
    }
    None
}

/// Drops the JPEG APP1 marker prefix when a writer has carried it into another container.
fn strip_exif_prefix(payload: &[u8]) -> &[u8] {
    payload
        .strip_prefix(b"Exif\0\0".as_slice())
        .unwrap_or(payload)
}

/// Reads the Orientation tag out of a bare TIFF header.
///
/// The entries of the first IFD are walked rather than the file being handed to the exif
/// crate, which reads from an owned buffer and would copy the whole image to reach twelve
/// bytes of it.
fn tiff_orientation(bytes: &[u8]) -> Option<u16> {
    let little_endian = match bytes.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };

    let read_u16 = |offset: usize| -> Option<u16> {
        let raw: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
        Some(if little_endian {
            u16::from_le_bytes(raw)
        } else {
            u16::from_be_bytes(raw)
        })
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(raw)
        } else {
            u32::from_be_bytes(raw)
        })
    };

    const ORIENTATION_TAG: u16 = 0x0112;
    const TYPE_SHORT: u16 = 3;
    const TYPE_LONG: u16 = 4;

    let ifd = usize::try_from(read_u32(4)?).ok()?;
    let entry_count = usize::from(read_u16(ifd)?);
    for index in 0..entry_count {
        let entry = ifd.checked_add(2)?.checked_add(index.checked_mul(12)?)?;
        if read_u16(entry)? != ORIENTATION_TAG {
            continue;
        }
        // A value short enough to fit is left-justified in the value field under either
        // byte order, so both widths are read from the same offset.
        return match read_u16(entry + 2)? {
            TYPE_SHORT => read_u16(entry + 8),
            TYPE_LONG => u16::try_from(read_u32(entry + 8)?).ok(),
            _ => None,
        };
    }
    None
}

/// Reads the Orientation tag out of an already-located Exif TIFF block.
fn exif_orientation_from_payload(payload: &[u8]) -> Option<u16> {
    let exif = exif::Reader::new().read_raw(payload.to_vec()).ok()?;
    let field = exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?;
    match &field.value {
        exif::Value::Short(values) => values.first().copied(),
        exif::Value::Long(values) => values.first().and_then(|value| u16::try_from(*value).ok()),
        _ => None,
    }
}

/// Returns the TIFF block of a JPEG's Exif APP1 segment, reading only segment headers.
fn jpeg_exif_payload(bytes: &[u8]) -> Option<&[u8]> {
    const EXIF_PREFIX: &[u8] = b"Exif\0\0";
    const APP1: u8 = 0xE1;

    let mut offset = 2;
    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xFF {
            return None;
        }
        while offset < bytes.len() && bytes[offset] == 0xFF {
            offset += 1;
        }

        let marker = *bytes.get(offset)?;
        offset += 1;

        // Start of scan or end of image: no metadata segment follows.
        if marker == 0xD9 || marker == 0xDA {
            return None;
        }
        // Standalone markers carry no length field.
        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue;
        }

        let length = read_u16_be(bytes.get(offset..offset + 2)?).ok()? as usize;
        if length < 2 || offset + length > bytes.len() {
            return None;
        }
        if marker == APP1
            && let Some(payload) = bytes[offset + 2..offset + length].strip_prefix(EXIF_PREFIX)
        {
            return Some(payload);
        }
        offset += length;
    }

    None
}

fn sniff_jpeg(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    let mut offset = 2;
    // Captured on the way past rather than by a second walk: the Exif APP1 segment always
    // precedes the SOF this loop is looking for.
    let mut exif_payload: Option<&[u8]> = None;

    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xFF {
            return Err(TransformError::DecodeFailed(
                "jpeg file has an invalid marker prefix".to_string(),
            ));
        }

        while offset < bytes.len() && bytes[offset] == 0xFF {
            offset += 1;
        }

        if offset >= bytes.len() {
            break;
        }

        let marker = bytes[offset];
        offset += 1;

        if marker == 0xD9 || marker == 0xDA {
            break;
        }

        if (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue;
        }

        if offset + 2 > bytes.len() {
            return Err(TransformError::DecodeFailed(
                "jpeg segment is truncated".to_string(),
            ));
        }

        let segment_length = read_u16_be(&bytes[offset..offset + 2])? as usize;
        if segment_length < 2 || offset + segment_length > bytes.len() {
            return Err(TransformError::DecodeFailed(
                "jpeg segment length is invalid".to_string(),
            ));
        }

        if marker == 0xE1
            && exif_payload.is_none()
            && let Some(payload) =
                bytes[offset + 2..offset + segment_length].strip_prefix(b"Exif\0\0".as_slice())
        {
            exif_payload = Some(payload);
        }

        if is_jpeg_sof_marker(marker) {
            if segment_length < 7 {
                return Err(TransformError::DecodeFailed(
                    "jpeg SOF segment is too short".to_string(),
                ));
            }

            let height = read_u16_be(&bytes[offset + 3..offset + 5])? as u32;
            let width = read_u16_be(&bytes[offset + 5..offset + 7])? as u32;

            return Ok(ArtifactMetadata {
                width: Some(width),
                height: Some(height),
                frame_count: 1,
                duration: None,
                has_alpha: Some(false),
                orientation: exif_payload.and_then(exif_orientation_from_payload),
            });
        }

        offset += segment_length;
    }

    Err(TransformError::DecodeFailed(
        "jpeg file is missing a SOF segment".to_string(),
    ))
}

fn sniff_webp(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    let mut offset = 12;

    while offset + 8 <= bytes.len() {
        let chunk_tag = &bytes[offset..offset + 4];
        let chunk_size = read_u32_le(&bytes[offset + 4..offset + 8])? as usize;
        let chunk_start = offset + 8;
        let chunk_end = chunk_start
            .checked_add(chunk_size)
            .ok_or_else(|| TransformError::DecodeFailed("webp chunk is too large".to_string()))?;

        if chunk_end > bytes.len() {
            return Err(TransformError::DecodeFailed(
                "webp chunk exceeds file length".to_string(),
            ));
        }

        let chunk_data = &bytes[chunk_start..chunk_end];

        let mut metadata = match chunk_tag {
            b"VP8X" => sniff_webp_vp8x(chunk_data)?,
            b"VP8 " => sniff_webp_vp8(chunk_data)?,
            b"VP8L" => sniff_webp_vp8l(chunk_data)?,
            _ => {
                offset = chunk_end + (chunk_size % 2);
                continue;
            }
        };

        // The EXIF chunk follows the image data, so it is read from the whole file rather
        // than from the chunk this loop stopped at. The frames are counted the same way,
        // since `ANMF` chunks also follow the header this loop stopped at.
        metadata.orientation = exif_orientation(MediaType::Webp, bytes);
        if metadata.frame_count > 1 {
            metadata.frame_count = count_webp_frames(bytes).max(2);
        }
        return Ok(metadata);
    }

    Err(TransformError::DecodeFailed(
        "webp file is missing an image chunk".to_string(),
    ))
}

/// Counts the `ANMF` chunks of an animated WebP, each of which holds one frame.
fn count_webp_frames(bytes: &[u8]) -> u32 {
    let mut frames = 0_u32;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let Ok(size) = read_u32_le(&bytes[offset + 4..offset + 8]) else {
            break;
        };
        if &bytes[offset..offset + 4] == b"ANMF" {
            frames = frames.saturating_add(1);
        }
        let size = size as usize;
        let Some(next) = offset
            .checked_add(8)
            .and_then(|next| next.checked_add(size))
            .and_then(|next| next.checked_add(size % 2))
        else {
            break;
        };
        offset = next;
    }
    frames
}

fn sniff_webp_vp8x(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    if bytes.len() < 10 {
        return Err(TransformError::DecodeFailed(
            "webp VP8X chunk is too short".to_string(),
        ));
    }

    let flags = bytes[0];
    let width = read_u24_le(&bytes[4..7])? + 1;
    let height = read_u24_le(&bytes[7..10])? + 1;
    let has_alpha = Some(flags & VP8X_ALPHA_FLAG != 0);
    // The frames themselves are counted by the caller, which has the whole file; this
    // records only that there is more than one of them, which is what the flag states.
    let frame_count = u32::from(flags & VP8X_ANIMATION_FLAG != 0) + 1;

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count,
        duration: None,
        has_alpha,
        orientation: None,
    })
}

/// The VP8X feature flags this sniffer reads, in the bit positions the container gives them.
const VP8X_ALPHA_FLAG: u8 = 0b0001_0000;
const VP8X_ANIMATION_FLAG: u8 = 0b0000_0010;

fn sniff_webp_vp8(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    if bytes.len() < 10 {
        return Err(TransformError::DecodeFailed(
            "webp VP8 chunk is too short".to_string(),
        ));
    }

    if bytes[3..6] != [0x9D, 0x01, 0x2A] {
        return Err(TransformError::DecodeFailed(
            "webp VP8 chunk has an invalid start code".to_string(),
        ));
    }

    let width = (read_u16_le(&bytes[6..8])? & 0x3FFF) as u32;
    let height = (read_u16_le(&bytes[8..10])? & 0x3FFF) as u32;

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: 1,
        duration: None,
        has_alpha: Some(false),
        orientation: None,
    })
}

fn sniff_webp_vp8l(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    if bytes.len() < 5 {
        return Err(TransformError::DecodeFailed(
            "webp VP8L chunk is too short".to_string(),
        ));
    }

    if bytes[0] != 0x2F {
        return Err(TransformError::DecodeFailed(
            "webp VP8L chunk has an invalid signature".to_string(),
        ));
    }

    // VP8L header bits, LSB first: 14 bits width-1, 14 bits height-1, 1 bit alpha_is_used,
    // 3 bits version.
    let bits = read_u32_le(&bytes[1..5])?;
    let width = (bits & 0x3FFF) + 1;
    let height = ((bits >> 14) & 0x3FFF) + 1;
    let has_alpha = (bits >> 28) & 1 != 0;

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: 1,
        duration: None,
        has_alpha: Some(has_alpha),
        orientation: None,
    })
}

fn is_jpeg_sof_marker(marker: u8) -> bool {
    matches!(
        marker,
        0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE | 0xCF
    )
}

fn read_u16_be(bytes: &[u8]) -> Result<u16, TransformError> {
    let array: [u8; 2] = bytes
        .try_into()
        .map_err(|_| TransformError::DecodeFailed("expected 2 bytes".to_string()))?;
    Ok(u16::from_be_bytes(array))
}

fn read_u16_le(bytes: &[u8]) -> Result<u16, TransformError> {
    let array: [u8; 2] = bytes
        .try_into()
        .map_err(|_| TransformError::DecodeFailed("expected 2 bytes".to_string()))?;
    Ok(u16::from_le_bytes(array))
}

fn read_u24_le(bytes: &[u8]) -> Result<u32, TransformError> {
    if bytes.len() != 3 {
        return Err(TransformError::DecodeFailed("expected 3 bytes".to_string()));
    }

    Ok(u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16))
}

fn read_u32_be(bytes: &[u8]) -> Result<u32, TransformError> {
    let array: [u8; 4] = bytes
        .try_into()
        .map_err(|_| TransformError::DecodeFailed("expected 4 bytes".to_string()))?;
    Ok(u32::from_be_bytes(array))
}

fn read_u32_le(bytes: &[u8]) -> Result<u32, TransformError> {
    let array: [u8; 4] = bytes
        .try_into()
        .map_err(|_| TransformError::DecodeFailed("expected 4 bytes".to_string()))?;
    Ok(u32::from_le_bytes(array))
}

fn read_u64_be(bytes: &[u8]) -> Result<u64, TransformError> {
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| TransformError::DecodeFailed("expected 8 bytes".to_string()))?;
    Ok(u64::from_be_bytes(array))
}

#[cfg(test)]
mod tests {
    #[cfg(any(feature = "server", feature = "wasm"))]
    use super::single_line;
    use super::{
        Artifact, ArtifactMetadata, Dimensions, Fit, MediaType, MetadataPolicy, OptimizeMode,
        Position, QualityMetric, RawArtifact, Rgba8, Rotation, TargetQuality, TransformError,
        TransformOptions, TransformRequest, exif_orientation, sniff_artifact,
        validate_height_value, validate_quality_value, validate_watermark_opacity_value,
        validate_width_value,
    };
    #[cfg(feature = "avif")]
    use image::codecs::avif::AvifEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    use rstest::rstest;

    /// One spelling per named value, across every parser that takes one.
    ///
    /// The vocabulary is what a caller learns once and reuses: `--fit cover` teaches that
    /// these values are written as the documentation writes them, and a metric that also
    /// took `SSIM` was the one place that lesson did not hold.
    #[test]
    fn a_named_value_has_one_spelling() {
        use std::str::FromStr;

        assert!(MediaType::from_str("jpeg").is_ok());
        assert!(MediaType::from_str("JPEG").is_err());
        assert!(Fit::from_str("cover").is_ok());
        assert!(Fit::from_str("COVER").is_err());
        assert!(Position::from_str("center").is_ok());
        assert!(Position::from_str("CENTER").is_err());
        assert!(OptimizeMode::from_str("lossless").is_ok());
        assert!(OptimizeMode::from_str("LOSSLESS").is_err());
        assert!(TargetQuality::from_str("ssim:0.98").is_ok());
        assert_eq!(
            TargetQuality::from_str("SSIM:0.98"),
            Err("unsupported target quality metric `SSIM`".to_string())
        );
        assert_eq!(
            TargetQuality::from_str("Psnr:42"),
            Err("unsupported target quality metric `Psnr`".to_string())
        );
    }

    /// A message that is already one trimmed line is what it was.
    #[cfg(any(feature = "server", feature = "wasm"))]
    #[test]
    fn single_line_leaves_a_line_alone() {
        assert_eq!(
            single_line("quality must be between 1 and 100"),
            "quality must be between 1 and 100"
        );
        assert_eq!(single_line(""), "");
    }

    /// The wording a decoder in a dependency produced, which ends with a newline.
    #[cfg(any(feature = "server", feature = "wasm"))]
    #[test]
    fn single_line_folds_a_message_that_leaves_its_line() {
        assert_eq!(
            single_line("Format error decoding Jpeg: Not enough bytes\n"),
            "Format error decoding Jpeg: Not enough bytes"
        );
        assert_eq!(single_line("first\nsecond"), "first second");
        assert_eq!(single_line("first\r\n\tsecond"), "first second");
        assert_eq!(single_line("  padded  "), "padded");
    }

    fn jpeg_artifact() -> Artifact {
        Artifact::new(vec![1, 2, 3], MediaType::Jpeg, ArtifactMetadata::default())
    }

    /// A PNG header and nothing else: eight signature bytes and an IHDR, with no image
    /// data. It is what the sniffer reads, and it is deliberately not a decodable file.
    fn png_ihdr_bytes(width: u32, height: u32, color_type: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes.extend_from_slice(&13_u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.push(8);
        bytes.push(color_type);
        bytes.push(0);
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes
    }

    /// A PNG signature, an IHDR, and whatever chunks the caller wants after it.
    ///
    /// The sniffers read headers rather than pixels, so a file with no IDAT is enough to
    /// describe every fact they report.
    fn png_bytes_with_chunks(color_type: u8, chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut bytes = png_ihdr_bytes(8, 8, color_type);
        for (chunk_type, data) in chunks {
            bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
            bytes.extend_from_slice(*chunk_type);
            bytes.extend_from_slice(data);
            bytes.extend_from_slice(&0_u32.to_be_bytes());
        }
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(b"IEND");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes
    }

    fn jpeg_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        bytes.extend_from_slice(&[0; 14]);
        bytes.extend_from_slice(&[
            0xFF,
            0xC0,
            0x00,
            0x11,
            0x08,
            (height >> 8) as u8,
            height as u8,
            (width >> 8) as u8,
            width as u8,
            0x03,
            0x01,
            0x11,
            0x00,
            0x02,
            0x11,
            0x00,
            0x03,
            0x11,
            0x00,
        ]);
        bytes.extend_from_slice(&[0xFF, 0xD9]);
        bytes
    }

    /// Builds a GIF whose block structure is valid, which is all `sniff_gif` walks.
    ///
    /// The LZW payload is deliberately opaque bytes: the sniffer never decodes image data,
    /// so a real compressed stream would only obscure what each test is pinning.
    fn gif_bytes(
        version: &[u8; 3],
        width: u16,
        height: u16,
        frames: usize,
        transparent: bool,
    ) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GIF");
        bytes.extend_from_slice(version);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        // Global Color Table present, 2 entries (2^(0+1)).
        bytes.push(0b1000_0000);
        bytes.push(0); // background color index
        bytes.push(0); // pixel aspect ratio
        bytes.extend_from_slice(&[0xFF, 0x00, 0x00, 0x00, 0x00, 0xFF]);

        for _ in 0..frames {
            if transparent {
                // Graphic Control Extension with the transparent-color flag set.
                bytes.extend_from_slice(&[0x21, 0xF9, 0x04, 0b0000_0001, 0x00, 0x00, 0x00, 0x00]);
            }
            // Image descriptor: position, size, no local color table.
            bytes.push(0x2C);
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&width.to_le_bytes());
            bytes.extend_from_slice(&height.to_le_bytes());
            bytes.push(0);
            // LZW minimum code size, then one data sub-block and the terminator.
            bytes.push(0x02);
            bytes.extend_from_slice(&[0x02, 0x44, 0x01, 0x00]);
        }

        bytes.push(0x3B);
        bytes
    }

    fn webp_vp8x_bytes(width: u32, height: u32, flags: u8) -> Vec<u8> {
        let width_minus_one = width - 1;
        let height_minus_one = height - 1;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&30_u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8X");
        bytes.extend_from_slice(&10_u32.to_le_bytes());
        bytes.push(flags);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&[
            (width_minus_one & 0xFF) as u8,
            ((width_minus_one >> 8) & 0xFF) as u8,
            ((width_minus_one >> 16) & 0xFF) as u8,
        ]);
        bytes.extend_from_slice(&[
            (height_minus_one & 0xFF) as u8,
            ((height_minus_one >> 8) & 0xFF) as u8,
            ((height_minus_one >> 16) & 0xFF) as u8,
        ]);
        bytes
    }

    fn webp_vp8l_bytes(width: u32, height: u32) -> Vec<u8> {
        webp_vp8l_bytes_with_alpha(width, height, false)
    }

    fn webp_vp8l_bytes_with_alpha(width: u32, height: u32, alpha_is_used: bool) -> Vec<u8> {
        let packed = (width - 1) | ((height - 1) << 14) | (u32::from(alpha_is_used) << 28);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&17_u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBP");
        bytes.extend_from_slice(b"VP8L");
        bytes.extend_from_slice(&5_u32.to_le_bytes());
        bytes.push(0x2F);
        bytes.extend_from_slice(&packed.to_le_bytes());
        bytes.push(0);
        bytes
    }

    fn avif_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24_u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"avif");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(b"mif1");
        bytes.extend_from_slice(b"avif");
        bytes
    }

    #[cfg(feature = "avif")]
    fn encoded_avif_bytes(width: u32, height: u32, fill: Rgba<u8>) -> Vec<u8> {
        let image = RgbaImage::from_pixel(width, height, fill);
        let mut bytes = Vec::new();
        AvifEncoder::new(&mut bytes)
            .write_image(&image, width, height, ColorType::Rgba8.into())
            .expect("encode avif");
        bytes
    }

    #[test]
    fn default_transform_options_match_documented_defaults() {
        let options = TransformOptions::default();

        assert_eq!(options.width, None);
        assert_eq!(options.height, None);
        assert_eq!(options.fit, None);
        assert_eq!(options.position, None);
        assert_eq!(options.format, None);
        assert_eq!(options.quality, None);
        assert_eq!(options.rotate, Rotation::DEG_0);
        assert!(options.auto_orient);
        assert!(options.strip_metadata);
        assert!(!options.preserve_exif);
    }

    #[test]
    fn media_type_helpers_report_expected_values() {
        assert_eq!(MediaType::Jpeg.as_name(), "jpeg");
        assert_eq!(MediaType::Jpeg.as_mime(), "image/jpeg");
        assert!(MediaType::Webp.is_lossy());
        assert!(!MediaType::Png.is_lossy());
    }

    #[test]
    fn media_type_parsing_accepts_documented_names() {
        assert_eq!("jpeg".parse::<MediaType>(), Ok(MediaType::Jpeg));
        assert_eq!("jpg".parse::<MediaType>(), Ok(MediaType::Jpeg));
        assert_eq!("png".parse::<MediaType>(), Ok(MediaType::Png));
        // `gif` parses: it is a supported input. Whether it may be used as an output is a
        // separate question, answered by `is_encodable`.
        assert_eq!("gif".parse::<MediaType>(), Ok(MediaType::Gif));
        assert!("heic".parse::<MediaType>().is_err());
    }

    #[test]
    fn fit_position_rotation_and_color_parsing_work() {
        assert_eq!("cover".parse::<Fit>(), Ok(Fit::Cover));
        assert_eq!(
            "bottom-right".parse::<Position>(),
            Ok(Position::BottomRight)
        );
        assert_eq!("270".parse::<Rotation>(), Ok(Rotation::DEG_270));
        assert_eq!(
            Rgba8::from_hex("AABBCCDD"),
            Ok(Rgba8 {
                r: 0xAA,
                g: 0xBB,
                b: 0xCC,
                a: 0xDD
            })
        );
        assert!(Rgba8::from_hex("AABB").is_err());

        // Non-ASCII input must not panic (even if byte length happens to be 6 or 8).
        assert!(Rgba8::from_hex("\u{00e9}\u{00e9}\u{00e9}").is_err());
        assert!(Rgba8::from_hex("\u{1f600}\u{1f600}").is_err());
    }

    #[test]
    fn normalize_defaults_fit_and_position_for_bounded_resize() {
        let normalized = TransformOptions {
            width: Some(1200),
            height: Some(630),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize bounded resize");

        assert_eq!(normalized.fit, Some(Fit::Contain));
        assert_eq!(normalized.position, Position::Center);
        assert_eq!(normalized.format, MediaType::Jpeg);
        assert_eq!(normalized.metadata_policy, MetadataPolicy::StripAll);
    }

    #[test]
    fn normalize_uses_requested_fit_and_output_format() {
        let normalized = TransformOptions {
            width: Some(320),
            height: Some(320),
            fit: Some(Fit::Cover),
            position: Some(Position::BottomRight),
            format: Some(MediaType::Webp),
            quality: Some(70),
            strip_metadata: false,
            preserve_exif: true,
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize explicit values");

        assert_eq!(normalized.fit, Some(Fit::Cover));
        assert_eq!(normalized.position, Position::BottomRight);
        assert_eq!(normalized.format, MediaType::Webp);
        assert_eq!(normalized.quality, Some(70));
        assert_eq!(normalized.metadata_policy, MetadataPolicy::PreserveExif);
    }

    #[test]
    fn normalize_can_keep_all_metadata() {
        let normalized = TransformOptions {
            strip_metadata: false,
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize keep metadata");

        assert_eq!(normalized.metadata_policy, MetadataPolicy::KeepAll);
    }

    #[test]
    fn normalize_lossy_optimize_preserves_icc_by_default() {
        let normalized = TransformOptions {
            optimize: OptimizeMode::Lossy,
            format: Some(MediaType::Jpeg),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize lossy optimize metadata policy");

        assert_eq!(normalized.metadata_policy, MetadataPolicy::PreserveIcc);
    }

    /// Every optimization mode re-encodes, and a profile dropped by any of them renders
    /// the picture in the wrong colors. Only `none`, which is what `truss convert` does
    /// when nobody asks for an optimization, strips the way the flag says.
    #[rstest]
    #[case::none(OptimizeMode::None, MetadataPolicy::StripAll)]
    #[case::auto(OptimizeMode::Auto, MetadataPolicy::PreserveIcc)]
    #[case::lossless(OptimizeMode::Lossless, MetadataPolicy::PreserveIcc)]
    #[case::lossy(OptimizeMode::Lossy, MetadataPolicy::PreserveIcc)]
    fn an_optimization_keeps_the_profile_a_plain_encode_strips(
        #[case] optimize: OptimizeMode,
        #[case] expected: MetadataPolicy,
    ) {
        let normalized = TransformOptions {
            optimize,
            strip_metadata: true,
            format: Some(MediaType::Jpeg),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize the metadata policy");

        assert_eq!(normalized.metadata_policy, expected);
    }

    /// A format that cannot carry a profile has nothing to preserve, and asking for it
    /// there is what made `--strip-metadata` fail outright before the upgrade was limited
    /// to formats that can take one. AVIF is the only such format an optimization mode
    /// reaches: TIFF and BMP refuse the mode itself.
    #[test]
    fn an_optimization_strips_for_a_format_that_carries_no_profile() {
        let normalized = TransformOptions {
            optimize: OptimizeMode::Auto,
            strip_metadata: true,
            format: Some(MediaType::Avif),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize the metadata policy");

        assert_eq!(normalized.metadata_policy, MetadataPolicy::StripAll);
    }

    #[test]
    fn normalize_lossy_optimize_preserves_icc_for_webp_output() {
        let normalized = TransformOptions {
            optimize: OptimizeMode::Lossy,
            format: Some(MediaType::Webp),
            strip_metadata: true,
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize lossy webp metadata policy");

        assert_eq!(normalized.metadata_policy, MetadataPolicy::PreserveIcc);
    }

    // Regression test for https://github.com/nao1215/truss/issues/279: the ICC upgrade must
    // not apply to a format that cannot carry a profile, or `--strip-metadata` puts the
    // pipeline into a state the encoder rejects.
    #[test]
    fn normalize_lossy_optimize_strips_all_for_a_format_without_icc_support() {
        let normalized = TransformOptions {
            optimize: OptimizeMode::Lossy,
            format: Some(MediaType::Avif),
            strip_metadata: true,
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize lossy avif metadata policy");

        assert_eq!(normalized.metadata_policy, MetadataPolicy::StripAll);
    }

    #[test]
    fn normalize_keeps_fit_none_when_resize_is_not_bounded() {
        let normalized = TransformOptions {
            width: Some(500),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("normalize unbounded resize");

        assert_eq!(normalized.fit, None);
        assert_eq!(normalized.position, Position::Center);
    }

    #[test]
    fn normalize_rejects_zero_dimensions() {
        let err = TransformOptions {
            width: Some(0),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("zero width should fail");

        assert_eq!(
            err,
            TransformError::InvalidOptions("width must be greater than zero".to_string())
        );
    }

    #[test]
    fn normalize_rejects_fit_without_both_dimensions() {
        let err = TransformOptions {
            width: Some(300),
            fit: Some(Fit::Contain),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("fit without bounded resize should fail");

        assert_eq!(
            err,
            TransformError::InvalidOptions("fit requires both width and height".to_string())
        );
    }

    /// Every rule the options settle between themselves gives the same message whether
    /// it is reached through `normalize`, which has an input, or through
    /// `validate_without_input`, which is what the HTTP server and `truss sign` call.
    /// Two lists would drift; one list read twice cannot.
    #[rstest]
    #[case(
        TransformOptions { width: Some(300), fit: Some(Fit::Contain), ..TransformOptions::default() },
        "fit requires both width and height"
    )]
    #[case(
        TransformOptions { height: Some(300), position: Some(Position::Top), ..TransformOptions::default() },
        "position requires both width and height"
    )]
    #[case(
        TransformOptions { without_enlargement: true, ..TransformOptions::default() },
        "withoutEnlargement requires width or height"
    )]
    #[case(
        TransformOptions { width: Some(0), ..TransformOptions::default() },
        "width must be greater than zero"
    )]
    #[case(
        TransformOptions { height: Some(0), ..TransformOptions::default() },
        "height must be greater than zero"
    )]
    #[case(
        TransformOptions { quality: Some(101), format: Some(MediaType::Jpeg), ..TransformOptions::default() },
        "quality must be between 1 and 100"
    )]
    #[case(
        TransformOptions { blur: Some(200.0), ..TransformOptions::default() },
        "blur sigma must be between 0.1 and 100.0"
    )]
    #[case(
        TransformOptions { sharpen: Some(500.0), ..TransformOptions::default() },
        "sharpen sigma must be between 0.1 and 100.0"
    )]
    #[case(
        TransformOptions {
            crop: Some(crate::CropRegion { x: 0, y: 0, width: 0, height: 0 }),
            ..TransformOptions::default()
        },
        "crop width and height must be greater than zero"
    )]
    fn the_input_independent_rules_answer_the_same_through_either_door(
        #[case] options: TransformOptions,
        #[case] message: &str,
    ) {
        let expected = TransformError::InvalidOptions(message.to_string());

        assert_eq!(
            options
                .validate_without_input()
                .expect_err("the options contradict each other whatever the input is"),
            expected
        );
        assert_eq!(
            options
                .normalize(MediaType::Jpeg)
                .expect_err("normalize runs the same list first"),
            expected
        );
    }

    #[test]
    fn normalize_rejects_position_without_both_dimensions() {
        let err = TransformOptions {
            height: Some(300),
            position: Some(Position::Top),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("position without bounded resize should fail");

        assert_eq!(
            err,
            TransformError::InvalidOptions("position requires both width and height".to_string())
        );
    }

    #[test]
    fn normalize_rejects_quality_for_lossless_output() {
        let err = TransformOptions {
            format: Some(MediaType::Png),
            quality: Some(80),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("quality for png should fail");

        assert_eq!(
            err,
            TransformError::InvalidOptions("quality requires a lossy output format".to_string())
        );
    }

    #[test]
    fn normalize_rejects_zero_quality() {
        let err = TransformOptions {
            quality: Some(0),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("zero quality should fail");

        assert_eq!(
            err,
            TransformError::InvalidOptions("quality must be between 1 and 100".to_string())
        );
    }

    #[test]
    fn normalize_rejects_quality_above_one_hundred() {
        let err = TransformOptions {
            quality: Some(101),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("quality above one hundred should fail");

        assert_eq!(
            err,
            TransformError::InvalidOptions("quality must be between 1 and 100".to_string())
        );
    }

    #[test]
    fn normalize_rejects_preserve_exif_when_metadata_is_stripped() {
        let err = TransformOptions {
            preserve_exif: true,
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("preserve_exif should require metadata retention");

        assert_eq!(
            err,
            TransformError::InvalidOptions(
                "preserveExif requires stripMetadata to be false".to_string()
            )
        );
    }

    #[test]
    fn normalize_validates_optimize_and_target_quality_matrix() {
        struct Case {
            name: &'static str,
            input_media_type: MediaType,
            options: TransformOptions,
            expected_error: Option<&'static str>,
        }

        let cases = [
            Case {
                name: "target quality requires optimize auto or lossy",
                input_media_type: MediaType::Jpeg,
                options: TransformOptions {
                    format: Some(MediaType::Jpeg),
                    target_quality: Some(TargetQuality {
                        metric: QualityMetric::Ssim,
                        value: 0.98,
                    }),
                    ..TransformOptions::default()
                },
                expected_error: Some("targetQuality requires optimize=auto or optimize=lossy"),
            },
            Case {
                name: "target quality not allowed with lossless optimize",
                input_media_type: MediaType::Webp,
                options: TransformOptions {
                    format: Some(MediaType::Webp),
                    optimize: OptimizeMode::Lossless,
                    target_quality: Some(TargetQuality {
                        metric: QualityMetric::Ssim,
                        value: 0.98,
                    }),
                    ..TransformOptions::default()
                },
                expected_error: Some("targetQuality requires optimize=auto or optimize=lossy"),
            },
            Case {
                name: "target quality requires lossy optimizable output",
                input_media_type: MediaType::Png,
                options: TransformOptions {
                    format: Some(MediaType::Png),
                    optimize: OptimizeMode::Auto,
                    target_quality: Some(TargetQuality {
                        metric: QualityMetric::Ssim,
                        value: 0.98,
                    }),
                    ..TransformOptions::default()
                },
                expected_error: Some("targetQuality requires jpeg, webp, or avif output"),
            },
            Case {
                name: "quality cannot combine with lossless optimize",
                input_media_type: MediaType::Jpeg,
                options: TransformOptions {
                    format: Some(MediaType::Jpeg),
                    optimize: OptimizeMode::Lossless,
                    quality: Some(80),
                    ..TransformOptions::default()
                },
                expected_error: Some("quality cannot be combined with optimize=lossless"),
            },
            Case {
                name: "lossy optimize requires lossy capable format",
                input_media_type: MediaType::Png,
                options: TransformOptions {
                    format: Some(MediaType::Png),
                    optimize: OptimizeMode::Lossy,
                    ..TransformOptions::default()
                },
                expected_error: Some(
                    "lossy optimization requires jpeg, webp, or avif output, got png",
                ),
            },
            Case {
                name: "optimize unsupported for svg output",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    optimize: OptimizeMode::Auto,
                    ..TransformOptions::default()
                },
                expected_error: Some("optimization is not supported for svg output"),
            },
            Case {
                name: "preserve exif unsupported for svg output",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    preserve_exif: true,
                    strip_metadata: false,
                    ..TransformOptions::default()
                },
                expected_error: Some("preserveExif is not supported with SVG output"),
            },
            // SVG output is a sanitize-only passthrough: the document comes back as written,
            // so an option asking for a different picture cannot be honoured. Refusing it is
            // what the rules above already do for the options they cover.
            Case {
                name: "width unsupported for svg output",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    width: Some(100),
                    ..TransformOptions::default()
                },
                expected_error: Some(
                    "width is not supported with SVG output; choose a raster output format such as png",
                ),
            },
            Case {
                name: "height unsupported for svg output",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    height: Some(100),
                    ..TransformOptions::default()
                },
                expected_error: Some(
                    "height is not supported with SVG output; choose a raster output format such as png",
                ),
            },
            Case {
                name: "rotate unsupported for svg output",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    rotate: Rotation::DEG_90,
                    ..TransformOptions::default()
                },
                expected_error: Some(
                    "rotate is not supported with SVG output; choose a raster output format such as png",
                ),
            },
            Case {
                name: "grayscale unsupported for svg output",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    grayscale: true,
                    ..TransformOptions::default()
                },
                expected_error: Some(
                    "grayscale is not supported with SVG output; choose a raster output format such as png",
                ),
            },
            Case {
                name: "background unsupported for svg output",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    background: Some(Rgba8 {
                        r: 255,
                        g: 0,
                        b: 0,
                        a: 255,
                    }),
                    ..TransformOptions::default()
                },
                expected_error: Some(
                    "background is not supported with SVG output; choose a raster output format such as png",
                ),
            },
            Case {
                name: "svg passthrough with no transform options is accepted",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Svg),
                    rotate: Rotation::DEG_0,
                    ..TransformOptions::default()
                },
                expected_error: None,
            },
            Case {
                name: "svg input rasterized to png accepts the same options",
                input_media_type: MediaType::Svg,
                options: TransformOptions {
                    format: Some(MediaType::Png),
                    width: Some(100),
                    height: Some(100),
                    rotate: Rotation::DEG_90,
                    grayscale: true,
                    ..TransformOptions::default()
                },
                expected_error: None,
            },
            Case {
                name: "auto optimize accepts lossy target quality",
                input_media_type: MediaType::Jpeg,
                options: TransformOptions {
                    format: Some(MediaType::Jpeg),
                    optimize: OptimizeMode::Auto,
                    target_quality: Some(TargetQuality {
                        metric: QualityMetric::Ssim,
                        value: 0.98,
                    }),
                    ..TransformOptions::default()
                },
                expected_error: None,
            },
            Case {
                name: "lossless optimize accepts png without quality",
                input_media_type: MediaType::Png,
                options: TransformOptions {
                    format: Some(MediaType::Png),
                    optimize: OptimizeMode::Lossless,
                    ..TransformOptions::default()
                },
                expected_error: None,
            },
        ];

        for case in cases {
            let result = case.options.normalize(case.input_media_type);
            match case.expected_error {
                Some(message) => {
                    let error = result.expect_err(case.name);
                    assert_eq!(
                        error,
                        TransformError::InvalidOptions(message.to_string()),
                        "{}",
                        case.name
                    );
                }
                None => {
                    result.expect(case.name);
                }
            }
        }
    }

    #[test]
    fn transform_request_normalize_uses_input_media_type_as_default_output() {
        let request = TransformRequest::new(jpeg_artifact(), TransformOptions::default());
        let normalized = request.normalize().expect("normalize request");

        assert_eq!(normalized.input.media_type, MediaType::Jpeg);
        assert_eq!(normalized.options.format, MediaType::Jpeg);
        assert_eq!(normalized.options.metadata_policy, MetadataPolicy::StripAll);
    }

    #[test]
    fn sniff_artifact_detects_png_dimensions_and_alpha() {
        let artifact =
            sniff_artifact(RawArtifact::new(png_ihdr_bytes(64, 32, 6), None)).expect("sniff png");

        assert_eq!(artifact.media_type, MediaType::Png);
        assert_eq!(artifact.metadata.width, Some(64));
        assert_eq!(artifact.metadata.height, Some(32));
        assert_eq!(artifact.metadata.has_alpha, Some(true));
    }

    #[test]
    fn sniff_artifact_detects_jpeg_dimensions() {
        let artifact =
            sniff_artifact(RawArtifact::new(jpeg_bytes(320, 240), None)).expect("sniff jpeg");

        assert_eq!(artifact.media_type, MediaType::Jpeg);
        assert_eq!(artifact.metadata.width, Some(320));
        assert_eq!(artifact.metadata.height, Some(240));
        assert_eq!(artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn normalize_defaults_gif_input_to_png_output() {
        // "Keep the input format" cannot mean GIF, because truss has no GIF encoder.
        let options = TransformOptions::default()
            .normalize(MediaType::Gif)
            .expect("gif input should normalize");

        assert_eq!(options.format, MediaType::Png);
    }

    #[test]
    fn normalize_keeps_an_explicit_format_for_gif_input() {
        let options = TransformOptions {
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Gif)
        .expect("gif input with an explicit format should normalize");

        assert_eq!(options.format, MediaType::Webp);
    }

    #[test]
    fn gif_is_not_encodable() {
        assert!(!MediaType::Gif.is_encodable());
        for media_type in [
            MediaType::Jpeg,
            MediaType::Png,
            MediaType::Webp,
            MediaType::Avif,
            MediaType::Svg,
            MediaType::Bmp,
            MediaType::Tiff,
        ] {
            assert!(
                media_type.is_encodable(),
                "{} should be encodable",
                media_type.as_name()
            );
        }
    }

    #[test]
    fn sniff_artifact_detects_static_gif87a() {
        let artifact = sniff_artifact(RawArtifact::new(
            gif_bytes(b"87a", 640, 480, 1, false),
            None,
        ))
        .expect("sniff gif87a");

        assert_eq!(artifact.media_type, MediaType::Gif);
        assert_eq!(artifact.metadata.width, Some(640));
        assert_eq!(artifact.metadata.height, Some(480));
        assert_eq!(artifact.metadata.frame_count, 1);
        assert_eq!(artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn sniff_artifact_detects_gif89a_transparency() {
        let artifact = sniff_artifact(RawArtifact::new(gif_bytes(b"89a", 4, 4, 1, true), None))
            .expect("sniff transparent gif");

        assert_eq!(artifact.media_type, MediaType::Gif);
        assert_eq!(
            artifact.metadata.has_alpha,
            Some(true),
            "a Graphic Control Extension with the transparent-color flag means alpha"
        );
    }

    #[test]
    fn sniff_artifact_counts_gif_frames() {
        // Frame count is what `inspect` turns into `isAnimated` and what the transform
        // pipeline refuses on, so the walk has to reach every image descriptor rather than
        // stopping at the first one.
        let artifact = sniff_artifact(RawArtifact::new(gif_bytes(b"89a", 8, 8, 5, true), None))
            .expect("sniff animated gif");

        assert_eq!(artifact.metadata.frame_count, 5);
    }

    /// A rejected colour is told what a colour looks like.
    ///
    /// Every other option in truss names its own rule in the failure, and `--background`
    /// answered every wrong spelling with the value repeated back. The assertion is the
    /// property rather than the sentence: the message says how many digits and says that no
    /// `#` is used, whatever wording carries it.
    #[test]
    fn a_rejected_color_is_told_what_a_color_looks_like() {
        for value in [
            "#ffffff", "fff", "white", "0xffffff", "FFFFFFF", "", "gggggg",
        ] {
            let message = Rgba8::from_hex(value).expect_err("not a color");
            assert!(
                message.contains("six or eight") && message.contains("hexadecimal"),
                "{value:?} was not told the digit count: {message}"
            );
            assert!(
                message.contains('#'),
                "{value:?} was not told that no `#` is used: {message}"
            );
        }

        for value in ["ffffff", "FFFFFF", "ffffffaa", "000000"] {
            assert!(
                Rgba8::from_hex(value).is_ok(),
                "{value:?} should be a color"
            );
        }
    }

    #[test]
    fn sniff_artifact_detects_an_animated_avif() {
        // An animated AVIF is a moving-image sequence, and the container says so in its
        // brands: `avis` is the sequence brand, which `is_avif_brand` already accepts as a
        // reason to call the file an AVIF at all.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24_u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"avis");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(b"avis");
        bytes.extend_from_slice(b"avif");
        let artifact =
            sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff an animated avif");

        assert!(
            artifact.metadata.frame_count > 1,
            "an animated avif reported {} frames",
            artifact.metadata.frame_count
        );
    }

    #[test]
    fn sniff_artifact_counts_the_frames_of_an_animated_avif() {
        // The frames are samples of a `moov` track, and the count is in `stsz`. The refusal
        // prints the number, so a placeholder there would state a count nothing measured.
        fn mp4_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
            out.extend_from_slice(box_type);
            out.extend_from_slice(payload);
            out
        }

        let mut stsz = vec![0_u8; 4];
        stsz.extend_from_slice(&0_u32.to_be_bytes());
        stsz.extend_from_slice(&7_u32.to_be_bytes());
        let stbl = mp4_box(b"stbl", &mp4_box(b"stsz", &stsz));
        let minf = mp4_box(b"minf", &stbl);
        let mdia = mp4_box(b"mdia", &minf);
        let trak = mp4_box(b"trak", &mdia);
        let moov = mp4_box(b"moov", &trak);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24_u32.to_be_bytes());
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"avis");
        bytes.extend_from_slice(&0_u32.to_be_bytes());
        bytes.extend_from_slice(b"avis");
        bytes.extend_from_slice(b"avif");
        bytes.extend_from_slice(&moov);

        let artifact =
            sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff an animated avif");

        assert_eq!(artifact.metadata.frame_count, 7);
    }

    #[test]
    fn sniff_artifact_counts_the_frames_of_an_animated_png() {
        // An APNG announces its frame count in an `acTL` chunk before the image data. The
        // IHDR says nothing about it, so a sniffer that stops there calls the file static.
        let mut actl = Vec::new();
        actl.extend_from_slice(&4_u32.to_be_bytes());
        actl.extend_from_slice(&0_u32.to_be_bytes());
        let artifact = sniff_artifact(RawArtifact::new(
            png_bytes_with_chunks(2, &[(b"acTL", actl)]),
            None,
        ))
        .expect("sniff an animated png");

        assert_eq!(artifact.metadata.frame_count, 4);
    }

    #[test]
    fn sniff_artifact_counts_the_frames_of_an_animated_webp() {
        // Bit 1 of the VP8X flags is the animation flag, beside the alpha flag at bit 4 that
        // the sniffer already reads, and the frames follow in `ANMF` chunks.
        const ANIMATION: u8 = 0b0000_0010;
        let mut bytes = webp_vp8x_bytes(8, 8, ANIMATION);
        for _ in 0..3 {
            bytes.extend_from_slice(b"ANMF");
            bytes.extend_from_slice(&0_u32.to_le_bytes());
        }
        let riff_len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&riff_len.to_le_bytes());
        let artifact =
            sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff an animated webp");

        assert!(
            artifact.metadata.frame_count > 1,
            "an animated webp reported {} frames",
            artifact.metadata.frame_count
        );
    }

    #[test]
    fn sniff_artifact_reads_png_transparency_from_a_trns_chunk() {
        // Colour types 0, 2, and 3 have no alpha channel and may still be transparent: the
        // PNG specification puts that transparency in a `tRNS` chunk. Reading only IHDR
        // calls a transparent palette PNG opaque while the same picture as a GIF is not.
        let cases: &[(u8, Vec<u8>, bool)] = &[
            (0, vec![0x00, 0x01], true),
            (2, vec![0x00, 0x01, 0x00, 0x02, 0x00, 0x03], true),
            (3, vec![0x00, 0xFF], true),
            (0, Vec::new(), false),
            (2, Vec::new(), false),
            (3, Vec::new(), false),
        ];

        for (color_type, trns, expected) in cases {
            let chunks: Vec<(&[u8; 4], Vec<u8>)> = if trns.is_empty() {
                Vec::new()
            } else {
                vec![(b"tRNS", trns.clone())]
            };
            let artifact = sniff_artifact(RawArtifact::new(
                png_bytes_with_chunks(*color_type, &chunks),
                None,
            ))
            .expect("sniff png");

            assert_eq!(
                artifact.metadata.has_alpha,
                Some(*expected),
                "color type {color_type} with {} bytes of tRNS",
                trns.len()
            );
        }

        // A colour type that carries its own alpha channel is unaffected.
        for color_type in [4_u8, 6] {
            let artifact = sniff_artifact(RawArtifact::new(
                png_bytes_with_chunks(color_type, &[]),
                None,
            ))
            .expect("sniff png");
            assert_eq!(artifact.metadata.has_alpha, Some(true));
        }
    }

    #[test]
    fn sniff_gif_rejects_a_header_shorter_than_the_screen_descriptor() {
        let err = sniff_artifact(RawArtifact::new(b"GIF89a\x04\x00".to_vec(), None))
            .expect_err("a 9-byte gif should be rejected");

        assert!(
            matches!(err, TransformError::DecodeFailed(ref msg) if msg.contains("too short")),
            "expected a too-short decode error, got: {err}"
        );
    }

    #[test]
    fn sniff_gif_rejects_a_file_truncated_inside_a_data_block() {
        let mut bytes = gif_bytes(b"89a", 4, 4, 1, false);
        // Drop the trailer and the sub-block terminator so the walk runs off the end.
        bytes.truncate(bytes.len() - 2);
        let err = sniff_artifact(RawArtifact::new(bytes, None))
            .expect_err("a truncated gif should be rejected");

        assert!(
            matches!(err, TransformError::DecodeFailed(ref msg) if msg.contains("ends inside a data block")),
            "expected a truncated-block decode error, got: {err}"
        );
    }

    #[test]
    fn sniff_gif_rejects_a_file_with_no_image_data() {
        let err = sniff_artifact(RawArtifact::new(gif_bytes(b"89a", 4, 4, 0, false), None))
            .expect_err("a gif with no frames should be rejected");

        assert!(
            matches!(err, TransformError::DecodeFailed(ref msg) if msg.contains("no image data")),
            "expected a no-image-data decode error, got: {err}"
        );
    }

    #[test]
    fn sniff_gif_rejects_an_unknown_block_introducer() {
        let mut bytes = gif_bytes(b"89a", 4, 4, 1, false);
        // Replace the trailer with a byte that is neither an extension, an image
        // descriptor, nor a trailer.
        let last = bytes.len() - 1;
        bytes[last] = 0x99;
        let err = sniff_artifact(RawArtifact::new(bytes, None))
            .expect_err("an unknown block introducer should be rejected");

        assert!(
            matches!(err, TransformError::DecodeFailed(ref msg) if msg.contains("unknown block introducer")),
            "expected an unknown-introducer decode error, got: {err}"
        );
    }

    #[test]
    fn sniff_gif_skips_a_local_color_table() {
        // A frame carrying its own palette shifts every later offset. Getting the skip
        // wrong would land the walk mid-palette and report a bogus block introducer.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GIF89a");
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&[0x00, 0x00, 0x00]); // no global color table
        bytes.push(0x2C);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.push(0b1000_0001); // local color table, 4 entries (2^(1+1))
        bytes.extend_from_slice(&[0u8; 12]);
        bytes.push(0x02);
        bytes.extend_from_slice(&[0x02, 0x44, 0x01, 0x00]);
        bytes.push(0x3B);

        let artifact =
            sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff gif with local palette");
        assert_eq!(artifact.metadata.frame_count, 1);
        assert_eq!(artifact.metadata.width, Some(4));
    }

    #[test]
    fn sniff_artifact_detects_webp_vp8x_dimensions() {
        let artifact = sniff_artifact(RawArtifact::new(
            webp_vp8x_bytes(800, 600, 0b0001_0000),
            None,
        ))
        .expect("sniff webp vp8x");

        assert_eq!(artifact.media_type, MediaType::Webp);
        assert_eq!(artifact.metadata.width, Some(800));
        assert_eq!(artifact.metadata.height, Some(600));
        assert_eq!(artifact.metadata.has_alpha, Some(true));
    }

    #[test]
    fn sniff_artifact_detects_webp_vp8l_dimensions() {
        let artifact = sniff_artifact(RawArtifact::new(webp_vp8l_bytes(123, 77), None))
            .expect("sniff webp vp8l");

        assert_eq!(artifact.media_type, MediaType::Webp);
        assert_eq!(artifact.metadata.width, Some(123));
        assert_eq!(artifact.metadata.height, Some(77));
        assert_eq!(artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn sniff_artifact_reads_the_webp_vp8l_alpha_bit() {
        let artifact = sniff_artifact(RawArtifact::new(
            webp_vp8l_bytes_with_alpha(123, 77, true),
            None,
        ))
        .expect("sniff webp vp8l");

        assert_eq!(artifact.metadata.has_alpha, Some(true));
    }

    #[test]
    fn sniff_artifact_detects_avif_brand() {
        let artifact = sniff_artifact(RawArtifact::new(avif_bytes(), None)).expect("sniff avif");

        assert_eq!(artifact.media_type, MediaType::Avif);
        assert_eq!(artifact.metadata, ArtifactMetadata::default());
    }

    #[cfg(feature = "avif")]
    #[test]
    fn sniff_artifact_detects_avif_dimensions_and_alpha() {
        let artifact = sniff_artifact(RawArtifact::new(
            encoded_avif_bytes(7, 5, Rgba([10, 20, 30, 0])),
            None,
        ))
        .expect("sniff avif with alpha");

        assert_eq!(artifact.media_type, MediaType::Avif);
        assert_eq!(artifact.metadata.width, Some(7));
        assert_eq!(artifact.metadata.height, Some(5));
        assert_eq!(artifact.metadata.has_alpha, Some(true));
    }

    #[cfg(feature = "avif")]
    #[test]
    fn sniff_artifact_detects_opaque_avif_without_alpha_item() {
        let artifact = sniff_artifact(RawArtifact::new(
            encoded_avif_bytes(9, 4, Rgba([10, 20, 30, 255])),
            None,
        ))
        .expect("sniff opaque avif");

        assert_eq!(artifact.media_type, MediaType::Avif);
        assert_eq!(artifact.metadata.width, Some(9));
        assert_eq!(artifact.metadata.height, Some(4));
        assert_eq!(artifact.metadata.has_alpha, Some(false));
    }

    fn mp4_box(box_type: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            &u32::try_from(payload.len() + 8)
                .expect("box size")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(box_type);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn mp4_full_box(box_type: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
        let mut body = vec![version];
        body.extend_from_slice(&flags.to_be_bytes()[1..]);
        body.extend_from_slice(payload);
        mp4_box(box_type, &body)
    }

    fn avif_ispe(width: u32, height: u32) -> Vec<u8> {
        let mut payload = width.to_be_bytes().to_vec();
        payload.extend_from_slice(&height.to_be_bytes());
        mp4_full_box(b"ispe", 0, 0, &payload)
    }

    /// An `ipma` box in the given encoding: version 1 widens item ids to 32 bits, and flag
    /// bit 0 widens property positions to 15 bits.
    fn avif_ipma(version: u8, flags: u32, associations: &[(u32, &[u16])]) -> Vec<u8> {
        let mut payload = u32::try_from(associations.len())
            .expect("entry count")
            .to_be_bytes()
            .to_vec();
        for (item, positions) in associations {
            if version == 0 {
                payload.extend_from_slice(&u16::try_from(*item).expect("item id").to_be_bytes());
            } else {
                payload.extend_from_slice(&item.to_be_bytes());
            }
            payload.push(u8::try_from(positions.len()).expect("association count"));
            for position in *positions {
                if flags & 1 == 1 {
                    payload.extend_from_slice(&position.to_be_bytes());
                } else {
                    payload.push(u8::try_from(*position).expect("narrow position"));
                }
            }
        }
        mp4_full_box(b"ipma", version, flags, &payload)
    }

    /// A structurally complete AVIF with no coded picture: the sniffer reads the item
    /// properties and never the payload, so none is needed to ask it about orientation.
    fn avif_bytes_with_properties(
        primary_item: u32,
        properties: &[Vec<u8>],
        ipma: Vec<u8>,
    ) -> Vec<u8> {
        let pitm = mp4_full_box(
            b"pitm",
            0,
            0,
            &u16::try_from(primary_item).expect("item id").to_be_bytes(),
        );
        let ipco = mp4_box(b"ipco", &properties.concat());
        let iprp = mp4_box(b"iprp", &[ipco, ipma].concat());
        let meta = mp4_full_box(b"meta", 0, 0, &[pitm, iprp].concat());
        let mut bytes = avif_bytes();
        bytes.extend_from_slice(&meta);
        bytes
    }

    fn avif_bytes_with_transforms(rotation: Option<u8>, mirror: Option<u8>) -> Vec<u8> {
        let mut properties = vec![avif_ispe(40, 20)];
        let mut positions = vec![1_u16];
        if let Some(angle) = rotation {
            properties.push(mp4_box(b"irot", &[angle]));
            positions.push(u16::try_from(properties.len()).expect("position"));
        }
        if let Some(mode) = mirror {
            properties.push(mp4_box(b"imir", &[mode]));
            positions.push(u16::try_from(properties.len()).expect("position"));
        }
        avif_bytes_with_properties(1, &properties, avif_ipma(0, 0, &[(1, &positions)]))
    }

    /// Every combination of the two properties, against the table Chrome and Firefox use.
    /// The rotation is applied before the mirror, which is what tells 5 from 7.
    #[rstest]
    #[case(None, None, None)]
    #[case(Some(0), None, Some(1))]
    #[case(Some(1), None, Some(8))]
    #[case(Some(2), None, Some(3))]
    #[case(Some(3), None, Some(6))]
    #[case(None, Some(0), Some(4))]
    #[case(None, Some(1), Some(2))]
    #[case(Some(1), Some(0), Some(5))]
    #[case(Some(1), Some(1), Some(7))]
    #[case(Some(2), Some(0), Some(2))]
    #[case(Some(2), Some(1), Some(4))]
    #[case(Some(3), Some(0), Some(7))]
    #[case(Some(3), Some(1), Some(5))]
    fn sniff_artifact_folds_avif_irot_and_imir_into_an_orientation(
        #[case] rotation: Option<u8>,
        #[case] mirror: Option<u8>,
        #[case] expected: Option<u16>,
    ) {
        let bytes = avif_bytes_with_transforms(rotation, mirror);
        let artifact = sniff_artifact(RawArtifact::new(bytes.clone(), None)).expect("sniff avif");

        assert_eq!(
            artifact.metadata.orientation, expected,
            "irot {rotation:?}, imir {mirror:?}"
        );
        assert_eq!(
            (artifact.metadata.width, artifact.metadata.height),
            (Some(40), Some(20)),
            "the dimensions are still read from the same property container"
        );
        assert_eq!(
            exif_orientation(MediaType::Avif, &bytes),
            expected,
            "the pipeline reads what the sniffer reports"
        );
    }

    /// The properties of another item — an alpha plane with its own `irot` — say nothing
    /// about the primary picture.
    #[test]
    fn sniff_artifact_ignores_avif_transforms_on_other_items() {
        let properties = vec![avif_ispe(40, 20), mp4_box(b"irot", &[3])];
        let bytes =
            avif_bytes_with_properties(1, &properties, avif_ipma(0, 0, &[(1, &[1]), (2, &[1, 2])]));

        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff avif");

        assert_eq!(artifact.metadata.orientation, None);
    }

    /// `ipma` has two encodings for ids and two for positions, and encoders use both.
    #[test]
    fn sniff_artifact_reads_avif_associations_in_the_wide_ipma_encoding() {
        let properties = vec![avif_ispe(40, 20), mp4_box(b"irot", &[3])];
        let bytes = avif_bytes_with_properties(1, &properties, avif_ipma(1, 1, &[(1, &[1, 2])]));

        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff avif");

        assert_eq!(artifact.metadata.orientation, Some(6));
    }

    /// The order the file lists the two properties in does not change the answer: MIAF
    /// fixes the rotation before the mirror.
    #[test]
    fn sniff_artifact_applies_avif_rotation_before_mirror_whatever_the_listed_order() {
        let properties = vec![
            avif_ispe(40, 20),
            mp4_box(b"imir", &[1]),
            mp4_box(b"irot", &[3]),
        ];
        let bytes = avif_bytes_with_properties(1, &properties, avif_ipma(0, 0, &[(1, &[3, 2, 1])]));

        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff avif");

        assert_eq!(artifact.metadata.orientation, Some(5));
    }

    /// An `ipma` that promises more entries than it holds is refused, not read past.
    #[test]
    fn sniff_artifact_rejects_a_truncated_avif_ipma() {
        let ipma = mp4_full_box(b"ipma", 0, 0, &5_u32.to_be_bytes());
        let bytes = avif_bytes_with_properties(1, &[avif_ispe(4, 4)], ipma);

        let error = sniff_artifact(RawArtifact::new(bytes, None)).expect_err("truncated ipma");

        assert!(
            error.to_string().contains("ipma box is too short"),
            "{error}"
        );
    }

    fn avif_clap(width: u32, height: u32, horizontal: i32, vertical: i32) -> Vec<u8> {
        let mut payload = Vec::new();
        for value in [width, 1, height, 1] {
            payload.extend_from_slice(&value.to_be_bytes());
        }
        for offset in [horizontal, vertical] {
            payload.extend_from_slice(&offset.to_be_bytes());
            payload.extend_from_slice(&1_u32.to_be_bytes());
        }
        mp4_box(b"clap", &payload)
    }

    /// The clean aperture is the picture, so the sniffer reports its size, and it is cut
    /// before the orientation turns it, so the oriented size follows from the cut.
    #[rstest]
    #[case::centred(30, 20, 0, 0, None, (30, 20), (30, 20))]
    #[case::offset_to_the_left(30, 20, -5, 0, None, (30, 20), (30, 20))]
    #[case::then_rotated(30, 20, 0, 0, Some(3), (30, 20), (20, 30))]
    #[case::whole_picture(40, 20, 0, 0, None, (40, 20), (40, 20))]
    fn sniff_artifact_reports_the_avif_clean_aperture_as_the_picture(
        #[case] width: u32,
        #[case] height: u32,
        #[case] horizontal: i32,
        #[case] vertical: i32,
        #[case] rotation: Option<u8>,
        #[case] expected: (u32, u32),
        #[case] expected_oriented: (u32, u32),
    ) {
        let mut properties = vec![
            avif_ispe(40, 20),
            avif_clap(width, height, horizontal, vertical),
        ];
        let mut positions = vec![1_u16, 2];
        if let Some(angle) = rotation {
            properties.push(mp4_box(b"irot", &[angle]));
            positions.push(3);
        }
        let bytes = avif_bytes_with_properties(1, &properties, avif_ipma(0, 0, &[(1, &positions)]));

        let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff avif");

        assert_eq!(
            (artifact.metadata.width, artifact.metadata.height),
            (Some(expected.0), Some(expected.1))
        );
        assert_eq!(
            artifact.metadata.oriented_dimensions(),
            Some(Dimensions::new(expected_oriented.0, expected_oriented.1))
        );
    }

    /// An aperture that does not land on whole pixels or does not fit is refused, not
    /// rounded: MIAF requires whole pixels for an AV1 image, and a viewer that rounds shows
    /// a different picture from one that does not.
    #[rstest]
    #[case::off_the_pixel_grid(31, 20, 0, 0, "does not land on a whole pixel")]
    #[case::wider_than_the_picture(50, 20, 0, 0, "larger than the 40-pixel picture")]
    #[case::pushed_out_of_the_picture(30, 20, 6, 0, "leaves the picture")]
    fn sniff_artifact_refuses_an_avif_clean_aperture_that_is_not_a_pixel_rectangle(
        #[case] width: u32,
        #[case] height: u32,
        #[case] horizontal: i32,
        #[case] vertical: i32,
        #[case] reason: &str,
    ) {
        let properties = vec![
            avif_ispe(40, 20),
            avif_clap(width, height, horizontal, vertical),
        ];
        let bytes = avif_bytes_with_properties(1, &properties, avif_ipma(0, 0, &[(1, &[1, 2])]));

        let error = sniff_artifact(RawArtifact::new(bytes, None)).expect_err("refused");

        assert!(error.to_string().contains(reason), "{error}");
    }

    /// Two files patched from what libheif wrote, since no encoder here writes the box: a
    /// 40x20 picture with a centred 30x20 aperture, and the same aperture on a rotated one.
    #[test]
    fn sniff_artifact_reads_the_clean_aperture_of_a_patched_avif() {
        let cropped = include_bytes!("../integration/fixtures/clap-cropped.avif");
        let rotated = include_bytes!("../integration/fixtures/clap-rotated.avif");

        let cropped = sniff_artifact(RawArtifact::new(cropped.to_vec(), None)).expect("sniff");
        assert_eq!(
            (cropped.metadata.width, cropped.metadata.height),
            (Some(30), Some(20))
        );
        assert_eq!(cropped.metadata.orientation, None);

        let rotated = sniff_artifact(RawArtifact::new(rotated.to_vec(), None)).expect("sniff");
        assert_eq!(
            (rotated.metadata.width, rotated.metadata.height),
            (Some(30), Some(20))
        );
        assert_eq!(rotated.metadata.orientation, Some(6));
        assert_eq!(
            rotated.metadata.oriented_dimensions(),
            Some(Dimensions::new(20, 30))
        );
    }

    /// Two files ImageMagick wrote through libheif, which is the encoder behind the phones
    /// and the CMSes that produce AVIF: the transform is in the properties and there is no
    /// Exif block at all.
    #[test]
    fn sniff_artifact_reads_the_orientation_libheif_writes() {
        let rotated = include_bytes!("../integration/fixtures/irot-rotated.avif");
        let transposed = include_bytes!("../integration/fixtures/imir-transposed-5.avif");

        let rotated = sniff_artifact(RawArtifact::new(rotated.to_vec(), None)).expect("sniff");
        assert_eq!(rotated.metadata.orientation, Some(6));
        assert_eq!(
            (rotated.metadata.width, rotated.metadata.height),
            (Some(40), Some(20))
        );
        assert_eq!(
            rotated.metadata.oriented_dimensions(),
            Some(Dimensions::new(20, 40)),
            "the oriented dimensions are what convert will produce"
        );

        let transposed =
            sniff_artifact(RawArtifact::new(transposed.to_vec(), None)).expect("sniff");
        assert_eq!(transposed.metadata.orientation, Some(5));
    }

    #[test]
    fn sniff_artifact_rejects_declared_media_type_mismatch() {
        let err = sniff_artifact(RawArtifact::new(
            png_ihdr_bytes(8, 8, 2),
            Some(MediaType::Jpeg),
        ))
        .expect_err("declared mismatch should fail");

        assert_eq!(
            err,
            TransformError::InvalidInput(
                "declared media type does not match detected media type".to_string()
            )
        );
    }

    /// A number a caller typed is not yet a `u8`, so the range they are told has to be the
    /// range truss documents rather than the span of the integer it would be stored in.
    ///
    /// `--quality 255` was answered `quality must be between 1 and 100` and `--quality 256`
    /// was answered `256 is not in 0..=255`, which is two limits for one option.
    #[test]
    fn a_quality_outside_the_documented_range_reports_that_range_at_any_width() {
        for value in [0_i64, 101, 255, 256, 999_999, -1, i64::MAX, i64::MIN] {
            assert_eq!(
                validate_quality_value(value),
                Err("quality must be between 1 and 100"),
                "{value}"
            );
        }
        for value in [1_i64, 50, 100] {
            assert_eq!(validate_quality_value(value), Ok(value as u8), "{value}");
        }
    }

    /// A dimension that cannot be a number of pixels says which of the two things is wrong,
    /// and never names the integer it would be stored in.
    ///
    /// `--width 4294967296` was answered `4294967296 is not in 0..=4294967295`, the span of
    /// a `u32`, while `--quality 256` had been given the documented range in v0.20.0.
    #[test]
    fn a_dimension_that_cannot_be_a_pixel_count_says_which_half_is_wrong() {
        for value in [1_i64, 2, 100, u32::MAX as i64] {
            assert_eq!(validate_width_value(value), Ok(value as u32), "{value}");
            assert_eq!(validate_height_value(value), Ok(value as u32), "{value}");
        }
        // Zero fits, and is reported where it always was, with the class it always had.
        assert_eq!(validate_width_value(0), Ok(0));
        assert_eq!(validate_height_value(0), Ok(0));

        for value in [-1_i64, i64::MIN] {
            assert_eq!(
                validate_width_value(value),
                Err("width must be greater than zero"),
                "{value}"
            );
            assert_eq!(
                validate_height_value(value),
                Err("height must be greater than zero"),
                "{value}"
            );
        }
        for value in [u32::MAX as i64 + 1, i64::MAX] {
            assert_eq!(
                validate_width_value(value),
                Err("width is too large to be a number of pixels"),
                "{value}"
            );
            assert_eq!(
                validate_height_value(value),
                Err("height is too large to be a number of pixels"),
                "{value}"
            );
        }
    }

    /// The same for the watermark opacity, which is the other option of this shape.
    #[test]
    fn a_watermark_opacity_outside_the_documented_range_reports_that_range_at_any_width() {
        for value in [0_i64, 101, 255, 256, -1, i64::MAX] {
            assert_eq!(
                validate_watermark_opacity_value(value),
                Err("watermark opacity must be between 1 and 100"),
                "{value}"
            );
        }
        assert_eq!(validate_watermark_opacity_value(50), Ok(50));
    }

    /// An angle past a full turn wraps, which is what the flag documents, and a big one is
    /// no different from a small one.
    ///
    /// `--rotate 9999999999` was refused with `expected a whole number of degrees`, which
    /// it is; the real limit was that the value had to fit an `i32`, and nothing said so.
    #[test]
    fn a_rotation_past_a_full_turn_wraps_however_large_it_is() {
        use std::str::FromStr;
        assert_eq!(
            Rotation::from_str("9999999999").expect("a whole number of degrees"),
            Rotation::from_str("279").expect("279")
        );
        assert_eq!(
            Rotation::from_str("-9999999999").expect("a whole number of degrees"),
            Rotation::from_str("81").expect("81")
        );
        assert_eq!(
            Rotation::from_str("2147483648").expect("one past i32"),
            Rotation::from_str(&(2147483648_i64 % 360).to_string()).expect("wrapped")
        );
        // A value that is not a whole number is still refused, and says so.
        let error = Rotation::from_str("1.5").expect_err("not a whole number");
        assert!(error.contains("whole number of degrees"), "{error}");
    }

    #[test]
    fn sniff_artifact_rejects_unknown_signatures() {
        let err =
            sniff_artifact(RawArtifact::new(vec![1, 2, 3, 4], None)).expect_err("unknown bytes");

        assert!(
            matches!(err, TransformError::UnsupportedInputMediaType(ref msg) if msg.contains("unknown file signature")),
            "expected unknown file signature error, got: {err}"
        );
        let msg = err.to_string();
        assert!(msg.contains("4 bytes"), "should include file size: {msg}");
        assert!(
            !msg.contains("01 02 03 04"),
            "the bytes themselves are content, and this message reaches whoever named a URL: {msg}"
        );
    }

    /// The XML prolog is `XMLDecl? Misc* (doctypedecl Misc*)?` with
    /// `Misc ::= Comment | PI | S`, so comments and processing instructions are
    /// legal on both sides of the DOCTYPE and in any number, and a DOCTYPE may
    /// carry an internal subset whose `>` characters are not its terminator.
    /// Every shape below is a valid SVG document.
    #[rstest]
    #[case::no_prolog(r#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#)]
    #[case::declaration(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::declaration_and_doctype(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::comment_before_doctype(
        "<?xml version=\"1.0\"?>\n<!-- Generator: Adobe Illustrator 27.0.0 -->\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\">\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::doctype_with_internal_subset(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE svg [<!ENTITY a \"b\">]>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::illustrator_export(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!-- Generator: Adobe Illustrator 27.0.0, SVG Export Plug-In -->\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\" [\n\t<!ENTITY ns_extend \"http://ns.adobe.com/Extensibility/1.0/\">\n]>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::internal_subset_with_angle_bracket_in_a_string(
        "<?xml version=\"1.0\"?>\n<!DOCTYPE svg [<!ENTITY gt \"a > b\">]>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::stylesheet_processing_instruction(
        "<?xml version=\"1.0\"?>\n<?xml-stylesheet type=\"text/css\" href=\"a.css\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::processing_instruction_between_comments(
        "<?xml version=\"1.0\"?>\n<!-- one -->\n<?foo bar?>\n<!-- two -->\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::comment_on_both_sides_of_the_doctype(
        "<!-- before -->\n<!DOCTYPE svg>\n<!-- after -->\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    #[case::bom_then_declaration(
        "\u{FEFF}<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"><rect/></svg>"
    )]
    fn sniff_artifact_accepts_every_legal_svg_prolog(#[case] document: &str) {
        let artifact = sniff_artifact(RawArtifact::new(document.as_bytes().to_vec(), None))
            .unwrap_or_else(|err| panic!("prolog should be recognized as SVG, got: {err}"));
        assert_eq!(artifact.media_type, MediaType::Svg);
    }

    /// Every other format reports the dimensions its container stores, and an SVG stores
    /// them on the root element. The unit table is what stops the next spelling from
    /// silently becoming `None`: a length with no unit and one in `px` are the same number,
    /// and the absolute units are fixed ratios of it.
    #[rstest]
    #[case::bare_numbers(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"/>"#, Some((100, 50)))]
    #[case::px(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100px" height="50px"/>"#, Some((100, 50)))]
    #[case::decimal(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100.6" height="50.2"/>"#, Some((100, 50)))]
    #[case::inches(r#"<svg xmlns="http://www.w3.org/2000/svg" width="1in" height="2in"/>"#, Some((96, 192)))]
    #[case::points(r#"<svg xmlns="http://www.w3.org/2000/svg" width="72pt" height="36pt"/>"#, Some((96, 48)))]
    #[case::picas(r#"<svg xmlns="http://www.w3.org/2000/svg" width="1pc" height="2pc"/>"#, Some((16, 32)))]
    #[case::whitespace_around_the_value(r#"<svg xmlns="http://www.w3.org/2000/svg" width=" 100 " height=" 50 "/>"#, Some((100, 50)))]
    #[case::single_quoted(r"<svg xmlns='http://www.w3.org/2000/svg' width='100' height='50'/>", Some((100, 50)))]
    #[case::view_box_only(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 60"/>"#, Some((120, 60)))]
    #[case::view_box_with_commas(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0,0,120,60"/>"#, Some((120, 60)))]
    #[case::percentages_fall_back_to_the_view_box(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%" viewBox="0 0 30 20"/>"#, Some((30, 20)))]
    #[case::one_axis_takes_its_aspect_ratio_from_the_view_box(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" viewBox="0 0 30 20"/>"#, Some((100, 66)))]
    #[case::font_relative_units_are_unresolvable(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10em" height="4em"/>"#,
        None
    )]
    #[case::percentages_with_no_view_box(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" height="100%"/>"#,
        None
    )]
    #[case::nothing_declared(r#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#, None)]
    #[case::zero_is_not_a_size(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="50"/>"#,
        None
    )]
    #[case::negative_is_not_a_size(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="-100" height="50"/>"#,
        None
    )]
    #[case::malformed_view_box(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120"/>"#,
        None
    )]
    #[case::illustrator_prolog(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!-- Generator: Adobe Illustrator 27.0.0 -->\n<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd\" [\n\t<!ENTITY ns_extend \"http://ns.adobe.com/Extensibility/1.0/\">\n]>\n<svg xmlns=\"http://www.w3.org/2000/svg\" x=\"0px\" y=\"0px\" width=\"64px\" height=\"32px\" viewBox=\"0 0 64 32\"><rect/></svg>",
        Some((64, 32))
    )]
    fn sniff_artifact_reads_svg_dimensions_from_the_root_element(
        #[case] document: &str,
        #[case] expected: Option<(u32, u32)>,
    ) {
        let artifact = sniff_artifact(RawArtifact::new(document.as_bytes().to_vec(), None))
            .expect("document should be recognized as SVG");

        assert_eq!(artifact.media_type, MediaType::Svg);
        assert_eq!(
            artifact.metadata.width.zip(artifact.metadata.height),
            expected
        );
        assert_eq!(
            artifact
                .metadata
                .oriented_dimensions()
                .map(|d| (d.width, d.height)),
            expected
        );
    }

    /// Reading the prolog must not turn the sniffer into something that claims
    /// any XML document, or any document that merely starts with `<svg`.
    #[rstest]
    #[case::xhtml_root(
        "<?xml version=\"1.0\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><body/></html>"
    )]
    #[case::element_with_an_svg_prefix("<?xml version=\"1.0\"?>\n<svgfoo/>")]
    #[case::prolog_with_no_root("<?xml version=\"1.0\"?>\n<!-- only a comment -->")]
    #[case::unterminated_declaration("<?xml version=\"1.0\"\n<svg/>")]
    #[case::unterminated_comment("<!-- never closed\n<svg/>")]
    #[case::unterminated_internal_subset("<!DOCTYPE svg [<!ENTITY a \"b\">\n<svg/>")]
    fn sniff_artifact_does_not_claim_non_svg_documents(#[case] document: &str) {
        let result = sniff_artifact(RawArtifact::new(document.as_bytes().to_vec(), None));
        assert!(
            result.is_err(),
            "should not be claimed as SVG: {document:?} produced {result:?}"
        );
    }

    #[test]
    fn sniff_artifact_rejects_invalid_png_structure() {
        let err = sniff_artifact(RawArtifact::new(b"\x89PNG\r\n\x1a\nbroken".to_vec(), None))
            .expect_err("broken png should fail");

        assert_eq!(
            err,
            TransformError::DecodeFailed("png file is too short".to_string())
        );
    }

    #[test]
    fn sniff_artifact_detects_bmp_dimensions() {
        // Build a minimal BMP with BITMAPINFOHEADER (40 bytes DIB header).
        // File header: 14 bytes, DIB header: 40 bytes minimum.
        let mut bmp = Vec::new();
        // BM signature
        bmp.extend_from_slice(b"BM");
        // File size (placeholder)
        bmp.extend_from_slice(&0u32.to_le_bytes());
        // Reserved
        bmp.extend_from_slice(&0u32.to_le_bytes());
        // Pixel data offset (14 + 40 = 54)
        bmp.extend_from_slice(&54u32.to_le_bytes());
        // DIB header size (BITMAPINFOHEADER = 40)
        bmp.extend_from_slice(&40u32.to_le_bytes());
        // Width = 8
        bmp.extend_from_slice(&8u32.to_le_bytes());
        // Height = 6
        bmp.extend_from_slice(&6i32.to_le_bytes());
        // Planes = 1
        bmp.extend_from_slice(&1u16.to_le_bytes());
        // Bits per pixel = 24
        bmp.extend_from_slice(&24u16.to_le_bytes());
        // Padding to reach minimum sniff length
        bmp.resize(54, 0);

        let artifact = sniff_artifact(RawArtifact::new(bmp, None)).unwrap();
        assert_eq!(artifact.media_type, MediaType::Bmp);
        assert_eq!(artifact.metadata.width, Some(8));
        assert_eq!(artifact.metadata.height, Some(6));
        assert_eq!(artifact.metadata.has_alpha, Some(false));
    }

    #[test]
    fn sniff_artifact_detects_bmp_32bit_alpha() {
        let mut bmp = Vec::new();
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&0u32.to_le_bytes());
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        // Width = 4
        bmp.extend_from_slice(&4u32.to_le_bytes());
        // Height = 4
        bmp.extend_from_slice(&4i32.to_le_bytes());
        // Planes = 1
        bmp.extend_from_slice(&1u16.to_le_bytes());
        // Bits per pixel = 32 (has alpha)
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.resize(54, 0);

        let artifact = sniff_artifact(RawArtifact::new(bmp, None)).unwrap();
        assert_eq!(artifact.media_type, MediaType::Bmp);
        assert_eq!(artifact.metadata.has_alpha, Some(true));
    }

    #[test]
    fn sniff_artifact_rejects_too_short_bmp() {
        // "BM" + enough padding to pass is_bmp (>= 26 bytes) but not sniff_bmp (>= 30)
        let mut data = b"BM".to_vec();
        data.resize(27, 0);
        let err =
            sniff_artifact(RawArtifact::new(data, None)).expect_err("too-short BMP should fail");

        assert_eq!(
            err,
            TransformError::DecodeFailed("bmp file is too short".to_string())
        );
    }

    #[test]
    fn normalize_rejects_blur_sigma_below_minimum() {
        let err = TransformOptions {
            blur: Some(0.0),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("blur sigma 0.0 should be rejected");

        assert_eq!(
            err,
            TransformError::InvalidOptions("blur sigma must be between 0.1 and 100.0".to_string())
        );
    }

    #[test]
    fn normalize_rejects_blur_sigma_above_maximum() {
        let err = TransformOptions {
            blur: Some(100.1),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("blur sigma 100.1 should be rejected");

        assert_eq!(
            err,
            TransformError::InvalidOptions("blur sigma must be between 0.1 and 100.0".to_string())
        );
    }

    #[test]
    fn normalize_accepts_blur_sigma_at_boundaries() {
        let opts_min = TransformOptions {
            blur: Some(0.1),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("blur sigma 0.1 should be accepted");
        assert_eq!(opts_min.blur, Some(0.1));

        let opts_max = TransformOptions {
            blur: Some(100.0),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("blur sigma 100.0 should be accepted");
        assert_eq!(opts_max.blur, Some(100.0));
    }

    #[test]
    fn normalize_rejects_sharpen_sigma_below_minimum() {
        let err = TransformOptions {
            sharpen: Some(0.0),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("sharpen sigma 0.0 should be rejected");

        assert_eq!(
            err,
            TransformError::InvalidOptions(
                "sharpen sigma must be between 0.1 and 100.0".to_string()
            )
        );
    }

    #[test]
    fn normalize_rejects_sharpen_sigma_above_maximum() {
        let err = TransformOptions {
            sharpen: Some(100.1),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect_err("sharpen sigma 100.1 should be rejected");

        assert_eq!(
            err,
            TransformError::InvalidOptions(
                "sharpen sigma must be between 0.1 and 100.0".to_string()
            )
        );
    }

    #[test]
    fn normalize_accepts_sharpen_sigma_at_boundaries() {
        let opts_min = TransformOptions {
            sharpen: Some(0.1),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("sharpen sigma 0.1 should be accepted");
        assert_eq!(opts_min.sharpen, Some(0.1));

        let opts_max = TransformOptions {
            sharpen: Some(100.0),
            ..TransformOptions::default()
        }
        .normalize(MediaType::Jpeg)
        .expect("sharpen sigma 100.0 should be accepted");
        assert_eq!(opts_max.sharpen, Some(100.0));
    }

    #[test]
    fn validate_watermark_rejects_zero_opacity() {
        let wm = super::WatermarkInput {
            image: jpeg_artifact(),
            position: Position::BottomRight,
            opacity: 0,
            margin: 10,
        };
        let err = super::validate_watermark(&wm).expect_err("opacity 0 should be rejected");
        assert_eq!(
            err,
            TransformError::InvalidOptions(
                "watermark opacity must be between 1 and 100".to_string()
            )
        );
    }

    #[test]
    fn validate_watermark_rejects_opacity_above_100() {
        let wm = super::WatermarkInput {
            image: jpeg_artifact(),
            position: Position::BottomRight,
            opacity: 101,
            margin: 10,
        };
        let err = super::validate_watermark(&wm).expect_err("opacity 101 should be rejected");
        assert_eq!(
            err,
            TransformError::InvalidOptions(
                "watermark opacity must be between 1 and 100".to_string()
            )
        );
    }

    #[test]
    fn validate_watermark_rejects_svg_image() {
        let wm = super::WatermarkInput {
            image: Artifact::new(vec![1], MediaType::Svg, ArtifactMetadata::default()),
            position: Position::BottomRight,
            opacity: 50,
            margin: 10,
        };
        let err = super::validate_watermark(&wm).expect_err("SVG watermark should be rejected");
        assert_eq!(
            err,
            TransformError::InvalidOptions("watermark image must be a raster format".to_string())
        );
    }

    #[test]
    fn validate_watermark_accepts_valid_input() {
        let wm = super::WatermarkInput {
            image: jpeg_artifact(),
            position: Position::BottomRight,
            opacity: 50,
            margin: 10,
        };
        super::validate_watermark(&wm).expect("valid watermark should be accepted");
    }

    #[test]
    fn crop_region_from_str_valid() {
        use super::CropRegion;
        let crop: CropRegion = "10,20,100,200".parse().expect("valid crop");
        assert_eq!(crop.x, 10);
        assert_eq!(crop.y, 20);
        assert_eq!(crop.width, 100);
        assert_eq!(crop.height, 200);
    }

    #[test]
    fn crop_region_from_str_zero_width() {
        use super::CropRegion;
        let err = "10,20,0,200"
            .parse::<CropRegion>()
            .expect_err("zero width should fail");
        assert!(err.contains("greater than zero"), "unexpected error: {err}");
    }

    #[test]
    fn crop_region_from_str_wrong_parts() {
        use super::CropRegion;
        let err = "10,20,100"
            .parse::<CropRegion>()
            .expect_err("three parts should fail");
        assert!(
            err.contains("four comma-separated"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn crop_region_display() {
        use super::CropRegion;
        let crop = CropRegion {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        };
        assert_eq!(crop.to_string(), "1,2,3,4");
    }

    #[test]
    fn normalize_rejects_zero_dimension_crop() {
        use super::{CropRegion, MediaType, TransformOptions};
        let opts = TransformOptions {
            crop: Some(CropRegion {
                x: 0,
                y: 0,
                width: 0,
                height: 100,
            }),
            ..TransformOptions::default()
        };
        let err = opts
            .normalize(MediaType::Jpeg)
            .expect_err("zero-width crop should fail");
        assert!(
            matches!(err, super::TransformError::InvalidOptions(_)),
            "unexpected error: {err:?}"
        );
    }
}
