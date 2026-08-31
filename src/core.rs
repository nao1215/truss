//! Shared Core types for transformations, validation, and media inspection.

use std::error::Error;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

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
        let metric = QualityMetric::from_str(&metric.to_ascii_lowercase())?;
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
    /// Normalizes and validates the options against the input media type.
    pub fn normalize(
        self,
        input_media_type: MediaType,
    ) -> Result<NormalizedTransformOptions, TransformError> {
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
        match value.parse::<i32>() {
            Ok(degrees) => Ok(Self::from_degrees(degrees)),
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
    pub fn from_hex(value: &str) -> Result<Self, String> {
        if !value.is_ascii() || (value.len() != 6 && value.len() != 8) {
            return Err(format!("unsupported color `{value}`"));
        }

        let r = u8::from_str_radix(&value[0..2], 16)
            .map_err(|_| format!("unsupported color `{value}`"))?;
        let g = u8::from_str_radix(&value[2..4], 16)
            .map_err(|_| format!("unsupported color `{value}`"))?;
        let b = u8::from_str_radix(&value[4..6], 16)
            .map_err(|_| format!("unsupported color `{value}`"))?;
        let a = if value.len() == 8 {
            u8::from_str_radix(&value[6..8], 16)
                .map_err(|_| format!("unsupported color `{value}`"))?
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    if matches!(value, Some(0) | Some(101..=u8::MAX)) {
        return Err(TransformError::InvalidOptions(
            "quality must be between 1 and 100".to_string(),
        ));
    }

    Ok(())
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

fn validate_watermark(wm: &WatermarkInput) -> Result<(), TransformError> {
    if wm.opacity == 0 || wm.opacity > 100 {
        return Err(TransformError::InvalidOptions(
            "watermark opacity must be between 1 and 100".to_string(),
        ));
    }

    if !wm.image.media_type.is_raster() {
        return Err(TransformError::InvalidOptions(
            "watermark image must be a raster format".to_string(),
        ));
    }

    Ok(())
}

/// Resolves the metadata flags into the policy the pipeline applies.
///
/// A lossy re-encode of a profile-tagged image renders with the wrong colors if the profile
/// is dropped, so a strip request is upgraded to "keep the ICC profile only" for lossy output.
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
    } else if strip_metadata && optimize == OptimizeMode::Lossy && format.supports_icc_profile() {
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

    let preview_len = bytes.len().min(16);
    let hex_preview: String = bytes[..preview_len]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    Err(TransformError::UnsupportedInputMediaType(format!(
        "unknown file signature ({} bytes, header: [{hex_preview}])",
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
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };

    // Skip UTF-8 BOM if present.
    let mut remaining = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut seen_doctype = false;

    loop {
        remaining = remaining.trim_start();

        if let Some(rest) = remaining.strip_prefix("<!--") {
            let Some(end) = rest.find("-->") else {
                return false;
            };
            remaining = &rest[end + 3..];
            continue;
        }

        // Any processing instruction, including the XML declaration, which is
        // just the one whose target is `xml`.
        if let Some(rest) = remaining.strip_prefix("<?") {
            let Some(end) = rest.find("?>") else {
                return false;
            };
            remaining = &rest[end + 2..];
            continue;
        }

        if !seen_doctype && let Some(rest) = remaining.strip_prefix("<!DOCTYPE") {
            let Some(after) = skip_doctype(rest) else {
                return false;
            };
            seen_doctype = true;
            remaining = after;
            continue;
        }

        break;
    }

    remaining.starts_with("<svg")
        && remaining
            .as_bytes()
            .get(4)
            .is_some_and(|&b| b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'>')
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

/// Extracts basic SVG metadata. SVGs inherently support transparency.
/// Width and height are left unknown because SVGs may define dimensions via
/// `viewBox`, percentage-based attributes, or not at all.
fn sniff_svg(_bytes: &[u8]) -> ArtifactMetadata {
    ArtifactMetadata {
        width: None,
        height: None,
        frame_count: 1,
        duration: None,
        has_alpha: Some(true),
        orientation: None,
    }
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
        orientation: None,
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
    let has_alpha = match color_type {
        4 | 6 => Some(true),
        0 | 2 | 3 => Some(false),
        _ => None,
    };

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: 1,
        duration: None,
        has_alpha,
        orientation: None,
    })
}

/// Reads the EXIF Orientation tag out of a JPEG, when it has one.
///
/// The transform pipeline reads the tag through this same function, so what `inspect`
/// reports and what `convert` applies cannot drift apart. A file with no EXIF block, no
/// Orientation field, or an unreadable one reports `None`, which means no transform.
///
/// The APP1 segment is located by walking the marker headers, which is what keeps a JPEG
/// without EXIF — the common case for `sniff_artifact` — from paying for a full container
/// scan on every call.
pub(crate) fn jpeg_exif_orientation(bytes: &[u8]) -> Option<u16> {
    exif_orientation_from_payload(jpeg_exif_payload(bytes)?)
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

        match chunk_tag {
            b"VP8X" => return sniff_webp_vp8x(chunk_data),
            b"VP8 " => return sniff_webp_vp8(chunk_data),
            b"VP8L" => return sniff_webp_vp8l(chunk_data),
            _ => {}
        }

        offset = chunk_end + (chunk_size % 2);
    }

    Err(TransformError::DecodeFailed(
        "webp file is missing an image chunk".to_string(),
    ))
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
    let has_alpha = Some(flags & 0b0001_0000 != 0);

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: 1,
        duration: None,
        has_alpha,
        orientation: None,
    })
}

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

fn sniff_avif(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    if bytes.len() < 16 {
        return Err(TransformError::DecodeFailed(
            "avif file is too short".to_string(),
        ));
    }

    if !has_avif_brand(&bytes[8..]) {
        return Err(TransformError::DecodeFailed(
            "avif file is missing a compatible AVIF brand".to_string(),
        ));
    }

    let inspection = inspect_avif_container(bytes)?;

    Ok(ArtifactMetadata {
        width: inspection.dimensions.map(|(width, _)| width),
        height: inspection.dimensions.map(|(_, height)| height),
        frame_count: 1,
        duration: None,
        has_alpha: inspection.has_alpha(),
        orientation: None,
    })
}

fn has_avif_brand(bytes: &[u8]) -> bool {
    if bytes.len() < 8 {
        return false;
    }

    if is_avif_brand(&bytes[0..4]) {
        return true;
    }

    let mut offset = 8;
    while offset + 4 <= bytes.len() {
        if is_avif_brand(&bytes[offset..offset + 4]) {
            return true;
        }
        offset += 4;
    }

    false
}

fn is_avif_brand(bytes: &[u8]) -> bool {
    matches!(bytes, b"avif" | b"avis")
}

const AVIF_ALPHA_AUX_TYPE: &[u8] = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha";

#[derive(Debug, Default)]
struct AvifInspection {
    dimensions: Option<(u32, u32)>,
    saw_structured_meta: bool,
    found_alpha_item: bool,
}

impl AvifInspection {
    fn has_alpha(&self) -> Option<bool> {
        if self.saw_structured_meta {
            Some(self.found_alpha_item)
        } else {
            None
        }
    }
}

fn inspect_avif_container(bytes: &[u8]) -> Result<AvifInspection, TransformError> {
    let mut inspection = AvifInspection::default();
    inspect_avif_boxes(bytes, &mut inspection)?;
    Ok(inspection)
}

fn inspect_avif_boxes(bytes: &[u8], inspection: &mut AvifInspection) -> Result<(), TransformError> {
    let mut offset = 0;

    while offset + 8 <= bytes.len() {
        let (box_type, payload, next_offset) = parse_mp4_box(bytes, offset)?;

        match box_type {
            b"meta" | b"iref" => {
                inspection.saw_structured_meta = true;
                if payload.len() < 4 {
                    return Err(TransformError::DecodeFailed(format!(
                        "{} box is too short",
                        String::from_utf8_lossy(box_type)
                    )));
                }
                inspect_avif_boxes(&payload[4..], inspection)?;
            }
            b"iprp" | b"ipco" => {
                inspection.saw_structured_meta = true;
                inspect_avif_boxes(payload, inspection)?;
            }
            b"ispe" => {
                inspection.saw_structured_meta = true;
                if inspection.dimensions.is_none() {
                    inspection.dimensions = Some(parse_avif_ispe(payload)?);
                }
            }
            b"auxC" => {
                inspection.saw_structured_meta = true;
                if avif_auxc_declares_alpha(payload)? {
                    inspection.found_alpha_item = true;
                }
            }
            b"auxl" => {
                inspection.saw_structured_meta = true;
                inspection.found_alpha_item = true;
            }
            _ => {}
        }

        offset = next_offset;
    }

    if offset != bytes.len() {
        return Err(TransformError::DecodeFailed(
            "avif box payload has trailing bytes".to_string(),
        ));
    }

    Ok(())
}

fn parse_mp4_box(bytes: &[u8], offset: usize) -> Result<(&[u8; 4], &[u8], usize), TransformError> {
    if offset + 8 > bytes.len() {
        return Err(TransformError::DecodeFailed(
            "mp4 box header is truncated".to_string(),
        ));
    }

    let size = read_u32_be(&bytes[offset..offset + 4])?;
    let box_type = bytes[offset + 4..offset + 8]
        .try_into()
        .map_err(|_| TransformError::DecodeFailed("expected 4-byte box type".to_string()))?;
    let mut header_len = 8_usize;
    let end = match size {
        0 => bytes.len(),
        1 => {
            if offset + 16 > bytes.len() {
                return Err(TransformError::DecodeFailed(
                    "extended mp4 box header is truncated".to_string(),
                ));
            }
            header_len = 16;
            let extended_size = read_u64_be(&bytes[offset + 8..offset + 16])?;
            usize::try_from(extended_size)
                .map_err(|_| TransformError::DecodeFailed("mp4 box is too large".to_string()))?
        }
        _ => size as usize,
    };

    if end < header_len {
        return Err(TransformError::DecodeFailed(
            "mp4 box size is smaller than its header".to_string(),
        ));
    }

    let box_end = offset
        .checked_add(end)
        .ok_or_else(|| TransformError::DecodeFailed("mp4 box is too large".to_string()))?;
    if box_end > bytes.len() {
        return Err(TransformError::DecodeFailed(
            "mp4 box exceeds file length".to_string(),
        ));
    }

    Ok((box_type, &bytes[offset + header_len..box_end], box_end))
}

fn parse_avif_ispe(bytes: &[u8]) -> Result<(u32, u32), TransformError> {
    if bytes.len() < 12 {
        return Err(TransformError::DecodeFailed(
            "avif ispe box is too short".to_string(),
        ));
    }

    let width = read_u32_be(&bytes[4..8])?;
    let height = read_u32_be(&bytes[8..12])?;
    Ok((width, height))
}

fn avif_auxc_declares_alpha(bytes: &[u8]) -> Result<bool, TransformError> {
    if bytes.len() < 5 {
        return Err(TransformError::DecodeFailed(
            "avif auxC box is too short".to_string(),
        ));
    }

    let urn = &bytes[4..];
    Ok(urn
        .strip_suffix(&[0])
        .is_some_and(|urn| urn == AVIF_ALPHA_AUX_TYPE))
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
    use super::{
        Artifact, ArtifactMetadata, Fit, MediaType, MetadataPolicy, OptimizeMode, Position,
        QualityMetric, RawArtifact, Rgba8, Rotation, TargetQuality, TransformError,
        TransformOptions, TransformRequest, sniff_artifact,
    };
    #[cfg(feature = "avif")]
    use image::codecs::avif::AvifEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    use rstest::rstest;

    fn jpeg_artifact() -> Artifact {
        Artifact::new(vec![1, 2, 3], MediaType::Jpeg, ArtifactMetadata::default())
    }

    fn png_bytes(width: u32, height: u32, color_type: u8) -> Vec<u8> {
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
            sniff_artifact(RawArtifact::new(png_bytes(64, 32, 6), None)).expect("sniff png");

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

    #[test]
    fn sniff_artifact_rejects_declared_media_type_mismatch() {
        let err = sniff_artifact(RawArtifact::new(png_bytes(8, 8, 2), Some(MediaType::Jpeg)))
            .expect_err("declared mismatch should fail");

        assert_eq!(
            err,
            TransformError::InvalidInput(
                "declared media type does not match detected media type".to_string()
            )
        );
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
            msg.contains("01 02 03 04"),
            "should include hex preview: {msg}"
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
