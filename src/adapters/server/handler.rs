/// Request handler implementations (transform, health, metrics, public, upload).
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
use super::config::StorageBackend;
use super::config::{ServerConfig, StorageBackendLabel};

use super::auth::{
    authorize_request, authorize_signed_request, parse_optional_bool_query,
    parse_optional_float_query, parse_optional_integer_query, parse_optional_u8_query,
    parse_query_params, required_query_param, validate_public_query_names,
};
use super::cache::{
    TransformCache, compute_cache_key, compute_watermark_identity, try_versioned_cache_lookup,
};
use super::http_parse::{
    HttpRequest, parse_named, parse_optional_named, request_has_json_content_type,
};
use super::metrics::{
    CACHE_HITS_TOTAL, CACHE_MISSES_TOTAL, record_storage_duration, record_transform_duration,
    record_transform_error, record_watermark_transform, render_metrics_text,
    storage_backend_index_from_config, uptime_seconds,
};
use super::multipart::{parse_multipart_boundary, parse_upload_request};
use super::negotiate::{
    CacheHitStatus, ImageResponsePolicy, PublicSourceKind, build_image_etag,
    build_image_response_headers, if_none_match_matches, negotiate_output_format,
};
use super::remote::{read_remote_watermark_bytes, resolve_source_bytes};
use super::response::{
    HttpResponse, NOT_FOUND_BODY, bad_request_response, push_warning_headers,
    service_unavailable_response, transform_error_response, unsupported_media_type_response,
    unsupported_output_media_type_response, warning_header_value,
};
use super::stderr_write;

use crate::{
    CropRegion, Fit, MediaType, OptimizeMode, Position, RawArtifact, Rgba8, Rotation,
    TargetQuality, TransformOptions, TransformRequest, WatermarkInput, sniff_artifact, transform,
};
use std::str::FromStr;

#[derive(Clone, Copy)]
pub(super) struct PublicCacheControl {
    pub(super) max_age: u32,
    pub(super) stale_while_revalidate: u32,
}

#[derive(Clone, Copy)]
pub(super) struct ImageResponseConfig {
    pub(super) disable_accept_negotiation: bool,
    pub(super) public_cache_control: PublicCacheControl,
    pub(super) transform_deadline: Duration,
}

/// RAII guard that holds a concurrency slot for an in-flight image transform.
///
/// The counter is incremented on successful acquisition and decremented when
/// the guard is dropped, ensuring the slot is always released even if the
/// caller returns early or panics.
pub(super) struct TransformSlot {
    counter: Arc<AtomicU64>,
}

impl TransformSlot {
    pub(super) fn try_acquire(counter: &Arc<AtomicU64>, limit: u64) -> Option<Self> {
        let prev = counter.fetch_add(1, Ordering::Relaxed);
        if prev >= limit {
            counter.fetch_sub(1, Ordering::Relaxed);
            None
        } else {
            Some(Self {
                counter: Arc::clone(counter),
            })
        }
    }
}

