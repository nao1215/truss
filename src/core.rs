//! Shared Core types for transformations, validation, and media inspection.

use std::error::Error;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// Maximum number of pixels in the output image (width × height).
///
/// This limit prevents resize operations from producing excessively large
/// output buffers. The value matches the API specification in `doc/api.md`.
///
/// ```
/// assert_eq!(truss::MAX_OUTPUT_PIXELS, 67_108_864);
/// ```
pub const MAX_OUTPUT_PIXELS: u64 = 67_108_864;

/// Maximum number of decoded pixels allowed for an input image (width × height).
///
/// This limit prevents decompression bombs from consuming unbounded memory.
/// The value matches the API specification in `doc/api.md`.
///
/// ```
/// assert_eq!(truss::MAX_DECODED_PIXELS, 100_000_000);
/// ```
pub const MAX_DECODED_PIXELS: u64 = 100_000_000;

/// Raw input bytes before media-type detection has completed.
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl Default for ArtifactMetadata {
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            frame_count: 1,
            duration: None,
            has_alpha: None,
        }
    }
}

/// Supported media types for the current implementation phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    /// JPEG image data.
    Jpeg,
    /// PNG image data.
    Png,
    /// WebP image data.
    Webp,
    /// AVIF image data.
    Avif,
}

impl MediaType {
    /// Returns the canonical media type name used by the API and CLI.
    pub const fn as_name(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Webp => "webp",
            Self::Avif => "avif",
        }
    }

    /// Returns the canonical MIME type string.
    pub const fn as_mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
            Self::Avif => "image/avif",
        }
    }

    /// Reports whether the media type is typically encoded with lossy quality controls.
    pub const fn is_lossy(self) -> bool {
        matches!(self, Self::Jpeg | Self::Webp | Self::Avif)
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
            _ => Err(format!("unsupported media type `{value}`")),
        }
    }
}

/// A complete transform request for the Core layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformRequest {
    /// The already-resolved input artifact.
    pub input: Artifact,
    /// Raw transform options as provided by an adapter.
    pub options: TransformOptions,
}

impl TransformRequest {
    /// Creates a new transform request.
    pub fn new(input: Artifact, options: TransformOptions) -> Self {
        Self { input, options }
    }

    /// Normalizes the request into a form that does not require adapter-specific defaults.
    pub fn normalize(self) -> Result<NormalizedTransformRequest, TransformError> {
        let options = self.options.normalize(self.input.media_type)?;

        Ok(NormalizedTransformRequest {
            input: self.input,
            options,
        })
    }
}

/// A fully normalized transform request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTransformRequest {
    /// The normalized input artifact.
    pub input: Artifact,
    /// Fully normalized transform options.
    pub options: NormalizedTransformOptions,
}

/// Raw transform options before defaulting and validation has completed.
#[derive(Debug, Clone, PartialEq, Eq)]
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
            background: None,
            rotate: Rotation::Deg0,
            auto_orient: true,
            strip_metadata: true,
            preserve_exif: false,
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

        if self.preserve_exif && self.strip_metadata {
            return Err(TransformError::InvalidOptions(
                "preserve_exif requires strip_metadata to be false".to_string(),
            ));
        }

        let format = self.format.unwrap_or(input_media_type);

        if self.quality.is_some() && !format.is_lossy() {
            return Err(TransformError::InvalidOptions(
                "quality requires a lossy output format".to_string(),
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
            background: self.background,
            rotate: self.rotate,
            auto_orient: self.auto_orient,
            metadata_policy: normalize_metadata_policy(self.strip_metadata, self.preserve_exif),
        })
    }
}

/// Fully normalized transform options ready for a backend pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// The requested background color.
    pub background: Option<Rgba8>,
    /// The requested extra rotation.
    pub rotate: Rotation,
    /// Whether EXIF-based auto-orientation should run.
    pub auto_orient: bool,
    /// The normalized metadata handling strategy.
    pub metadata_policy: MetadataPolicy,
}

/// Resize behavior for bounded transforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Scale to fit within the box while preserving aspect ratio.
    Contain,
    /// Scale to cover the box while preserving aspect ratio.
    Cover,
    /// Stretch to fill the box.
    Fill,
    /// Scale down only when the input is larger than the box.
    Inside,
}

impl Fit {
    /// Returns the canonical option name used by the API and CLI.
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

/// Rotation that is applied after auto-orientation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    /// No extra rotation.
    Deg0,
    /// Rotate 90 degrees clockwise.
    Deg90,
    /// Rotate 180 degrees.
    Deg180,
    /// Rotate 270 degrees clockwise.
    Deg270,
}

impl Rotation {
    /// Returns the canonical degree value used by the API and CLI.
    pub const fn as_degrees(self) -> u16 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 90,
            Self::Deg180 => 180,
            Self::Deg270 => 270,
        }
    }
}

impl FromStr for Rotation {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "0" => Ok(Self::Deg0),
            "90" => Ok(Self::Deg90),
            "180" => Ok(Self::Deg180),
            "270" => Ok(Self::Deg270),
            _ => Err(format!("unsupported rotation `{value}`")),
        }
    }
}

/// A simple 8-bit RGBA color.
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
        if value.len() != 6 && value.len() != 8 {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataPolicy {
    /// Drop metadata from the output.
    StripAll,
    /// Keep metadata unchanged when possible.
    KeepAll,
    /// Preserve EXIF while allowing other metadata policies later.
    PreserveExif,
}

/// Errors returned by Core validation or backend execution.
#[derive(Debug, Clone, PartialEq, Eq)]
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
            Self::UnsupportedOutputMediaType(media_type) => {
                write!(f, "unsupported output media type: {media_type}")
            }
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
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataKind {
    /// XMP (Extensible Metadata Platform) metadata.
    Xmp,
    /// IPTC/IIM (International Press Telecommunications Council) metadata.
    Iptc,
}

impl fmt::Display for MetadataKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xmp => f.write_str("XMP"),
            Self::Iptc => f.write_str("IPTC"),
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
pub enum TransformWarning {
    /// Metadata of the given kind was present in the input but could not be preserved
    /// by the output encoder and was silently dropped.
    MetadataDropped(MetadataKind),
}

impl fmt::Display for TransformWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MetadataDropped(kind) => write!(
                f,
                "{kind} metadata was present in the input but could not be preserved by the output encoder"
            ),
        }
    }
}

/// The result of a successful transform, containing the output artifact and any warnings.
///
/// Warnings indicate aspects of the request that could not be fully honored, such as
/// metadata types that were silently dropped because the output encoder does not support them.
#[derive(Debug)]
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
/// Detection currently supports JPEG, PNG, WebP, and AVIF recognition.
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
/// ```
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
pub fn sniff_artifact(input: RawArtifact) -> Result<Artifact, TransformError> {
    let (media_type, metadata) = detect_artifact(&input.bytes)?;

    if let Some(declared_media_type) = input.declared_media_type {
        if declared_media_type != media_type {
            return Err(TransformError::InvalidInput(
                "declared media type does not match detected media type".to_string(),
            ));
        }
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

fn normalize_metadata_policy(strip_metadata: bool, preserve_exif: bool) -> MetadataPolicy {
    if preserve_exif {
        MetadataPolicy::PreserveExif
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

    Err(TransformError::UnsupportedInputMediaType(
        "unknown file signature".to_string(),
    ))
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
    })
}

fn sniff_jpeg(bytes: &[u8]) -> Result<ArtifactMetadata, TransformError> {
    let mut offset = 2;

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

    let bits = read_u32_le(&bytes[1..5])?;
    let width = (bits & 0x3FFF) + 1;
    let height = ((bits >> 14) & 0x3FFF) + 1;

    Ok(ArtifactMetadata {
        width: Some(width),
        height: Some(height),
        frame_count: 1,
        duration: None,
        has_alpha: None,
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

fn parse_mp4_box(
    bytes: &[u8],
    offset: usize,
) -> Result<(&[u8; 4], &[u8], usize), TransformError> {
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
        sniff_artifact, Artifact, ArtifactMetadata, Fit, MediaType, MetadataPolicy, Position,
        RawArtifact, Rgba8, Rotation, TransformError, TransformOptions, TransformRequest,
    };
    use image::codecs::avif::AvifEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};

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
        let packed = (width - 1) | ((height - 1) << 14);
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
        assert_eq!(options.rotate, Rotation::Deg0);
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
        assert!("gif".parse::<MediaType>().is_err());
    }

    #[test]
    fn fit_position_rotation_and_color_parsing_work() {
        assert_eq!("cover".parse::<Fit>(), Ok(Fit::Cover));
        assert_eq!(
            "bottom-right".parse::<Position>(),
            Ok(Position::BottomRight)
        );
        assert_eq!("270".parse::<Rotation>(), Ok(Rotation::Deg270));
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
                "preserve_exif requires strip_metadata to be false".to_string()
            )
        );
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
    }

    #[test]
    fn sniff_artifact_detects_avif_brand() {
        let artifact = sniff_artifact(RawArtifact::new(avif_bytes(), None)).expect("sniff avif");

        assert_eq!(artifact.media_type, MediaType::Avif);
        assert_eq!(artifact.metadata, ArtifactMetadata::default());
    }

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

        assert_eq!(
            err,
            TransformError::UnsupportedInputMediaType("unknown file signature".to_string())
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
}