impl Drop for TransformSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransformImageRequestPayload {
    pub(super) source: TransformSourcePayload,
    #[serde(default)]
    pub(super) options: TransformOptionsPayload,
    #[serde(default)]
    pub(super) watermark: Option<WatermarkPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(super) enum TransformSourcePayload {
    Path {
        path: String,
        version: Option<String>,
    },
    Url {
        url: String,
        version: Option<String>,
    },
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    Storage {
        bucket: Option<String>,
        key: String,
        version: Option<String>,
    },
}

/// Appends one length-prefixed field to a cache identifier.
///
/// A separator that can occur inside a field is not a separator. Joining the fields with a
/// newline made `("a.png\nv1", "x")` and `("a.png", "v1\nx")` the same identifier, so the
/// second request was served the first one's bytes from a cache that reported a hit. Object
/// keys containing a newline are legal on a filesystem and on S3, GCS, and Azure Blob alike.
fn push_identifier_field(id: &mut String, field: &str) {
    id.push_str(&field.len().to_string());
    id.push(':');
    id.push_str(field);
}

impl TransformSourcePayload {
    /// Computes a stable source hash from the reference and version, avoiding the
    /// need to read the full source bytes when a version tag is present. Returns
    /// `None` when no version is available, in which case the caller must fall back
    /// to the content-hash approach.
    /// Computes a stable source hash that includes the instance configuration
    /// boundaries (storage root, allow_insecure_url_sources) so that cache entries
    /// cannot be reused across instances with different security settings sharing
    /// the same cache directory.
    pub(super) fn versioned_source_hash(&self, config: &ServerConfig) -> Option<String> {
        let (kind, reference, version): (&str, std::borrow::Cow<'_, str>, Option<&str>) = match self
        {
            Self::Path { path, version } => ("path", path.as_str().into(), version.as_deref()),
            Self::Url { url, version } => ("url", url.as_str().into(), version.as_deref()),
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            Self::Storage {
                bucket,
                key,
                version,
            } => {
                let (scheme, effective_bucket) =
                    storage_scheme_and_bucket(bucket.as_deref(), config);
                let effective_bucket = effective_bucket?;
                // Length-prefixed for the same reason the outer identifier is: a key
                // containing a slash would otherwise reach across the bucket boundary.
                let mut reference = String::new();
                push_identifier_field(&mut reference, scheme);
                push_identifier_field(&mut reference, effective_bucket);
                push_identifier_field(&mut reference, key);
                ("storage", reference.into(), version.as_deref())
            }
        };
        let version = version?;
        // Every field is length-prefixed, so no value can forge a delimiter and reach into
        // the next field. Configuration boundaries are included to prevent cross-instance
        // cache poisoning.
        let mut id = String::new();
        push_identifier_field(&mut id, kind);
        push_identifier_field(&mut id, &reference);
        push_identifier_field(&mut id, version);
        push_identifier_field(&mut id, config.storage_root.to_string_lossy().as_ref());
        push_identifier_field(
            &mut id,
            if config.allow_insecure_url_sources {
                "insecure"
            } else {
                "strict"
            },
        );
        #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
        {
            push_identifier_field(&mut id, storage_backend_label(config));
            #[cfg(feature = "s3")]
            if let Some(ref ctx) = config.s3_context
                && let Some(ref endpoint) = ctx.endpoint_url
            {
                push_identifier_field(&mut id, endpoint);
            }
            #[cfg(feature = "gcs")]
            if let Some(ref ctx) = config.gcs_context
                && let Some(ref endpoint) = ctx.endpoint_url
            {
                push_identifier_field(&mut id, endpoint);
            }
            #[cfg(feature = "azure")]
            if let Some(ref ctx) = config.azure_context {
                push_identifier_field(&mut id, &ctx.endpoint_url);
            }
        }
        Some(hex::encode(Sha256::digest(id.as_bytes())))
    }

    /// Returns the storage backend label for metrics based on the source kind,
    /// rather than the server config default.  Path → Filesystem, Storage →
    /// whatever the config backend is, Url → None (no storage backend).
    pub(super) fn metrics_backend_label(
        &self,
        _config: &ServerConfig,
    ) -> Option<StorageBackendLabel> {
        match self {
            Self::Path { .. } => Some(StorageBackendLabel::Filesystem),
            Self::Url { .. } => None,
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            Self::Storage { .. } => Some(_config.storage_backend_label()),
        }
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub(super) fn storage_scheme_and_bucket<'a>(
    explicit_bucket: Option<&'a str>,
    config: &'a ServerConfig,
) -> (&'static str, Option<&'a str>) {
    match config.storage_backend {
        #[cfg(feature = "s3")]
        StorageBackend::S3 => {
            let bucket = explicit_bucket.or(config
                .s3_context
                .as_ref()
                .map(|ctx| ctx.default_bucket.as_str()));
            ("s3", bucket)
        }
        #[cfg(feature = "gcs")]
        StorageBackend::Gcs => {
            let bucket = explicit_bucket.or(config
                .gcs_context
                .as_ref()
                .map(|ctx| ctx.default_bucket.as_str()));
            ("gcs", bucket)
        }
        StorageBackend::Filesystem => ("fs", explicit_bucket),
        #[cfg(feature = "azure")]
        StorageBackend::Azure => {
            let bucket = explicit_bucket.or(config
                .azure_context
                .as_ref()
                .map(|ctx| ctx.default_container.as_str()));
            ("azure", bucket)
        }
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub(super) fn is_object_storage_backend(config: &ServerConfig) -> bool {
    match config.storage_backend {
        StorageBackend::Filesystem => false,
        #[cfg(feature = "s3")]
        StorageBackend::S3 => true,
        #[cfg(feature = "gcs")]
        StorageBackend::Gcs => true,
        #[cfg(feature = "azure")]
        StorageBackend::Azure => true,
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub(super) fn storage_backend_label(config: &ServerConfig) -> &'static str {
    match config.storage_backend {
        StorageBackend::Filesystem => "fs-backend",
        #[cfg(feature = "s3")]
        StorageBackend::S3 => "s3-backend",
        #[cfg(feature = "gcs")]
        StorageBackend::Gcs => "gcs-backend",
        #[cfg(feature = "azure")]
        StorageBackend::Azure => "azure-backend",
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformOptionsPayload {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fit: Option<String>,
    pub position: Option<String>,
    pub format: Option<String>,
    pub quality: Option<u8>,
    pub optimize: Option<String>,
    pub target_quality: Option<String>,
    pub background: Option<String>,
    /// Clockwise rotation in whole degrees. Negatives turn counter-clockwise and values
    /// past a full turn wrap, which is what `Rotation` accepts and what the CLI and the
    /// Wasm package take.
    pub rotate: Option<i32>,
    pub auto_orient: Option<bool>,
    pub strip_metadata: Option<bool>,
    pub preserve_exif: Option<bool>,
    pub crop: Option<String>,
    pub blur: Option<f32>,
    pub sharpen: Option<f32>,
    pub grayscale: Option<bool>,
    pub without_enlargement: Option<bool>,
}

impl TransformOptionsPayload {
    /// Merges per-request overrides on top of preset defaults.
    /// Each field in `overrides` takes precedence when set (`Some`).
    pub(super) fn with_overrides(self, overrides: &TransformOptionsPayload) -> Self {
        Self {
            width: overrides.width.or(self.width),
            height: overrides.height.or(self.height),
            fit: overrides.fit.clone().or(self.fit),
            position: overrides.position.clone().or(self.position),
            format: overrides.format.clone().or(self.format),
            quality: overrides.quality.or(self.quality),
            optimize: overrides.optimize.clone().or(self.optimize),
            target_quality: overrides.target_quality.clone().or(self.target_quality),
            background: overrides.background.clone().or(self.background),
            rotate: overrides.rotate.or(self.rotate),
            auto_orient: overrides.auto_orient.or(self.auto_orient),
            strip_metadata: overrides.strip_metadata.or(self.strip_metadata),
            preserve_exif: overrides.preserve_exif.or(self.preserve_exif),
            crop: overrides.crop.clone().or(self.crop),
            blur: overrides.blur.or(self.blur),
            sharpen: overrides.sharpen.or(self.sharpen),
            grayscale: overrides.grayscale.or(self.grayscale),
            without_enlargement: overrides.without_enlargement.or(self.without_enlargement),
        }
    }

    /// Resolves the requested output format, refusing one truss reads but cannot write.
    ///
    /// The refusal is the same class the pipeline gives it, 415 with
    /// `unsupported-output-media-type`, and the same sentence the CLI and the Wasm package
    /// print; only the moment moves. It used to be raised by the encoder, which meant the
    /// server had already fetched the source and decoded the picture for a request it was
    /// always going to refuse.
    fn output_format(&self) -> Result<Option<MediaType>, HttpResponse> {
        let Some(value) = self.format.as_deref() else {
            return Ok(None);
        };
        let media_type = parse_named(value, "format", MediaType::from_str)?;
        match media_type.unencodable_reason() {
            Some(reason) => Err(unsupported_output_media_type_response(&reason)),
            None => Ok(Some(media_type)),
        }
    }

    pub(super) fn into_options(self) -> Result<TransformOptions, HttpResponse> {
        let defaults = TransformOptions::default();

        // `preserveExif` implies "do not strip", the same way it does on the CLI and in
        // the WASM build. Reading the two fields independently made this adapter the one
        // that answered 400 for `preserveExif=true` on its own — including when a
        // server-side preset was the thing that set it — while every other caller of
        // `resolve_metadata_flags` accepted it.
        let (strip_metadata, preserve_exif) = crate::resolve_metadata_flags(
            self.strip_metadata,
            None,
            self.preserve_exif.or(Some(defaults.preserve_exif)),
        )
        .map_err(|error| bad_request_response(&error.to_string()))?;

        Ok(TransformOptions {
            width: self.width,
            height: self.height,
            fit: parse_optional_named(self.fit.as_deref(), "fit", Fit::from_str)?,
            position: parse_optional_named(
                self.position.as_deref(),
                "position",
                Position::from_str,
            )?,
            format: self.output_format()?,
            quality: self.quality,
            optimize: parse_optional_named(
                self.optimize.as_deref(),
                "optimize",
                OptimizeMode::from_str,
            )?
            .unwrap_or(defaults.optimize),
            target_quality: parse_optional_named(
                self.target_quality.as_deref(),
                "targetQuality",
                TargetQuality::from_str,
            )?,
            background: parse_optional_named(
                self.background.as_deref(),
                "background",
                Rgba8::from_hex,
            )?,
            rotate: match self.rotate {
                Some(value) => parse_named(&value.to_string(), "rotate", Rotation::from_str)?,
                None => defaults.rotate,
            },
            auto_orient: self.auto_orient.unwrap_or(defaults.auto_orient),
            strip_metadata,
            preserve_exif,
            crop: parse_optional_named(self.crop.as_deref(), "crop", CropRegion::from_str)?,
            blur: self.blur,
            sharpen: self.sharpen,
            grayscale: self.grayscale.unwrap_or(defaults.grayscale),
            without_enlargement: self
                .without_enlargement
                .unwrap_or(defaults.without_enlargement),
            deadline: defaults.deadline,
        })
    }
}

/// Overall request deadline for outbound fetches (source + watermark combined).
const REQUEST_DEADLINE_SECS: u64 = 60;

const WATERMARK_DEFAULT_POSITION: Position = Position::BottomRight;
const WATERMARK_DEFAULT_OPACITY: u8 = 50;
const WATERMARK_DEFAULT_MARGIN: u32 = 10;
const WATERMARK_MAX_MARGIN: u32 = 9999;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WatermarkPayload {
    pub(super) url: Option<String>,
    pub(super) position: Option<String>,
    pub(super) opacity: Option<u8>,
    pub(super) margin: Option<u32>,
}

/// Validated watermark parameters ready for fetching. No network I/O performed.
pub(super) struct ValidatedWatermarkPayload {
    pub(super) url: String,
    pub(super) position: Position,
    pub(super) opacity: u8,
    pub(super) margin: u32,
}

impl ValidatedWatermarkPayload {
    pub(super) fn cache_identity(&self) -> String {
        compute_watermark_identity(
            &self.url,
            self.position.as_name(),
            self.opacity,
            self.margin,
        )
    }
}

/// Validates watermark payload fields without performing network I/O.
pub(super) fn validate_watermark_payload(
    payload: Option<&WatermarkPayload>,
) -> Result<Option<ValidatedWatermarkPayload>, HttpResponse> {
    let Some(wm) = payload else {
        return Ok(None);
    };
    let url = wm.url.as_deref().filter(|u| !u.is_empty()).ok_or_else(|| {
        bad_request_response("watermark.url is required when watermark is present")
    })?;

    let position = parse_optional_named(
        wm.position.as_deref(),
        "watermark.position",
        Position::from_str,
    )?
    .unwrap_or(WATERMARK_DEFAULT_POSITION);

    let opacity = wm.opacity.unwrap_or(WATERMARK_DEFAULT_OPACITY);
    if opacity == 0 || opacity > 100 {
        return Err(bad_request_response(
            "watermark.opacity must be between 1 and 100",
        ));
    }
    let margin = wm.margin.unwrap_or(WATERMARK_DEFAULT_MARGIN);
    if margin > WATERMARK_MAX_MARGIN {
        return Err(bad_request_response(
            "watermark.margin must be at most 9999",
        ));
    }

    Ok(Some(ValidatedWatermarkPayload {
        url: url.to_string(),
        position,
        opacity,
        margin,
    }))
}

/// Fetches watermark image and builds WatermarkInput. Called after try_acquire.
pub(super) fn fetch_watermark(
    validated: ValidatedWatermarkPayload,
    config: &ServerConfig,
    deadline: Option<Instant>,
) -> Result<WatermarkInput, HttpResponse> {
    let bytes = read_remote_watermark_bytes(&validated.url, config, deadline)?;
    let artifact = sniff_artifact(RawArtifact::new(bytes, None))
        .map_err(|error| bad_request_response(&format!("watermark image is invalid: {error}")))?;
    if !artifact.media_type.is_raster() {
        return Err(bad_request_response(
            "watermark image must be a raster format (not SVG)",
        ));
    }
    Ok(WatermarkInput {
        image: artifact,
        position: validated.position,
        opacity: validated.opacity,
        margin: validated.margin,
    })
}

pub(super) fn resolve_multipart_watermark(
    bytes: Vec<u8>,
    position: Option<String>,
    opacity: Option<u8>,
    margin: Option<u32>,
) -> Result<WatermarkInput, HttpResponse> {
    let artifact = sniff_artifact(RawArtifact::new(bytes, None))
        .map_err(|error| bad_request_response(&format!("watermark image is invalid: {error}")))?;
    if !artifact.media_type.is_raster() {
        return Err(bad_request_response(
            "watermark image must be a raster format (not SVG)",
        ));
    }
    let position = parse_optional_named(
        position.as_deref(),
        "watermark_position",
        Position::from_str,
    )?
    .unwrap_or(WATERMARK_DEFAULT_POSITION);
    let opacity = opacity.unwrap_or(WATERMARK_DEFAULT_OPACITY);
    if opacity == 0 || opacity > 100 {
        return Err(bad_request_response(
            "watermark_opacity must be between 1 and 100",
        ));
    }
    let margin = margin.unwrap_or(WATERMARK_DEFAULT_MARGIN);
    if margin > WATERMARK_MAX_MARGIN {
        return Err(bad_request_response(
            "watermark_margin must be at most 9999",
        ));
    }
    Ok(WatermarkInput {
        image: artifact,
        position,
        opacity,
        margin,
    })
}

/// Watermark source: either already resolved (multipart upload) or deferred (URL fetch).
pub(super) enum WatermarkSource {
    Deferred(ValidatedWatermarkPayload),
    Ready(WatermarkInput),
    None,
}

impl WatermarkSource {
    pub(super) fn from_validated(validated: Option<ValidatedWatermarkPayload>) -> Self {
        match validated {
            Some(v) => Self::Deferred(v),
            None => Self::None,
        }
    }

    pub(super) fn from_ready(input: Option<WatermarkInput>) -> Self {
        match input {
            Some(w) => Self::Ready(w),
            None => Self::None,
        }
    }

    pub(super) fn is_some(&self) -> bool {
        !matches!(self, Self::None)
    }
}

// ---------------------------------------------------------------------------
// Cached syscall helpers for health endpoints (#74)
// ---------------------------------------------------------------------------

/// Sentinel value representing `None` in atomic storage.
const CACHED_NONE: u64 = u64::MAX;

/// Default TTL for health-check syscall caching (5 seconds).
pub(super) const DEFAULT_HEALTH_CACHE_TTL_SECS: u64 = 5;

/// Default recovery margin for hysteresis-based resource checks.
///
/// When a resource check transitions to "fail", it must recover past
/// `threshold * (1 ± margin)` before returning to "ok",
/// preventing rapid oscillation (flapping) near the boundary.
///
/// Configurable via `TRUSS_HEALTH_HYSTERESIS_MARGIN` (0.01–0.50, default 0.05).
pub(super) const DEFAULT_HYSTERESIS_MARGIN: f64 = 0.05;

/// Directionality for threshold-based resource checks.
#[derive(Clone, Copy)]
pub(crate) enum ThresholdDirection {
    /// Higher values are worse (e.g. memory usage). Fails when `current >= threshold`.
    HigherIsWorse,
    /// Lower values are worse (e.g. free disk space). Fails when `current < threshold`.
    LowerIsWorse,
}

/// Lock-free cache for expensive syscall results used by health endpoints.
///
/// Caches `disk_free_bytes()` and `process_rss_bytes()` with a configurable
/// TTL so that high-frequency polling does not generate redundant kernel
/// context switches and file I/O.
pub(crate) struct HealthCache {
    disk_free: AtomicU64,
    disk_free_at: AtomicU64,
    rss: AtomicU64,
    rss_at: AtomicU64,
    pub(super) ttl_nanos: u64,
    /// Hysteresis recovery margin (0.01–0.50).
    pub(super) hysteresis_margin: f64,
    /// Hysteresis state for disk free-space checks (0 = ok, 1 = fail).
    disk_state: AtomicU8,
    /// Hysteresis state for RSS memory checks (0 = ok, 1 = fail).
    rss_state: AtomicU8,
}

impl HealthCache {
    /// Creates a new cache with the given TTL in seconds and hysteresis margin.
    pub(super) fn new(ttl_secs: u64, hysteresis_margin: f64) -> Self {
        Self {
            disk_free: AtomicU64::new(CACHED_NONE),
            disk_free_at: AtomicU64::new(0),
            rss: AtomicU64::new(CACHED_NONE),
            rss_at: AtomicU64::new(0),
            ttl_nanos: ttl_secs.saturating_mul(1_000_000_000),
            hysteresis_margin,
            disk_state: AtomicU8::new(0),
            rss_state: AtomicU8::new(0),
        }
    }

    /// Returns the monotonic timestamp in nanoseconds since `START_TIME`.
    fn now_nanos() -> u64 {
        super::metrics::START_TIME
            .get_or_init(Instant::now)
            .elapsed()
            .as_nanos() as u64
    }

    /// Returns the cached disk free bytes, refreshing if the TTL has expired.
    pub(super) fn disk_free(&self, path: &std::path::Path) -> Option<u64> {
        let now = Self::now_nanos();
        let last = self.disk_free_at.load(Ordering::Acquire);
        if now.wrapping_sub(last) < self.ttl_nanos && last != 0 {
            let v = self.disk_free.load(Ordering::Relaxed);
            return if v == CACHED_NONE { None } else { Some(v) };
        }
        let fresh = disk_free_bytes(path);
        self.disk_free
            .store(fresh.unwrap_or(CACHED_NONE), Ordering::Relaxed);
        self.disk_free_at.store(now, Ordering::Release);
        fresh
    }

    /// Returns the cached process RSS bytes, refreshing if the TTL has expired.
    pub(super) fn rss(&self) -> Option<u64> {
        let now = Self::now_nanos();
        let last = self.rss_at.load(Ordering::Acquire);
        if now.wrapping_sub(last) < self.ttl_nanos && last != 0 {
            let v = self.rss.load(Ordering::Relaxed);
            return if v == CACHED_NONE { None } else { Some(v) };
        }
        let fresh = process_rss_bytes();
        self.rss
            .store(fresh.unwrap_or(CACHED_NONE), Ordering::Relaxed);
        self.rss_at.store(now, Ordering::Release);
        fresh
    }

    /// Applies hysteresis to a threshold check, preventing flapping when
    /// values hover near the boundary.
    ///
    /// `higher_is_worse` controls directionality:
    /// - `true` (memory): fail when `current >= threshold`, recover when
    ///   `current < threshold * (1 - margin)`
    /// - `false` (disk): fail when `current < threshold`, recover when
    ///   `current > threshold * (1 + margin)`
    ///
    /// Returns `(ok, recovering)` where `recovering` is `true` when the check
    /// remains in the "fail" state only because the recovery margin has not been
    /// crossed yet (the value has passed the threshold but not the recovery
    /// point).
    pub(crate) fn check_with_hysteresis(
        &self,
        state: &AtomicU8,
        current: u64,
        threshold: u64,
        direction: ThresholdDirection,
    ) -> (bool, bool) {
        let prev_fail = state.load(Ordering::Relaxed) == 1;
        let (ok, recovering) = match direction {
            ThresholdDirection::HigherIsWorse => {
                if prev_fail {
                    let recovery = (threshold as f64 * (1.0 - self.hysteresis_margin)) as u64;
                    let ok = current < recovery;
                    // Recovering: value has dropped below threshold but not below recovery point
                    let recovering = !ok && current < threshold;
                    (ok, recovering)
                } else {
                    (current < threshold, false)
                }
            }
            ThresholdDirection::LowerIsWorse => {
                if prev_fail {
                    let recovery = (threshold as f64 * (1.0 + self.hysteresis_margin)) as u64;
                    let ok = current > recovery;
                    // Recovering: value has risen above threshold but not above recovery point
                    let recovering = !ok && current >= threshold;
                    (ok, recovering)
                } else {
                    (current >= threshold, false)
                }
            }
        };
        state.store(if ok { 0 } else { 1 }, Ordering::Relaxed);
        (ok, recovering)
    }
}

/// Returns the number of free bytes on the filesystem containing `path`,
/// or `None` if the query fails.
#[cfg(target_os = "linux")]
pub(super) fn disk_free_bytes(path: &std::path::Path) -> Option<u64> {
    use std::ffi::CString;

    let c_path = CString::new(path.to_str()?).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret == 0 {
        stat.f_bavail.checked_mul(stat.f_frsize)
    } else {
        None
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn disk_free_bytes(_path: &std::path::Path) -> Option<u64> {
    None
}

/// Returns the current process RSS (Resident Set Size) in bytes by reading
/// `/proc/self/status`. Returns `None` on non-Linux platforms or on read failure.
#[cfg(target_os = "linux")]
pub(super) fn process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            let value = value.trim();
            // Format: "123456 kB"
            let kb_str = value.strip_suffix(" kB")?.trim();
            let kb: u64 = kb_str.parse().ok()?;
            return kb.checked_mul(1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub(super) fn process_rss_bytes() -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// Health handlers
// ---------------------------------------------------------------------------

/// Returns a minimal liveness response confirming the process is running.
pub(super) fn handle_health_live() -> HttpResponse {
    let body = serde_json::to_vec(&json!({
        "status": "ok",
        "service": "truss",
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .expect("serialize liveness");
    let mut body = body;
    body.push(b'\n');
    HttpResponse::json("200 OK", body)
}

/// Returns a readiness response after checking that critical infrastructure
/// dependencies are available (storage root, cache root if configured, S3
/// reachability) and configurable resource thresholds.
pub(super) fn handle_health_ready(config: &ServerConfig) -> HttpResponse {
    // When the server is draining (shutdown signal received), immediately
    // report not-ready so that load balancers stop routing traffic.
    // Skip expensive probes (storage, disk, memory) — they are irrelevant
    // once the process is shutting down.
    if config.draining.load(Ordering::Relaxed) {
        let mut body = serde_json::to_vec(&json!({
            "status": "fail",
            "checks": [{ "name": "draining", "status": "fail" }],
        }))
        .expect("serialize readiness");
        body.push(b'\n');
        let mut response = HttpResponse::json("503 Service Unavailable", body);
        // The process is going away, not momentarily busy, so tell the client
        // and any intermediary not to come straight back.
        response
            .headers
            .push(("Retry-After".to_string(), "5".to_string()));
        return response;
    }

    let (checks, all_ok) = collect_resource_checks(config);

    let status_str = if all_ok { "ok" } else { "fail" };
    let mut body = serde_json::to_vec(&json!({
        "status": status_str,
        "checks": checks,
    }))
    .expect("serialize readiness");
    body.push(b'\n');

    // Resource check results use application/json (health-check format),
    // not problem+json, because they represent a structured health report
    // rather than an error condition.
    if all_ok {
        HttpResponse::json("200 OK", body)
    } else {
        HttpResponse::json("503 Service Unavailable", body)
    }
}

/// Collects all resource health checks shared by `/health` and `/health/ready`.
///
/// Returns the accumulated check entries and a boolean indicating whether all
/// checks passed.
fn collect_resource_checks(config: &ServerConfig) -> (Vec<serde_json::Value>, bool) {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut all_ok = true;

    for (ok, name) in storage_health_check(config) {
        checks.push(json!({
            "name": name,
            "status": if ok { "ok" } else { "fail" },
        }));
        if !ok {
            all_ok = false;
        }
    }

    if let Some(cache_root) = &config.cache_root {
        let cache_ok = cache_root.is_dir();
        checks.push(json!({
            "name": "cacheRoot",
            "status": if cache_ok { "ok" } else { "fail" },
        }));
        if !cache_ok {
            all_ok = false;
        }
    }

    if let Some(cache_root) = &config.cache_root {
        let free = config.health_cache.disk_free(cache_root);
        let threshold = config.health_cache_min_free_bytes;
        let (disk_ok, disk_recovering) = match (free, threshold) {
            (Some(f), Some(min)) => config.health_cache.check_with_hysteresis(
                &config.health_cache.disk_state,
                f,
                min,
                ThresholdDirection::LowerIsWorse,
            ),
            _ => (true, false),
        };
        let mut check = json!({
            "name": "cacheDiskFree",
            "status": if disk_ok { "ok" } else { "fail" },
        });
        if let Some(f) = free {
            check["freeBytes"] = json!(f);
        }
        if let Some(min) = threshold {
            check["thresholdBytes"] = json!(min);
        }
        if disk_recovering {
            check["recovering"] = json!(true);
        }
        checks.push(check);
        if !disk_ok {
            all_ok = false;
        }
    }

    // Concurrency utilization
    let in_flight = config.transforms_in_flight.load(Ordering::Relaxed);
    let overloaded = in_flight >= config.max_concurrent_transforms;
    checks.push(json!({
        "name": "transformCapacity",
        "status": if overloaded { "fail" } else { "ok" },
        "current": in_flight,
        "max": config.max_concurrent_transforms,
    }));
    if overloaded {
        all_ok = false;
    }

    // Memory usage (Linux only) — skip entirely when RSS is unavailable
    if let Some(rss_bytes) = config.health_cache.rss() {
        let threshold = config.health_max_memory_bytes;
        let (mem_ok, mem_recovering) = match threshold {
            Some(max) => config.health_cache.check_with_hysteresis(
                &config.health_cache.rss_state,
                rss_bytes,
                max,
                ThresholdDirection::HigherIsWorse,
            ),
            None => (true, false),
        };
        let mut check = json!({
            "name": "memoryUsage",
            "status": if mem_ok { "ok" } else { "fail" },
            "rssBytes": rss_bytes,
        });
        if let Some(max) = threshold {
            check["thresholdBytes"] = json!(max);
        }
        if mem_recovering {
            check["recovering"] = json!(true);
        }
        checks.push(check);
        if !mem_ok {
            all_ok = false;
        }
    }

    (checks, all_ok)
}

/// Returns storage backend health checks (storage root existence and cloud
/// backend reachability).
pub(super) fn storage_health_check(config: &ServerConfig) -> Vec<(bool, &'static str)> {
    #[allow(unused_mut)]
    let mut checks = vec![(config.storage_root.is_dir(), "storageRoot")];
    #[cfg(feature = "s3")]
    if config.storage_backend == StorageBackend::S3 {
        let reachable = config
            .s3_context
            .as_ref()
            .is_some_and(|ctx| ctx.check_reachable());
        checks.push((reachable, "storageBackend"));
    }
    #[cfg(feature = "gcs")]
    if config.storage_backend == StorageBackend::Gcs {
        let reachable = config
            .gcs_context
            .as_ref()
            .is_some_and(|ctx| ctx.check_reachable());
        checks.push((reachable, "storageBackend"));
    }
    #[cfg(feature = "azure")]
    if config.storage_backend == StorageBackend::Azure {
        let reachable = config
            .azure_context
            .as_ref()
            .is_some_and(|ctx| ctx.check_reachable());
        checks.push((reachable, "storageBackend"));
    }
    checks
}

pub(super) fn handle_health(config: &ServerConfig) -> HttpResponse {
    let (checks, all_ok) = collect_resource_checks(config);

    let status_str = if all_ok { "ok" } else { "fail" };
    let mut body = serde_json::to_vec(&json!({
        "status": status_str,
        "service": "truss",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": uptime_seconds(),
        "checks": checks,
        "maxInputPixels": config.max_input_pixels,
    }))
    .expect("serialize health");
    body.push(b'\n');

    HttpResponse::json("200 OK", body)
}

// ---------------------------------------------------------------------------
// Metrics handler
// ---------------------------------------------------------------------------

pub(super) fn handle_metrics_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if config.disable_metrics {
        return HttpResponse::problem("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec());
    }

    if let Some(expected) = &config.metrics_token {
        let provided = request
            .header("authorization")
            .and_then(super::auth::extract_bearer_token);
        match provided {
            Some(token) if token.as_bytes().ct_eq(expected.as_bytes()).into() => {}
            _ => {
                return super::response::auth_required_response(
                    "metrics endpoint requires authentication",
                );
            }
        }
    }

    HttpResponse::text(
        "200 OK",
        "text/plain; version=0.0.4; charset=utf-8",
        render_metrics_text(
            config.max_concurrent_transforms,
            &config.transforms_in_flight,
        )
        .into_bytes(),
    )
}

// ---------------------------------------------------------------------------
// Transform handler
// ---------------------------------------------------------------------------

pub(super) fn handle_transform_request(
    request: HttpRequest,
    config: &ServerConfig,
) -> HttpResponse {
    let request_deadline = Some(Instant::now() + Duration::from_secs(REQUEST_DEADLINE_SECS));

    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    if !request_has_json_content_type(&request) {
        return unsupported_media_type_response("content-type must be application/json");
    }

    let payload: TransformImageRequestPayload = match serde_json::from_slice(&request.body) {
        Ok(payload) => payload,
        Err(error) => {
            return bad_request_response(&format!("request body must be valid JSON: {error}"));
        }
    };
    let options = match payload.options.into_options() {
        Ok(options) => options,
        Err(response) => return response,
    };

    let versioned_hash = payload.source.versioned_source_hash(config);
    let validated_wm = match validate_watermark_payload(payload.watermark.as_ref()) {
        Ok(wm) => wm,
        Err(response) => return response,
    };
    let watermark_id = validated_wm
        .as_ref()
        .map(ValidatedWatermarkPayload::cache_identity);

    if let Some(response) = try_versioned_cache_lookup(
        versioned_hash.as_deref(),
        &options,
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
        watermark_id.as_deref(),
    ) {
        return response;
    }

    let storage_start = Instant::now();
    let backend_label = payload.source.metrics_backend_label(config);
    let backend_idx = backend_label.map(|l| storage_backend_index_from_config(&l));
    let source_bytes = match resolve_source_bytes(payload.source, config, request_deadline) {
        Ok(bytes) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            bytes
        }
        Err(response) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            return response;
        }
    };
    transform_source_bytes(
        source_bytes,
        options,
        versioned_hash.as_deref(),
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
        WatermarkSource::from_validated(validated_wm),
        watermark_id.as_deref(),
        request_deadline,
    )
}

// ---------------------------------------------------------------------------
// Public GET handlers
// ---------------------------------------------------------------------------

pub(super) fn handle_public_path_request(
    request: HttpRequest,
    config: &ServerConfig,
) -> HttpResponse {
    handle_public_get_request(request, config, PublicSourceKind::Path)
}

pub(super) fn handle_public_url_request(
    request: HttpRequest,
    config: &ServerConfig,
) -> HttpResponse {
    handle_public_get_request(request, config, PublicSourceKind::Url)
}

fn handle_public_get_request(
    request: HttpRequest,
    config: &ServerConfig,
    source_kind: PublicSourceKind,
) -> HttpResponse {
    let request_deadline = Some(Instant::now() + Duration::from_secs(REQUEST_DEADLINE_SECS));
    let query = match parse_query_params(&request) {
        Ok(query) => query,
        Err(response) => return response,
    };
    if let Err(response) = authorize_signed_request(&request, &query, config) {
        return response;
    }
    let (source, options, watermark_payload) =
        match parse_public_get_request(&query, source_kind, config) {
            Ok(parsed) => parsed,
            Err(response) => return response,
        };

    let validated_wm = match validate_watermark_payload(watermark_payload.as_ref()) {
        Ok(wm) => wm,
        Err(response) => return response,
    };
    let watermark_id = validated_wm
        .as_ref()
        .map(ValidatedWatermarkPayload::cache_identity);

    // When the storage backend is object storage (S3 or GCS), convert Path
    // sources to Storage sources so that the `path` query parameter is
    // resolved as an object key.
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    let source = if is_object_storage_backend(config) {
        match source {
            TransformSourcePayload::Path { path, version } => TransformSourcePayload::Storage {
                bucket: None,
                key: path.trim_start_matches('/').to_string(),
                version,
            },
            other => other,
        }
    } else {
        source
    };

    let versioned_hash = source.versioned_source_hash(config);
    if let Some(response) = try_versioned_cache_lookup(
        versioned_hash.as_deref(),
        &options,
        &request,
        ImageResponsePolicy::PublicGet,
        config,
        watermark_id.as_deref(),
    ) {
        return response;
    }

    let storage_start = Instant::now();
    let backend_label = source.metrics_backend_label(config);
    let backend_idx = backend_label.map(|l| storage_backend_index_from_config(&l));
    let source_bytes = match resolve_source_bytes(source, config, request_deadline) {
        Ok(bytes) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            bytes
        }
        Err(response) => {
            if let Some(idx) = backend_idx {
                record_storage_duration(idx, storage_start);
            }
            return response;
        }
    };

    transform_source_bytes(
        source_bytes,
        options,
        versioned_hash.as_deref(),
        &request,
        ImageResponsePolicy::PublicGet,
        config,
        WatermarkSource::from_validated(validated_wm),
        watermark_id.as_deref(),
        request_deadline,
    )
}

// ---------------------------------------------------------------------------
// Upload handler
// ---------------------------------------------------------------------------

pub(super) fn handle_upload_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    let boundary = match parse_multipart_boundary(&request) {
        Ok(boundary) => boundary,
        Err(response) => return response,
    };
    let (file_bytes, options, watermark) = match parse_upload_request(&request.body, &boundary) {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    let watermark_identity = watermark.as_ref().map(|wm| {
        let content_hash = hex::encode(sha2::Sha256::digest(&wm.image.bytes));
        super::cache::compute_watermark_content_identity(
            &content_hash,
            wm.position.as_name(),
            wm.opacity,
            wm.margin,
        )
    });
    transform_source_bytes(
        file_bytes,
        options,
        None,
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
        WatermarkSource::from_ready(watermark),
        watermark_identity.as_deref(),
        None,
    )
}

// ---------------------------------------------------------------------------
// Public GET query parsing
// ---------------------------------------------------------------------------

pub(super) fn parse_public_get_request(
    query: &BTreeMap<String, String>,
    source_kind: PublicSourceKind,
    config: &ServerConfig,
) -> Result<
    (
        TransformSourcePayload,
        TransformOptions,
        Option<WatermarkPayload>,
    ),
    HttpResponse,
> {
    validate_public_query_names(query, source_kind)?;

    let source = match source_kind {
        PublicSourceKind::Path => TransformSourcePayload::Path {
            path: required_query_param(query, "path")?.to_string(),
            version: query.get("version").cloned(),
        },
        PublicSourceKind::Url => TransformSourcePayload::Url {
            url: required_query_param(query, "url")?.to_string(),
            version: query.get("version").cloned(),
        },
    };

    let has_orphaned_watermark_params = query.contains_key("watermarkPosition")
        || query.contains_key("watermarkOpacity")
        || query.contains_key("watermarkMargin");
    let watermark = if query.contains_key("watermarkUrl") {
        Some(WatermarkPayload {
            url: query.get("watermarkUrl").cloned(),
            position: query.get("watermarkPosition").cloned(),
            opacity: parse_optional_u8_query(query, "watermarkOpacity")?,
            margin: parse_optional_integer_query(query, "watermarkMargin")?,
        })
    } else if has_orphaned_watermark_params {
        return Err(bad_request_response(
            "watermarkPosition, watermarkOpacity, and watermarkMargin require watermarkUrl",
        ));
    } else {
        None
    };

    // Build per-request overrides from query parameters.
    let per_request = TransformOptionsPayload {
        width: parse_optional_integer_query(query, "width")?,
        height: parse_optional_integer_query(query, "height")?,
        fit: query.get("fit").cloned(),
        position: query.get("position").cloned(),
        format: query.get("format").cloned(),
        quality: parse_optional_u8_query(query, "quality")?,
        optimize: query.get("optimize").cloned(),
        target_quality: query.get("targetQuality").cloned(),
        background: query.get("background").cloned(),
        // `Rotation` accepts any whole number of degrees, negatives included, so the query
        // is read through it rather than through a narrower integer type with a range
        // sentence of its own that stopped being true in v0.13.0.
        rotate: parse_optional_named(query.get("rotate").map(String::as_str), "rotate", |value| {
            Rotation::from_str(value).map(|rotation| i32::from(rotation.as_degrees()))
        })?,
        auto_orient: parse_optional_bool_query(query, "autoOrient")?,
        strip_metadata: parse_optional_bool_query(query, "stripMetadata")?,
        preserve_exif: parse_optional_bool_query(query, "preserveExif")?,
        crop: query.get("crop").cloned(),
        blur: parse_optional_float_query(query, "blur")?,
        sharpen: parse_optional_float_query(query, "sharpen")?,
        grayscale: parse_optional_bool_query(query, "grayscale")?,
        without_enlargement: parse_optional_bool_query(query, "withoutEnlargement")?,
    };

    // Resolve preset and merge with per-request overrides.
    let merged = if let Some(preset_name) = query.get("preset") {
        let presets = config.presets.read().expect("presets lock poisoned");
        let preset = presets
            .get(preset_name)
            .ok_or_else(|| bad_request_response(&format!("unknown preset `{preset_name}`")))?;
        preset.clone().with_overrides(&per_request)
    } else {
        per_request
    };

    let options = merged.into_options()?;

    Ok((source, options, watermark))
}

// ---------------------------------------------------------------------------
// Transform pipeline
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub(super) fn transform_source_bytes(
    source_bytes: Vec<u8>,
    options: TransformOptions,
    versioned_hash: Option<&str>,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    config: &ServerConfig,
    watermark: WatermarkSource,
    watermark_identity: Option<&str>,
    request_deadline: Option<Instant>,
) -> HttpResponse {
    let content_hash;
    let source_hash = match versioned_hash {
        Some(hash) => hash,
        None => {
            content_hash = hex::encode(Sha256::digest(&source_bytes));
            &content_hash
        }
    };

    let cache = config.cache_root.as_ref().map(|root| {
        TransformCache::new(root.clone())
            .with_log_handler(config.log_handler.clone())
            .with_max_bytes(config.cache_max_bytes)
    });

    if let Some(ref cache) = cache
        && options.format.is_some()
    {
        let cache_key = compute_cache_key(source_hash, &options, watermark_identity);
        if let Some(response) = cache.get(&cache_key).into_hit_response(
            request,
            response_policy,
            false,
            config.public_cache_control(),
            &config.custom_response_headers,
        ) {
            CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
            return response;
        }
    }

    let Some(_slot) = TransformSlot::try_acquire(
        &config.transforms_in_flight,
        config.max_concurrent_transforms,
    ) else {
        return service_unavailable_response("too many concurrent transforms; retry later");
    };
    transform_source_bytes_inner(
        source_bytes,
        options,
        request,
        response_policy,
        cache.as_ref(),
        source_hash,
        ImageResponseConfig {
            disable_accept_negotiation: config.disable_accept_negotiation,
            public_cache_control: PublicCacheControl {
                max_age: config.public_max_age_seconds,
                stale_while_revalidate: config.public_stale_while_revalidate_seconds,
            },
            transform_deadline: Duration::from_secs(config.transform_deadline_secs),
        },
        watermark,
        watermark_identity,
        config,
        request_deadline,
    )
}

#[allow(clippy::too_many_arguments)]
fn transform_source_bytes_inner(
    source_bytes: Vec<u8>,
    mut options: TransformOptions,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    cache: Option<&TransformCache>,
    source_hash: &str,
    response_config: ImageResponseConfig,
    watermark_source: WatermarkSource,
    watermark_identity: Option<&str>,
    config: &ServerConfig,
    request_deadline: Option<Instant>,
) -> HttpResponse {
    if options.deadline.is_none() {
        options.deadline = Some(response_config.transform_deadline);
    }
    let artifact = match sniff_artifact(RawArtifact::new(source_bytes, None)) {
        Ok(artifact) => artifact,
        Err(error) => {
            record_transform_error(&error);
            return transform_error_response(error);
        }
    };
    // `Vary` describes the resource, not the request that happened to arrive. What
    // matters is whether `Accept` could have selected the representation for this
    // URL, not whether it did: a request that sends no `Accept` gets the default
    // representation of a URL that another request negotiates away from, and a
    // shared cache that stores a response with no `Vary` serves it to everyone.
    let accept_may_vary = options.format.is_none() && !response_config.disable_accept_negotiation;
    if accept_may_vary {
        match negotiate_output_format(
            request.header("accept"),
            &artifact,
            &config.format_preference,
        ) {
            Ok(Some(format)) => options.format = Some(format),
            Ok(None) => {}
            Err(response) => return response,
        }
    }

    if options.format.is_none() {
        // The server needs a concrete format here, ahead of the transform, for the cache
        // key and the response Content-Type. `default_output` keeps the input's format
        // except for a decode-only input such as GIF, which resolves to PNG.
        options.format = Some(artifact.media_type.default_output());
    }

    // Check input pixel count against the server-level limit before decode.
    // This runs before the cache lookup so that a policy change (lowering the
    // limit) takes effect immediately, even for previously-cached images.
    if let (Some(w), Some(h)) = (artifact.metadata.width, artifact.metadata.height) {
        let pixels = u64::from(w) * u64::from(h);
        if pixels > config.max_input_pixels {
            return super::response::unprocessable_entity_response(&format!(
                "input image has {pixels} pixels, server limit is {}",
                config.max_input_pixels
            ));
        }
    }

    // The Accept header is deliberately absent from the key. Negotiation's whole output is
    // the format, which `options.format` already carries, so including the raw header would
    // write one copy of the same image per distinct header string — unboundedly many, and
    // straight off the request. `Vary: Accept` is built per response from `accept_may_vary`,
    // so an entry shared with a request that named the format explicitly still answers right.
    let cache_key = compute_cache_key(source_hash, &options, watermark_identity);

    if let Some(cache) = cache
        && let Some(response) = cache.get(&cache_key).into_hit_response(
            request,
            response_policy,
            accept_may_vary,
            response_config.public_cache_control,
            &config.custom_response_headers,
        )
    {
        CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
        return response;
    }

    if cache.is_some() {
        CACHE_MISSES_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    let is_svg = artifact.media_type == MediaType::Svg;

    // Resolve watermark: reject SVG+watermark early (before fetch), then fetch if deferred.
    let watermark = if is_svg && watermark_source.is_some() {
        return bad_request_response("watermark is not supported for SVG source images");
    } else {
        match watermark_source {
            WatermarkSource::Deferred(validated) => {
                match fetch_watermark(validated, config, request_deadline) {
                    Ok(wm) => {
                        record_watermark_transform();
                        Some(wm)
                    }
                    Err(response) => return response,
                }
            }
            WatermarkSource::Ready(wm) => {
                record_watermark_transform();
                Some(wm)
            }
            WatermarkSource::None => None,
        }
    };

    let had_watermark = watermark.is_some();

    let transform_start = Instant::now();
    let mut request_obj = TransformRequest::new(artifact, options);
    request_obj.watermark = watermark;
    let result = match transform(request_obj) {
        Ok(result) => result,
        Err(error) => {
            record_transform_error(&error);
            return transform_error_response(error);
        }
    };
    record_transform_duration(result.artifact.media_type, transform_start);

    // The warnings go three ways: the log, the response, and the cache entry, so that a
    // later hit answers with the same headers this miss does.
    let warnings: Vec<String> = result
        .warnings
        .iter()
        .map(|warning| warning_header_value(&warning.to_string()))
        .collect();
    for warning in &result.warnings {
        let msg = format!("truss: {warning}");
        if let Some(c) = cache
            && let Some(handler) = &c.log_handler
        {
            handler(&msg);
        } else {
            stderr_write(&msg);
        }
    }

    let output = result.artifact;

    if let Some(cache) = cache {
        cache.put(&cache_key, output.media_type, &output.bytes, &warnings);
    }

    let cache_hit_status = if cache.is_some() {
        CacheHitStatus::Miss
    } else {
        CacheHitStatus::Disabled
    };

    let etag = build_image_etag(&output.bytes);
    let mut headers = build_image_response_headers(
        output.media_type,
        &etag,
        response_policy,
        accept_may_vary,
        cache_hit_status,
        response_config.public_cache_control,
        &config.custom_response_headers,
    );

    if matches!(response_policy, ImageResponsePolicy::PublicGet)
        && if_none_match_matches(request.header("if-none-match"), &etag)
    {
        return HttpResponse::empty("304 Not Modified", headers);
    }

    // A 304 says the client already has the representation, warnings included; only the
    // response that carries the image carries them.
    push_warning_headers(&mut headers, &warnings);
    let mut response = HttpResponse::binary_with_headers(
        "200 OK",
        output.media_type.as_mime(),
        headers,
        output.bytes,
    );
    if had_watermark {
        response
            .headers
            .push(("X-Truss-Watermark".to_string(), "true".to_string()));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    use ThresholdDirection::{HigherIsWorse, LowerIsWorse};

    /// Shorthand: returns (ok, recovering) tuple from check_with_hysteresis.
    fn check(
        cache: &HealthCache,
        state: &AtomicU8,
        current: u64,
        threshold: u64,
        direction: ThresholdDirection,
    ) -> (bool, bool) {
        cache.check_with_hysteresis(state, current, threshold, direction)
    }

    // -- Memory hysteresis (HigherIsWorse) --

    #[test]
    fn hysteresis_memory_ok_below_threshold() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        assert_eq!(
            check(&c, &c.rss_state, 999, 1000, HigherIsWorse),
            (true, false)
        );
    }

    #[test]
    fn hysteresis_memory_fails_at_threshold() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        assert_eq!(
            check(&c, &c.rss_state, 1000, 1000, HigherIsWorse),
            (false, false)
        );
    }

    #[test]
    fn hysteresis_memory_stays_failed_in_margin() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        check(&c, &c.rss_state, 1000, 1000, HigherIsWorse);
        // 960 is below threshold (1000) but above recovery (950) -> recovering
        assert_eq!(
            check(&c, &c.rss_state, 960, 1000, HigherIsWorse),
            (false, true)
        );
        // 950 is at recovery boundary -> still recovering
        assert_eq!(
            check(&c, &c.rss_state, 950, 1000, HigherIsWorse),
            (false, true)
        );
    }

    #[test]
    fn hysteresis_memory_recovers_below_margin() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        check(&c, &c.rss_state, 1000, 1000, HigherIsWorse);
        assert_eq!(
            check(&c, &c.rss_state, 949, 1000, HigherIsWorse),
            (true, false)
        );
    }

    #[test]
    fn hysteresis_memory_full_cycle() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        assert!(check(&c, &c.rss_state, 900, 1000, HigherIsWorse).0);
        assert!(!check(&c, &c.rss_state, 1000, 1000, HigherIsWorse).0);
        // In margin: recovering
        assert_eq!(
            check(&c, &c.rss_state, 960, 1000, HigherIsWorse),
            (false, true)
        );
        assert!(check(&c, &c.rss_state, 940, 1000, HigherIsWorse).0);
        assert!(check(&c, &c.rss_state, 999, 1000, HigherIsWorse).0);
        assert!(!check(&c, &c.rss_state, 1000, 1000, HigherIsWorse).0);
    }

    // -- Disk hysteresis (LowerIsWorse) --

    #[test]
    fn hysteresis_disk_ok_at_threshold() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        assert_eq!(
            check(&c, &c.disk_state, 1000, 1000, LowerIsWorse),
            (true, false)
        );
    }

    #[test]
    fn hysteresis_disk_fails_below_threshold() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        assert_eq!(
            check(&c, &c.disk_state, 999, 1000, LowerIsWorse),
            (false, false)
        );
    }

    #[test]
    fn hysteresis_disk_stays_failed_in_margin() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        check(&c, &c.disk_state, 999, 1000, LowerIsWorse);
        // 1040 is above threshold (1000) but below recovery (1050) -> recovering
        assert_eq!(
            check(&c, &c.disk_state, 1040, 1000, LowerIsWorse),
            (false, true)
        );
        // 1050 is at recovery boundary -> still recovering
        assert_eq!(
            check(&c, &c.disk_state, 1050, 1000, LowerIsWorse),
            (false, true)
        );
    }

    #[test]
    fn hysteresis_disk_recovers_above_margin() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        check(&c, &c.disk_state, 999, 1000, LowerIsWorse);
        assert_eq!(
            check(&c, &c.disk_state, 1051, 1000, LowerIsWorse),
            (true, false)
        );
    }

    #[test]
    fn hysteresis_disk_full_cycle() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        assert!(check(&c, &c.disk_state, 2000, 1000, LowerIsWorse).0);
        assert!(!check(&c, &c.disk_state, 999, 1000, LowerIsWorse).0);
        // In margin: recovering
        assert_eq!(
            check(&c, &c.disk_state, 1040, 1000, LowerIsWorse),
            (false, true)
        );
        assert!(check(&c, &c.disk_state, 1051, 1000, LowerIsWorse).0);
        assert!(check(&c, &c.disk_state, 1000, 1000, LowerIsWorse).0);
        assert!(!check(&c, &c.disk_state, 999, 1000, LowerIsWorse).0);
    }

    #[test]
    fn hysteresis_independent_states() {
        let c = HealthCache::new(5, DEFAULT_HYSTERESIS_MARGIN);
        assert!(!check(&c, &c.disk_state, 500, 1000, LowerIsWorse).0);
        assert!(check(&c, &c.rss_state, 500, 1000, HigherIsWorse).0);
        assert_eq!(c.disk_state.load(Ordering::Relaxed), 1);
        assert_eq!(c.rss_state.load(Ordering::Relaxed), 0);
    }

    // -- Metadata flags resolve the way every other adapter resolves them --

    /// `rotate` is a whole number of degrees, sign included, everywhere truss takes one.
    ///
    /// The payload used to hold a `u16`, so `-90`, which the CLI and the Wasm package both
    /// turn counter-clockwise, was refused by serde before any truss code ran, with a
    /// message that named the Rust type.
    #[test]
    fn rotate_accepts_the_angles_the_other_adapters_accept() {
        for (degrees, expected) in [
            (-90, Rotation::DEG_270),
            (-360, Rotation::DEG_0),
            (0, Rotation::DEG_0),
            (45, Rotation::from_degrees(45)),
            (370, Rotation::from_degrees(10)),
        ] {
            let options = TransformOptionsPayload {
                rotate: Some(degrees),
                ..TransformOptionsPayload::default()
            }
            .into_options()
            .unwrap_or_else(|_| panic!("rotate {degrees} is a whole number of degrees"));

            assert_eq!(options.rotate, expected, "rotate {degrees}");
        }
    }

    /// A `gif` output is refused while the options are read, not after the picture has been
    /// decoded, and with the class the pipeline gives the same refusal.
    #[test]
    fn a_decode_only_output_format_is_refused_with_the_options() {
        let error = TransformOptionsPayload {
            format: Some("gif".to_string()),
            ..TransformOptionsPayload::default()
        }
        .into_options()
        .expect_err("gif has no encoder behind it");

        assert_eq!(error.status, "415 Unsupported Media Type");
        let body = String::from_utf8(error.body).expect("utf-8 problem body");
        assert!(body.contains("unsupported-output-media-type"), "{body}");
        assert!(body.contains("input-only"), "{body}");
    }

    /// A format name that is not a format is still a request that could not be understood,
    /// which is a different class from a format truss knows and will not write.
    #[test]
    fn an_unknown_output_format_stays_an_invalid_request() {
        let error = TransformOptionsPayload {
            format: Some("bogus".to_string()),
            ..TransformOptionsPayload::default()
        }
        .into_options()
        .expect_err("bogus is not a format");

        assert_eq!(error.status, "400 Bad Request");
        let body = String::from_utf8(error.body).expect("utf-8 problem body");
        assert!(body.contains("invalid-request"), "{body}");
    }

    #[test]
    fn preserve_exif_alone_implies_not_stripping() {
        // `?preserveExif=true` on its own used to be a 400 from this adapter alone,
        // because the two fields were read independently instead of through
        // `resolve_metadata_flags`, whose contract is that every adapter agrees.
        let options = TransformOptionsPayload {
            preserve_exif: Some(true),
            ..TransformOptionsPayload::default()
        }
        .into_options()
        .expect("preserveExif on its own is a complete request");

        assert!(options.preserve_exif);
        assert!(!options.strip_metadata);
    }

    #[test]
    fn preserve_exif_wins_over_an_explicit_strip_metadata() {
        // The CLI resolves `--strip-metadata --preserve-exif` the same way: the more
        // specific request decides, rather than the pair being refused.
        let options = TransformOptionsPayload {
            preserve_exif: Some(true),
            strip_metadata: Some(true),
            ..TransformOptionsPayload::default()
        }
        .into_options()
        .expect("the pair resolves rather than failing");

        assert!(options.preserve_exif);
        assert!(!options.strip_metadata);
    }

    #[test]
    fn metadata_flags_keep_their_defaults_when_nothing_asks_for_them() {
        let options = TransformOptionsPayload::default()
            .into_options()
            .expect("an empty payload is valid");

        assert!(!options.preserve_exif);
        assert!(options.strip_metadata, "stripping stays the default");

        let kept = TransformOptionsPayload {
            strip_metadata: Some(false),
            ..TransformOptionsPayload::default()
        }
        .into_options()
        .expect("stripMetadata=false is valid on its own");

        assert!(!kept.preserve_exif);
        assert!(!kept.strip_metadata);
    }

    #[test]
    fn a_preset_that_sets_preserve_exif_is_usable_without_a_second_field() {
        // The 400 also reached operators: a preset naming only `preserveExif` was
        // unusable until `stripMetadata: false` was written beside it.
        let preset = TransformOptionsPayload {
            preserve_exif: Some(true),
            ..TransformOptionsPayload::default()
        };
        let options = preset
            .with_overrides(&TransformOptionsPayload {
                width: Some(64),
                ..TransformOptionsPayload::default()
            })
            .into_options()
            .expect("a preset may set preserveExif on its own");

        assert!(options.preserve_exif);
        assert!(!options.strip_metadata);
        assert_eq!(options.width, Some(64));
    }
}
