use super::TransformOptionsPayload;
#[cfg(feature = "azure")]
use super::azure;
#[cfg(feature = "gcs")]
use super::gcs;
/// Default maximum number of concurrent transforms allowed.
pub(super) const DEFAULT_MAX_CONCURRENT_TRANSFORMS: u64 = 64;
#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
use super::remote::STORAGE_DOWNLOAD_TIMEOUT_SECS;
#[cfg(feature = "s3")]
use super::s3;
use super::stderr_write;

use std::collections::HashMap;
use std::env;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use url::Url;

/// Log verbosity level for the server.
///
/// Levels are ordered from least verbose (`Error`) to most verbose (`Debug`).
/// A message is emitted only when its level is less than or equal to the
/// currently active level.
///
/// Configurable at startup via `TRUSS_LOG_LEVEL` (default: `info`) and
/// switchable at runtime via `SIGUSR1` (Unix only), which cycles through
/// `info → debug → error → warn → info`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Errors that indicate a failed operation.
    Error = 0,
    /// Warnings about potentially harmful situations.
    Warn = 1,
    /// Informational messages about normal operations.
    Info = 2,
    /// Detailed diagnostic messages for debugging.
    Debug = 3,
}

impl LogLevel {
    /// Returns the next level in the SIGUSR1 cycle:
    /// `Info → Debug → Error → Warn → Info`.
    pub(super) fn cycle(self) -> Self {
        match self {
            Self::Info => Self::Debug,
            Self::Debug => Self::Error,
            Self::Error => Self::Warn,
            Self::Warn => Self::Info,
        }
    }

    /// Converts a `u8` to a `LogLevel`, defaulting to `Info` for unknown values.
    pub(super) fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Error,
            1 => Self::Warn,
            2 => Self::Info,
            3 => Self::Debug,
            _ => Self::Info,
        }
    }

    /// Returns the lowercase name of this level.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            _ => Err(format!(
                "invalid log level `{s}`: expected error, warn, info, or debug"
            )),
        }
    }
}

/// Feature-flag-independent label for the active storage backend, used only
/// by the metrics subsystem to tag duration histograms.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) enum StorageBackendLabel {
    Filesystem,
    S3,
    Gcs,
    Azure,
}

/// The storage backend that determines how `Path`-based public GET requests are
/// resolved.
#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageBackend {
    /// Source images live on the local filesystem under `storage_root`.
    Filesystem,
    /// Source images live in an S3-compatible bucket.
    #[cfg(feature = "s3")]
    S3,
    /// Source images live in a Google Cloud Storage bucket.
    #[cfg(feature = "gcs")]
    Gcs,
    /// Source images live in an Azure Blob Storage container.
    #[cfg(feature = "azure")]
    Azure,
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
impl StorageBackend {
    /// Parses the `TRUSS_STORAGE_BACKEND` environment variable value.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "filesystem" | "fs" | "local" => Ok(Self::Filesystem),
            #[cfg(feature = "s3")]
            "s3" => Ok(Self::S3),
            #[cfg(feature = "gcs")]
            "gcs" => Ok(Self::Gcs),
            #[cfg(feature = "azure")]
            "azure" => Ok(Self::Azure),
            _ => {
                let mut expected = vec!["filesystem"];
                #[cfg(feature = "s3")]
                expected.push("s3");
                #[cfg(feature = "gcs")]
                expected.push("gcs");
                #[cfg(feature = "azure")]
                expected.push("azure");

                #[allow(unused_mut)]
                let mut hint = String::new();
                #[cfg(not(feature = "s3"))]
                if value.eq_ignore_ascii_case("s3") {
                    hint = " (hint: rebuild with --features s3)".to_string();
                }
                #[cfg(not(feature = "gcs"))]
                if value.eq_ignore_ascii_case("gcs") {
                    hint = " (hint: rebuild with --features gcs)".to_string();
                }
                #[cfg(not(feature = "azure"))]
                if value.eq_ignore_ascii_case("azure") {
                    hint = " (hint: rebuild with --features azure)".to_string();
                }

                Err(format!(
                    "unknown storage backend `{value}` (expected {}){hint}",
                    expected.join(" or ")
                ))
            }
        }
    }
}

/// The default bind address for the development HTTP server.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

/// The default storage root used by the server adapter.
pub const DEFAULT_STORAGE_ROOT: &str = ".";

pub(super) const DEFAULT_PUBLIC_MAX_AGE_SECONDS: u32 = 3600;
pub(super) const DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS: u32 = 60;

/// Default drain period (in seconds) during graceful shutdown.
/// Configurable at runtime via `TRUSS_SHUTDOWN_DRAIN_SECS`.
pub(super) const DEFAULT_SHUTDOWN_DRAIN_SECS: u64 = 10;

/// Default wall-clock deadline (in seconds) for server-side transforms.
/// Configurable at runtime via `TRUSS_TRANSFORM_DEADLINE_SECS`.
pub(super) const DEFAULT_TRANSFORM_DEADLINE_SECS: u64 = 30;

/// Default maximum number of input pixels allowed before decode.
/// Configurable at runtime via `TRUSS_MAX_INPUT_PIXELS`.
pub(super) const DEFAULT_MAX_INPUT_PIXELS: u64 = 40_000_000;

/// Default maximum number of requests served over a single keep-alive
/// connection before the server closes it.
/// Configurable at runtime via `TRUSS_KEEP_ALIVE_MAX_REQUESTS`.
pub(super) const DEFAULT_KEEP_ALIVE_MAX_REQUESTS: u64 = 100;

use super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES;

/// Runtime configuration for the HTTP server adapter.
///
/// The HTTP adapter keeps environment-specific concerns, such as the storage root and
/// authentication secret, outside the Core transformation API. Tests and embedding runtimes
/// can construct this value directly, while the CLI entry point typically uses
/// [`ServerConfig::from_env`] to load the same fields from process environment variables.
/// A logging callback invoked by the server for diagnostic messages.
///
/// Adapters that embed the server can supply a custom handler to route
/// messages to their preferred logging infrastructure instead of stderr.
pub type LogHandler = Arc<dyn Fn(&str) + Send + Sync>;

pub struct ServerConfig {
    /// The storage root used for `source.kind=path` lookups.
    pub storage_root: PathBuf,
    /// The expected Bearer token for private endpoints.
    pub bearer_token: Option<String>,
    /// The externally visible base URL used for public signed-URL authority.
    ///
    /// When this value is set, public signed GET requests use its authority component when
    /// reconstructing the canonical signature payload. This is primarily useful when the server
    /// runs behind a reverse proxy and the incoming `Host` header is not the externally visible
    /// authority that clients sign.
    pub public_base_url: Option<String>,
    /// The expected key identifier for public signed GET requests.
    ///
    /// Deprecated in favor of `signing_keys`. Retained for backward compatibility:
    /// when set alongside `signed_url_secret`, the pair is automatically inserted
    /// into `signing_keys`.
    pub signed_url_key_id: Option<String>,
    /// The shared secret used to verify public signed GET requests.
    ///
    /// Deprecated in favor of `signing_keys`. See `signed_url_key_id`.
    pub signed_url_secret: Option<String>,
    /// Multiple signing keys for public signed GET requests (key rotation).
    ///
    /// Each entry maps a key identifier to its HMAC shared secret. During
    /// verification the server looks up the `keyId` from the request in this
    /// map and uses the corresponding secret for HMAC validation.
    ///
    /// Configurable via `TRUSS_SIGNING_KEYS` (JSON object `{"keyId":"secret", ...}`).
    /// The legacy `TRUSS_SIGNED_URL_KEY_ID` / `TRUSS_SIGNED_URL_SECRET` pair is
    /// merged into this map automatically.
    pub signing_keys: HashMap<String, String>,
    /// Whether server-side URL sources may bypass private-network and port restrictions.
    ///
    /// This flag is intended for local development and automated tests where fixture servers
    /// commonly run on loopback addresses and non-standard ports. Production-like configurations
    /// should keep this disabled.
    pub allow_insecure_url_sources: bool,
    /// Optional directory for the on-disk transform cache.
    ///
    /// When set, transformed image bytes are cached on disk using a sharded directory layout
    /// (`ab/cd/ef/<sha256_hex>`). Repeated requests with the same source and transform options
    /// are served from the cache instead of re-transforming. When `None`, caching is disabled
    /// and every request performs a fresh transform.
    pub cache_root: Option<PathBuf>,
    /// `Cache-Control: max-age` value (in seconds) for public GET image responses.
    ///
    /// Defaults to `3600`. Operators can tune this
    /// via the `TRUSS_PUBLIC_MAX_AGE` environment variable when running behind a CDN.
    pub public_max_age_seconds: u32,
    /// `Cache-Control: stale-while-revalidate` value (in seconds) for public GET image responses.
    ///
    /// Defaults to `60`. Configurable
    /// via `TRUSS_PUBLIC_STALE_WHILE_REVALIDATE`.
    pub public_stale_while_revalidate_seconds: u32,
    /// Whether Accept-based content negotiation is disabled for public GET endpoints.
    ///
    /// When running behind a CDN such as CloudFront, Accept negotiation combined with
    /// `Vary: Accept` can cause cache key mismatches or mis-served responses if the CDN
    /// cache policy does not forward the `Accept` header.  Setting this flag to `true`
    /// disables Accept negotiation entirely: public GET requests that omit the `format`
    /// query parameter will preserve the input format instead of negotiating via Accept.
    pub disable_accept_negotiation: bool,
    /// Optional logging callback for diagnostic messages.
    ///
    /// When set, the server routes all diagnostic messages (cache errors, connection
    /// failures, transform warnings) through this handler. When `None`, messages are
    /// written to stderr via `eprintln!`.
    pub log_handler: Option<LogHandler>,
    /// Current log verbosity level.
    ///
    /// Configurable at startup via `TRUSS_LOG_LEVEL` (default: `info`).
    /// Can be changed at runtime via `SIGUSR1` (Unix only).
    pub log_level: Arc<AtomicU8>,
    /// Maximum number of concurrent image transforms.
    ///
    /// Configurable via `TRUSS_MAX_CONCURRENT_TRANSFORMS`. Defaults to 64.
    pub max_concurrent_transforms: u64,
    /// Per-transform wall-clock deadline in seconds.
    ///
    /// Configurable via `TRUSS_TRANSFORM_DEADLINE_SECS`. Defaults to 30.
    pub transform_deadline_secs: u64,
    /// Maximum number of input pixels allowed before decode.
    ///
    /// Configurable via `TRUSS_MAX_INPUT_PIXELS`. Defaults to 40,000,000 (~40 MP).
    /// Images exceeding this limit are rejected with 422 Unprocessable Entity.
    pub max_input_pixels: u64,
    /// Maximum upload body size in bytes.
    ///
    /// Configurable via `TRUSS_MAX_UPLOAD_BYTES`. Defaults to 100 MB.
    /// Requests exceeding this limit are rejected with 413 Payload Too Large.
    pub max_upload_bytes: usize,
    /// Maximum number of requests served over a single keep-alive connection.
    ///
    /// Configurable via `TRUSS_KEEP_ALIVE_MAX_REQUESTS`. Defaults to 100.
    pub keep_alive_max_requests: u64,
    /// Bearer token for the `/metrics` endpoint.
    ///
    /// When set, the `/metrics` endpoint requires `Authorization: Bearer <token>`.
    /// When absent, `/metrics` is accessible without authentication.
    /// Configurable via `TRUSS_METRICS_TOKEN`.
    pub metrics_token: Option<String>,
    /// Whether the `/metrics` endpoint is disabled.
    ///
    /// Configurable via `TRUSS_DISABLE_METRICS`. When enabled, `/metrics` returns 404.
    pub disable_metrics: bool,
    /// Minimum free bytes on the cache disk before `/health/ready` reports failure.
    ///
    /// Configurable via `TRUSS_HEALTH_CACHE_MIN_FREE_BYTES`. When unset, the cache
    /// disk free-space check is skipped.
    pub health_cache_min_free_bytes: Option<u64>,
    /// Maximum resident memory (RSS) in bytes before `/health/ready` reports failure.
    ///
    /// Configurable via `TRUSS_HEALTH_MAX_MEMORY_BYTES`. When unset, the memory
    /// check is skipped. Only effective on Linux.
    pub health_max_memory_bytes: Option<u64>,
    /// Drain period (in seconds) during graceful shutdown.
    ///
    /// On receiving a shutdown signal the server immediately marks itself as
    /// draining (causing `/health/ready` to return 503), then waits this many
    /// seconds before stopping acceptance of new connections so that load
    /// balancers have time to remove the instance from rotation.
    ///
    /// Configurable via `TRUSS_SHUTDOWN_DRAIN_SECS`. Defaults to 10.
    pub shutdown_drain_secs: u64,
    /// Runtime flag indicating the server is draining.
    ///
    /// Set to `true` upon receiving SIGTERM/SIGINT. While draining,
    /// `/health/ready` returns 503 so that load balancers stop routing traffic.
    pub draining: Arc<AtomicBool>,
    /// Custom response headers applied to all public image responses.
    ///
    /// Configurable via `TRUSS_RESPONSE_HEADERS` (JSON object `{"Header-Name": "value", ...}`).
    /// Validated at startup; invalid header names or values cause a startup error.
    pub custom_response_headers: Vec<(String, String)>,
    /// Whether gzip compression is enabled for non-image responses.
    ///
    /// Configurable via `TRUSS_DISABLE_COMPRESSION`. Defaults to `true`.
    pub enable_compression: bool,
    /// Gzip compression level (0-9). Higher values produce smaller output but
    /// use more CPU. `1` is fastest, `6` is the default (a good trade-off),
    /// and `9` is best compression.
    ///
    /// Configurable via `TRUSS_COMPRESSION_LEVEL`. Defaults to `1` (fast).
    pub compression_level: u32,
    /// Per-server counter tracking the number of image transforms currently in
    /// flight.  This is runtime state (not configuration) but lives here so that
    /// each `serve_with_config` invocation gets an independent counter, avoiding
    /// cross-server interference when multiple listeners run in the same process
    /// or during tests.
    pub transforms_in_flight: Arc<AtomicU64>,
    /// Named transform presets that can be referenced by name on public endpoints.
    ///
    /// Configurable via `TRUSS_PRESETS` (inline JSON) or `TRUSS_PRESETS_FILE` (path to JSON file).
    /// Each key is a preset name and the value is a set of transform options.
    /// Wrapped in `Arc<RwLock<...>>` to support hot-reload from `TRUSS_PRESETS_FILE`.
    pub presets: Arc<std::sync::RwLock<HashMap<String, TransformOptionsPayload>>>,
    /// Path to the presets JSON file, if configured via `TRUSS_PRESETS_FILE`.
    ///
    /// When set, a background thread watches this file for changes and reloads
    /// presets atomically. When `None` (inline `TRUSS_PRESETS` or no presets),
    /// hot-reload is disabled.
    pub presets_file_path: Option<PathBuf>,
    /// Download timeout in seconds for object storage backends (S3, GCS, Azure).
    ///
    /// Configurable via `TRUSS_STORAGE_TIMEOUT_SECS`. Defaults to 30.
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    pub storage_timeout_secs: u64,
    /// The storage backend used to resolve `Path`-based public GET requests.
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    pub storage_backend: StorageBackend,
    /// Shared S3 client context, present when `storage_backend` is `S3`.
    #[cfg(feature = "s3")]
    pub s3_context: Option<Arc<s3::S3Context>>,
    /// Shared GCS client context, present when `storage_backend` is `Gcs`.
    #[cfg(feature = "gcs")]
    pub gcs_context: Option<Arc<gcs::GcsContext>>,
    /// Shared Azure Blob Storage client context, present when `storage_backend` is `Azure`.
    #[cfg(feature = "azure")]
    pub azure_context: Option<Arc<azure::AzureContext>>,
}

impl Clone for ServerConfig {
    fn clone(&self) -> Self {
        Self {
            storage_root: self.storage_root.clone(),
            bearer_token: self.bearer_token.clone(),
            public_base_url: self.public_base_url.clone(),
            signed_url_key_id: self.signed_url_key_id.clone(),
            signed_url_secret: self.signed_url_secret.clone(),
            signing_keys: self.signing_keys.clone(),
            allow_insecure_url_sources: self.allow_insecure_url_sources,
            cache_root: self.cache_root.clone(),
            public_max_age_seconds: self.public_max_age_seconds,
            public_stale_while_revalidate_seconds: self.public_stale_while_revalidate_seconds,
            disable_accept_negotiation: self.disable_accept_negotiation,
            log_handler: self.log_handler.clone(),
            log_level: Arc::clone(&self.log_level),
            max_concurrent_transforms: self.max_concurrent_transforms,
            transform_deadline_secs: self.transform_deadline_secs,
            max_input_pixels: self.max_input_pixels,
            max_upload_bytes: self.max_upload_bytes,
            keep_alive_max_requests: self.keep_alive_max_requests,
            metrics_token: self.metrics_token.clone(),
            disable_metrics: self.disable_metrics,
            health_cache_min_free_bytes: self.health_cache_min_free_bytes,
            health_max_memory_bytes: self.health_max_memory_bytes,
            shutdown_drain_secs: self.shutdown_drain_secs,
            draining: Arc::clone(&self.draining),
            custom_response_headers: self.custom_response_headers.clone(),
            enable_compression: self.enable_compression,
            compression_level: self.compression_level,
            transforms_in_flight: Arc::clone(&self.transforms_in_flight),
            presets: Arc::clone(&self.presets),
            presets_file_path: self.presets_file_path.clone(),
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            storage_timeout_secs: self.storage_timeout_secs,
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            storage_backend: self.storage_backend,
            #[cfg(feature = "s3")]
            s3_context: self.s3_context.clone(),
            #[cfg(feature = "gcs")]
            gcs_context: self.gcs_context.clone(),
            #[cfg(feature = "azure")]
            azure_context: self.azure_context.clone(),
        }
    }
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("ServerConfig");
        d.field("storage_root", &self.storage_root)
            .field(
                "bearer_token",
                &self.bearer_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("public_base_url", &self.public_base_url)
            .field("signed_url_key_id", &self.signed_url_key_id)
            .field(
                "signed_url_secret",
                &self.signed_url_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "signing_keys",
                &self.signing_keys.keys().collect::<Vec<_>>(),
            )
            .field(
                "allow_insecure_url_sources",
                &self.allow_insecure_url_sources,
            )
            .field("cache_root", &self.cache_root)
            .field("public_max_age_seconds", &self.public_max_age_seconds)
            .field(
                "public_stale_while_revalidate_seconds",
                &self.public_stale_while_revalidate_seconds,
            )
            .field(
                "disable_accept_negotiation",
                &self.disable_accept_negotiation,
            )
            .field("log_handler", &self.log_handler.as_ref().map(|_| ".."))
            .field("log_level", &self.current_log_level())
            .field("max_concurrent_transforms", &self.max_concurrent_transforms)
            .field("transform_deadline_secs", &self.transform_deadline_secs)
            .field("max_input_pixels", &self.max_input_pixels)
            .field("max_upload_bytes", &self.max_upload_bytes)
            .field("keep_alive_max_requests", &self.keep_alive_max_requests)
            .field(
                "metrics_token",
                &self.metrics_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("disable_metrics", &self.disable_metrics)
            .field(
                "health_cache_min_free_bytes",
                &self.health_cache_min_free_bytes,
            )
            .field("health_max_memory_bytes", &self.health_max_memory_bytes)
            .field("shutdown_drain_secs", &self.shutdown_drain_secs)
            .field(
                "custom_response_headers",
                &self.custom_response_headers.len(),
            )
            .field("enable_compression", &self.enable_compression)
            .field("compression_level", &self.compression_level)
            .field(
                "presets",
                &self
                    .presets
                    .read()
                    .map(|p| p.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default(),
            )
            .field("presets_file_path", &self.presets_file_path);
        #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
        {
            d.field("storage_backend", &self.storage_backend);
        }
        #[cfg(feature = "s3")]
        {
            d.field("s3_context", &self.s3_context.as_ref().map(|_| ".."));
        }
        #[cfg(feature = "gcs")]
        {
            d.field("gcs_context", &self.gcs_context.as_ref().map(|_| ".."));
        }
        #[cfg(feature = "azure")]
        {
            d.field("azure_context", &self.azure_context.as_ref().map(|_| ".."));
        }
        d.finish()
    }
}

impl PartialEq for ServerConfig {
    fn eq(&self, other: &Self) -> bool {
        self.storage_root == other.storage_root
            && self.bearer_token == other.bearer_token
            && self.public_base_url == other.public_base_url
            && self.signed_url_key_id == other.signed_url_key_id
            && self.signed_url_secret == other.signed_url_secret
            && self.signing_keys == other.signing_keys
            && self.allow_insecure_url_sources == other.allow_insecure_url_sources
            && self.cache_root == other.cache_root
            && self.public_max_age_seconds == other.public_max_age_seconds
            && self.public_stale_while_revalidate_seconds
                == other.public_stale_while_revalidate_seconds
            && self.disable_accept_negotiation == other.disable_accept_negotiation
            && self.max_concurrent_transforms == other.max_concurrent_transforms
            && self.transform_deadline_secs == other.transform_deadline_secs
            && self.max_input_pixels == other.max_input_pixels
            && self.max_upload_bytes == other.max_upload_bytes
            && self.keep_alive_max_requests == other.keep_alive_max_requests
            && self.metrics_token == other.metrics_token
            && self.disable_metrics == other.disable_metrics
            && self.health_cache_min_free_bytes == other.health_cache_min_free_bytes
            && self.health_max_memory_bytes == other.health_max_memory_bytes
            && self.shutdown_drain_secs == other.shutdown_drain_secs
            && self.custom_response_headers == other.custom_response_headers
            && self.enable_compression == other.enable_compression
            && self.compression_level == other.compression_level
            && *self.presets.read().unwrap() == *other.presets.read().unwrap()
            && self.presets_file_path == other.presets_file_path
            && cfg_storage_eq(self, other)
    }
}

fn cfg_storage_eq(_this: &ServerConfig, _other: &ServerConfig) -> bool {
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    {
        if _this.storage_backend != _other.storage_backend {
            return false;
        }
    }
    #[cfg(feature = "s3")]
    {
        if _this
            .s3_context
            .as_ref()
            .map(|c| (&c.default_bucket, &c.endpoint_url))
            != _other
                .s3_context
                .as_ref()
                .map(|c| (&c.default_bucket, &c.endpoint_url))
        {
            return false;
        }
    }
    #[cfg(feature = "gcs")]
    {
        if _this
            .gcs_context
            .as_ref()
            .map(|c| (&c.default_bucket, &c.endpoint_url))
            != _other
                .gcs_context
                .as_ref()
                .map(|c| (&c.default_bucket, &c.endpoint_url))
        {
            return false;
        }
    }
    #[cfg(feature = "azure")]
    {
        if _this
            .azure_context
            .as_ref()
            .map(|c| (&c.default_container, &c.endpoint_url))
            != _other
                .azure_context
                .as_ref()
                .map(|c| (&c.default_container, &c.endpoint_url))
        {
            return false;
        }
    }
    true
}

impl Eq for ServerConfig {}

impl ServerConfig {
    /// Creates a server configuration from explicit values.
    ///
    /// This constructor does not canonicalize the storage root. It is primarily intended for
    /// tests and embedding scenarios where the caller already controls the filesystem layout.
    ///
    /// # Examples
    ///
    /// ```
    /// use truss::adapters::server::ServerConfig;
    ///
    /// let config = ServerConfig::new(std::env::temp_dir(), Some("secret".to_string()));
    ///
    /// assert_eq!(config.bearer_token.as_deref(), Some("secret"));
    /// ```
    pub fn new(storage_root: PathBuf, bearer_token: Option<String>) -> Self {
        Self {
            storage_root,
            bearer_token,
            public_base_url: None,
            signed_url_key_id: None,
            signed_url_secret: None,
            signing_keys: HashMap::new(),
            allow_insecure_url_sources: false,
            cache_root: None,
            public_max_age_seconds: DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            public_stale_while_revalidate_seconds: DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
            disable_accept_negotiation: false,
            log_handler: None,
            log_level: Arc::new(AtomicU8::new(LogLevel::Info as u8)),
            max_concurrent_transforms: DEFAULT_MAX_CONCURRENT_TRANSFORMS,
            transform_deadline_secs: DEFAULT_TRANSFORM_DEADLINE_SECS,
            max_input_pixels: DEFAULT_MAX_INPUT_PIXELS,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BODY_BYTES,
            keep_alive_max_requests: DEFAULT_KEEP_ALIVE_MAX_REQUESTS,
            metrics_token: None,
            disable_metrics: false,
            health_cache_min_free_bytes: None,
            health_max_memory_bytes: None,
            shutdown_drain_secs: DEFAULT_SHUTDOWN_DRAIN_SECS,
            draining: Arc::new(AtomicBool::new(false)),
            custom_response_headers: Vec::new(),
            enable_compression: true,
            compression_level: 1,
            transforms_in_flight: Arc::new(AtomicU64::new(0)),
            presets: Arc::new(std::sync::RwLock::new(HashMap::new())),
            presets_file_path: None,
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            storage_timeout_secs: STORAGE_DOWNLOAD_TIMEOUT_SECS,
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            storage_backend: StorageBackend::Filesystem,
            #[cfg(feature = "s3")]
            s3_context: None,
            #[cfg(feature = "gcs")]
            gcs_context: None,
            #[cfg(feature = "azure")]
            azure_context: None,
        }
    }

    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    pub(super) fn storage_backend_label(&self) -> StorageBackendLabel {
        #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
        {
            match self.storage_backend {
                StorageBackend::Filesystem => StorageBackendLabel::Filesystem,
                #[cfg(feature = "s3")]
                StorageBackend::S3 => StorageBackendLabel::S3,
                #[cfg(feature = "gcs")]
                StorageBackend::Gcs => StorageBackendLabel::Gcs,
                #[cfg(feature = "azure")]
                StorageBackend::Azure => StorageBackendLabel::Azure,
            }
        }
        #[cfg(not(any(feature = "s3", feature = "gcs", feature = "azure")))]
        {
            StorageBackendLabel::Filesystem
        }
    }

    /// Returns the current log level.
    pub(super) fn current_log_level(&self) -> LogLevel {
        LogLevel::from_u8(self.log_level.load(Ordering::Relaxed))
    }

    /// Emits a diagnostic message if the given `level` is at or below the
    /// currently active log level.
    pub(super) fn log_at(&self, level: LogLevel, msg: &str) {
        if level > self.current_log_level() {
            return;
        }
        if let Some(handler) = &self.log_handler {
            handler(msg);
        } else {
            stderr_write(msg);
        }
    }

    /// Emits a diagnostic message through the configured log handler, or falls
    /// back to stderr when no handler is set. Messages are emitted at
    /// [`LogLevel::Info`].
    pub(super) fn log(&self, msg: &str) {
        self.log_at(LogLevel::Info, msg);
    }

    /// Emits an error-level diagnostic message.
    #[allow(dead_code)]
    pub(super) fn log_error(&self, msg: &str) {
        self.log_at(LogLevel::Error, msg);
    }

    /// Emits a warning-level diagnostic message.
    pub(super) fn log_warn(&self, msg: &str) {
        self.log_at(LogLevel::Warn, msg);
    }

    /// Emits a debug-level diagnostic message.
    #[allow(dead_code)]
    pub(super) fn log_debug(&self, msg: &str) {
        self.log_at(LogLevel::Debug, msg);
    }

    /// Returns a copy of the configuration with signed-URL verification credentials attached.
    ///
    /// Public GET endpoints require both a key identifier and a shared secret. Tests and local
    /// development setups can use this helper to attach those values directly without going
    /// through environment variables.
    ///
    /// # Examples
    ///
    /// ```
    /// use truss::adapters::server::ServerConfig;
    ///
    /// let config = ServerConfig::new(std::env::temp_dir(), None)
    ///     .with_signed_url_credentials("public-dev", "top-secret");
    ///
    /// assert_eq!(config.signed_url_key_id.as_deref(), Some("public-dev"));
    /// assert_eq!(config.signed_url_secret.as_deref(), Some("top-secret"));
    /// ```
    pub fn with_signed_url_credentials(
        mut self,
        key_id: impl Into<String>,
        secret: impl Into<String>,
    ) -> Self {
        let key_id = key_id.into();
        let secret = secret.into();
        self.signing_keys.insert(key_id.clone(), secret.clone());
        self.signed_url_key_id = Some(key_id);
        self.signed_url_secret = Some(secret);
        self
    }

    /// Returns a copy of the configuration with multiple signing keys attached.
    ///
    /// Each entry maps a key identifier to its HMAC shared secret. During key
    /// rotation both old and new keys can be active simultaneously, allowing a
    /// graceful cutover.
    pub fn with_signing_keys(mut self, keys: HashMap<String, String>) -> Self {
        self.signing_keys.extend(keys);
        self
    }

    /// Returns a copy of the configuration with insecure URL source allowances toggled.
    ///
    /// Enabling this flag allows URL sources that target loopback or private-network addresses
    /// and permits non-standard ports. This is useful for local integration tests but weakens
    /// the default SSRF protections of the server adapter.
    ///
    /// # Examples
    ///
    /// ```
    /// use truss::adapters::server::ServerConfig;
    ///
    /// let config = ServerConfig::new(std::env::temp_dir(), Some("secret".to_string()))
    ///     .with_insecure_url_sources(true);
    ///
    /// assert!(config.allow_insecure_url_sources);
    /// ```
    pub fn with_insecure_url_sources(mut self, allow_insecure_url_sources: bool) -> Self {
        self.allow_insecure_url_sources = allow_insecure_url_sources;
        self
    }

    /// Returns a copy of the configuration with a transform cache directory set.
    ///
    /// When a cache root is configured, the server stores transformed images on disk using a
    /// sharded directory layout and serves subsequent identical requests from the cache.
    ///
    /// # Examples
    ///
    /// ```
    /// use truss::adapters::server::ServerConfig;
    ///
    /// let config = ServerConfig::new(std::env::temp_dir(), None)
    ///     .with_cache_root(std::env::temp_dir().join("truss-cache"));
    ///
    /// assert!(config.cache_root.is_some());
    /// ```
    pub fn with_cache_root(mut self, cache_root: impl Into<PathBuf>) -> Self {
        self.cache_root = Some(cache_root.into());
        self
    }

    /// Returns a copy of the configuration with an S3 storage backend attached.
    #[cfg(feature = "s3")]
    pub fn with_s3_context(mut self, context: s3::S3Context) -> Self {
        self.storage_backend = StorageBackend::S3;
        self.s3_context = Some(Arc::new(context));
        self
    }

    /// Returns a copy of the configuration with a GCS storage backend attached.
    #[cfg(feature = "gcs")]
    pub fn with_gcs_context(mut self, context: gcs::GcsContext) -> Self {
        self.storage_backend = StorageBackend::Gcs;
        self.gcs_context = Some(Arc::new(context));
        self
    }

    /// Returns a copy of the configuration with an Azure Blob Storage backend attached.
    #[cfg(feature = "azure")]
    pub fn with_azure_context(mut self, context: azure::AzureContext) -> Self {
        self.storage_backend = StorageBackend::Azure;
        self.azure_context = Some(Arc::new(context));
        self
    }

    /// Returns a copy of the configuration with named transform presets attached.
    pub fn with_presets(mut self, presets: HashMap<String, TransformOptionsPayload>) -> Self {
        self.presets = Arc::new(std::sync::RwLock::new(presets));
        self
    }

    /// Loads server configuration from environment variables.
    ///
    /// The adapter currently reads:
    ///
    /// - `TRUSS_STORAGE_ROOT`: filesystem root for `source.kind=path` inputs. Defaults to the
    ///   current directory and is canonicalized before use.
    /// - `TRUSS_BEARER_TOKEN`: private API Bearer token. When this value is missing, private
    ///   endpoints remain unavailable and return `503 Service Unavailable`.
    /// - `TRUSS_PUBLIC_BASE_URL`: externally visible base URL reserved for future public endpoint
    ///   signing. When set, it must parse as an absolute `http` or `https` URL.
    /// - `TRUSS_SIGNED_URL_KEY_ID`: key identifier accepted by public signed GET endpoints.
    /// - `TRUSS_SIGNED_URL_SECRET`: shared secret used to verify public signed GET signatures.
    /// - `TRUSS_ALLOW_INSECURE_URL_SOURCES`: when set to `1`, `true`, `yes`, or `on`, URL
    ///   sources may target loopback or private-network addresses and non-standard ports.
    /// - `TRUSS_CACHE_ROOT`: directory for the on-disk transform cache. When set, transformed
    ///   images are cached using a sharded `ab/cd/ef/<sha256>` layout. When absent, caching is
    ///   disabled.
    /// - `TRUSS_PUBLIC_MAX_AGE`: `Cache-Control: max-age` value (in seconds) for public GET
    ///   image responses. Defaults to 3600.
    /// - `TRUSS_PUBLIC_STALE_WHILE_REVALIDATE`: `Cache-Control: stale-while-revalidate` value
    ///   (in seconds) for public GET image responses. Defaults to 60.
    /// - `TRUSS_DISABLE_ACCEPT_NEGOTIATION`: when set to `1`, `true`, `yes`, or `on`, disables
    ///   Accept-based content negotiation on public GET endpoints. This is recommended when running
    ///   behind a CDN that does not forward the `Accept` header in its cache key.
    /// - `TRUSS_STORAGE_BACKEND` *(requires the `s3`, `gcs`, or `azure` feature)*: storage backend
    ///   for resolving `Path`-based public GET requests. Accepts `filesystem` (default), `s3`,
    ///   `gcs`, or `azure`.
    /// - `TRUSS_S3_BUCKET` *(requires the `s3` feature)*: default S3 bucket name. Required when
    ///   the storage backend is `s3`.
    /// - `TRUSS_S3_FORCE_PATH_STYLE` *(requires the `s3` feature)*: when set to `1`, `true`,
    ///   `yes`, or `on`, use path-style S3 addressing (`http://endpoint/bucket/key`) instead
    ///   of virtual-hosted-style. Required for S3-compatible services such as MinIO and
    ///   adobe/s3mock.
    /// - `TRUSS_GCS_BUCKET` *(requires the `gcs` feature)*: default GCS bucket name. Required
    ///   when the storage backend is `gcs`.
    /// - `TRUSS_GCS_ENDPOINT` *(requires the `gcs` feature)*: custom GCS endpoint URL. Used for
    ///   emulators such as `fake-gcs-server`. When absent, the default Google Cloud Storage
    ///   endpoint is used.
    /// - `GOOGLE_APPLICATION_CREDENTIALS`: path to a GCS service account JSON key file.
    /// - `GOOGLE_APPLICATION_CREDENTIALS_JSON`: inline GCS service account JSON (alternative to
    ///   file path).
    /// - `TRUSS_AZURE_CONTAINER` *(requires the `azure` feature)*: default Azure Blob Storage
    ///   container name. Required when the storage backend is `azure`.
    /// - `TRUSS_AZURE_ENDPOINT` *(requires the `azure` feature)*: custom Azure Blob Storage
    ///   endpoint URL. Used for emulators such as Azurite. When absent, the endpoint is derived
    ///   from `AZURE_STORAGE_ACCOUNT_NAME`.
    /// - `AZURE_STORAGE_ACCOUNT_NAME`: Azure storage account name (used to derive the default
    ///   endpoint when `TRUSS_AZURE_ENDPOINT` is not set).
    /// - `TRUSS_MAX_CONCURRENT_TRANSFORMS`: maximum number of concurrent image transforms
    ///   (default: 64, range: 1–1024). Requests exceeding this limit are rejected with 503.
    /// - `TRUSS_TRANSFORM_DEADLINE_SECS`: per-transform wall-clock deadline in seconds
    ///   (default: 30, range: 1–300). Transforms exceeding this deadline are cancelled.
    /// - `TRUSS_MAX_INPUT_PIXELS`: maximum number of input image pixels allowed before decode
    ///   (default: 40,000,000, range: 1–100,000,000). Images exceeding this limit are rejected
    ///   with 422 Unprocessable Entity.
    /// - `TRUSS_MAX_UPLOAD_BYTES`: maximum upload body size in bytes (default: 104,857,600 = 100 MB,
    ///   range: 1–10,737,418,240). Requests exceeding this limit are rejected with 413.
    /// - `TRUSS_METRICS_TOKEN`: Bearer token for the `/metrics` endpoint. When set, the endpoint
    ///   requires `Authorization: Bearer <token>`. When absent, no authentication is required.
    /// - `TRUSS_DISABLE_METRICS`: when set to `1`, `true`, `yes`, or `on`, disables the `/metrics`
    ///   endpoint entirely (returns 404).
    /// - `TRUSS_STORAGE_TIMEOUT_SECS`: download timeout for storage backends in seconds
    ///   (default: 30, range: 1–300).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] when the configured storage root does not exist or cannot be
    /// canonicalized.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// // SAFETY: This example runs single-threaded; no concurrent env access.
    /// unsafe {
    ///     std::env::set_var("TRUSS_STORAGE_ROOT", ".");
    ///     std::env::set_var("TRUSS_ALLOW_INSECURE_URL_SOURCES", "true");
    /// }
    ///
    /// let config = truss::adapters::server::ServerConfig::from_env().unwrap();
    ///
    /// assert!(config.storage_root.is_absolute());
    /// assert!(config.allow_insecure_url_sources);
    /// ```
    pub fn from_env() -> io::Result<Self> {
        #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
        let storage_backend = match env::var("TRUSS_STORAGE_BACKEND")
            .ok()
            .filter(|v| !v.is_empty())
        {
            Some(value) => StorageBackend::parse(&value)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
            None => StorageBackend::Filesystem,
        };

        let storage_root =
            env::var("TRUSS_STORAGE_ROOT").unwrap_or_else(|_| DEFAULT_STORAGE_ROOT.to_string());
        let storage_root = PathBuf::from(storage_root).canonicalize()?;
        let bearer_token = env::var("TRUSS_BEARER_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        let public_base_url = env::var("TRUSS_PUBLIC_BASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
            .map(validate_public_base_url)
            .transpose()?;
        let signed_url_key_id = env::var("TRUSS_SIGNED_URL_KEY_ID")
            .ok()
            .filter(|value| !value.is_empty());
        let signed_url_secret = env::var("TRUSS_SIGNED_URL_SECRET")
            .ok()
            .filter(|value| !value.is_empty());

        if signed_url_key_id.is_some() != signed_url_secret.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TRUSS_SIGNED_URL_KEY_ID and TRUSS_SIGNED_URL_SECRET must be set together",
            ));
        }

        let mut signing_keys = HashMap::new();
        if let (Some(kid), Some(sec)) = (&signed_url_key_id, &signed_url_secret) {
            signing_keys.insert(kid.clone(), sec.clone());
        }
        if let Ok(json) = env::var("TRUSS_SIGNING_KEYS")
            && !json.is_empty()
        {
            let extra: HashMap<String, String> = serde_json::from_str(&json).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("TRUSS_SIGNING_KEYS must be valid JSON: {e}"),
                )
            })?;
            for (kid, sec) in &extra {
                if kid.is_empty() || sec.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "TRUSS_SIGNING_KEYS must not contain empty key IDs or secrets",
                    ));
                }
            }
            signing_keys.extend(extra);
        }

        if !signing_keys.is_empty() && public_base_url.is_none() {
            eprintln!(
                "truss: warning: signing keys are configured but TRUSS_PUBLIC_BASE_URL is not. \
                 Behind a reverse proxy or CDN the Host header may differ from the externally \
                 visible authority, causing signed URL verification to fail. Consider setting \
                 TRUSS_PUBLIC_BASE_URL to the canonical external origin."
            );
        }

        let cache_root = env::var("TRUSS_CACHE_ROOT")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);

        let public_max_age_seconds = parse_optional_env_u32("TRUSS_PUBLIC_MAX_AGE")?
            .unwrap_or(DEFAULT_PUBLIC_MAX_AGE_SECONDS);
        let public_stale_while_revalidate_seconds =
            parse_optional_env_u32("TRUSS_PUBLIC_STALE_WHILE_REVALIDATE")?
                .unwrap_or(DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS);

        let allow_insecure_url_sources = env_flag("TRUSS_ALLOW_INSECURE_URL_SOURCES");

        let max_concurrent_transforms =
            parse_env_u64_ranged("TRUSS_MAX_CONCURRENT_TRANSFORMS", 1, 1024)?
                .unwrap_or(DEFAULT_MAX_CONCURRENT_TRANSFORMS);

        let transform_deadline_secs =
            parse_env_u64_ranged("TRUSS_TRANSFORM_DEADLINE_SECS", 1, 300)?
                .unwrap_or(DEFAULT_TRANSFORM_DEADLINE_SECS);

        let max_input_pixels =
            parse_env_u64_ranged("TRUSS_MAX_INPUT_PIXELS", 1, crate::MAX_DECODED_PIXELS)?
                .unwrap_or(DEFAULT_MAX_INPUT_PIXELS);

        let max_upload_bytes =
            parse_env_u64_ranged("TRUSS_MAX_UPLOAD_BYTES", 1, 10 * 1024 * 1024 * 1024)?
                .unwrap_or(DEFAULT_MAX_UPLOAD_BODY_BYTES as u64) as usize;

        let keep_alive_max_requests =
            parse_env_u64_ranged("TRUSS_KEEP_ALIVE_MAX_REQUESTS", 1, 100_000)?
                .unwrap_or(DEFAULT_KEEP_ALIVE_MAX_REQUESTS);

        #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
        let storage_timeout_secs = parse_env_u64_ranged("TRUSS_STORAGE_TIMEOUT_SECS", 1, 300)?
            .unwrap_or(STORAGE_DOWNLOAD_TIMEOUT_SECS);

        #[cfg(feature = "s3")]
        let s3_context = if storage_backend == StorageBackend::S3 {
            let bucket = env::var("TRUSS_S3_BUCKET")
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "TRUSS_S3_BUCKET is required when TRUSS_STORAGE_BACKEND=s3",
                    )
                })?;
            Some(Arc::new(s3::build_s3_context(
                bucket,
                allow_insecure_url_sources,
            )?))
        } else {
            None
        };

        #[cfg(feature = "gcs")]
        let gcs_context = if storage_backend == StorageBackend::Gcs {
            let bucket = env::var("TRUSS_GCS_BUCKET")
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "TRUSS_GCS_BUCKET is required when TRUSS_STORAGE_BACKEND=gcs",
                    )
                })?;
            Some(Arc::new(gcs::build_gcs_context(
                bucket,
                allow_insecure_url_sources,
            )?))
        } else {
            if env::var("TRUSS_GCS_BUCKET")
                .ok()
                .filter(|v| !v.is_empty())
                .is_some()
            {
                eprintln!(
                    "truss: warning: TRUSS_GCS_BUCKET is set but TRUSS_STORAGE_BACKEND is not \
                     `gcs`. The GCS bucket will be ignored. Set TRUSS_STORAGE_BACKEND=gcs to \
                     enable the GCS backend."
                );
            }
            None
        };

        #[cfg(feature = "azure")]
        let azure_context = if storage_backend == StorageBackend::Azure {
            let container = env::var("TRUSS_AZURE_CONTAINER")
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "TRUSS_AZURE_CONTAINER is required when TRUSS_STORAGE_BACKEND=azure",
                    )
                })?;
            Some(Arc::new(azure::build_azure_context(
                container,
                allow_insecure_url_sources,
            )?))
        } else {
            if env::var("TRUSS_AZURE_CONTAINER")
                .ok()
                .filter(|v| !v.is_empty())
                .is_some()
            {
                eprintln!(
                    "truss: warning: TRUSS_AZURE_CONTAINER is set but TRUSS_STORAGE_BACKEND is not \
                     `azure`. The Azure container will be ignored. Set TRUSS_STORAGE_BACKEND=azure to \
                     enable the Azure backend."
                );
            }
            None
        };

        let metrics_token = env::var("TRUSS_METRICS_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        let disable_metrics = env_flag("TRUSS_DISABLE_METRICS");

        let health_cache_min_free_bytes =
            parse_env_u64_ranged("TRUSS_HEALTH_CACHE_MIN_FREE_BYTES", 1, u64::MAX)?;
        let health_max_memory_bytes =
            parse_env_u64_ranged("TRUSS_HEALTH_MAX_MEMORY_BYTES", 1, u64::MAX)?;

        let (presets, presets_file_path) = parse_presets_from_env()?;

        let shutdown_drain_secs = parse_env_u64_ranged("TRUSS_SHUTDOWN_DRAIN_SECS", 0, 300)?
            .unwrap_or(DEFAULT_SHUTDOWN_DRAIN_SECS);

        let custom_response_headers = parse_response_headers_from_env()?;

        let enable_compression = !env_flag("TRUSS_DISABLE_COMPRESSION");
        let compression_level =
            parse_env_u64_ranged("TRUSS_COMPRESSION_LEVEL", 0, 9)?.unwrap_or(1) as u32;

        let log_level = match env::var("TRUSS_LOG_LEVEL")
            .ok()
            .filter(|v| !v.is_empty())
        {
            Some(val) => val.parse::<LogLevel>().map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, e)
            })?,
            None => LogLevel::Info,
        };

        Ok(Self {
            storage_root,
            bearer_token,
            public_base_url,
            signed_url_key_id,
            signed_url_secret,
            signing_keys,
            allow_insecure_url_sources,
            cache_root,
            public_max_age_seconds,
            public_stale_while_revalidate_seconds,
            disable_accept_negotiation: env_flag("TRUSS_DISABLE_ACCEPT_NEGOTIATION"),
            log_handler: None,
            log_level: Arc::new(AtomicU8::new(log_level as u8)),
            max_concurrent_transforms,
            transform_deadline_secs,
            max_input_pixels,
            max_upload_bytes,
            keep_alive_max_requests,
            metrics_token,
            disable_metrics,
            health_cache_min_free_bytes,
            health_max_memory_bytes,
            shutdown_drain_secs,
            draining: Arc::new(AtomicBool::new(false)),
            custom_response_headers,
            enable_compression,
            compression_level,
            transforms_in_flight: Arc::new(AtomicU64::new(0)),
            presets: Arc::new(std::sync::RwLock::new(presets)),
            presets_file_path,
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            storage_timeout_secs,
            #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
            storage_backend,
            #[cfg(feature = "s3")]
            s3_context,
            #[cfg(feature = "gcs")]
            gcs_context,
            #[cfg(feature = "azure")]
            azure_context,
        })
    }
}

/// Parse an optional environment variable as `u64`, validating that its value
/// falls within `[min, max]`. Returns `Ok(None)` when the variable is unset or
/// empty, `Ok(Some(value))` on success, or an `io::Error` on parse / range
/// failure.
pub(super) fn parse_env_u64_ranged(name: &str, min: u64, max: u64) -> io::Result<Option<u64>> {
    match env::var(name).ok().filter(|v| !v.is_empty()) {
        Some(value) => {
            let n: u64 = value.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must be a positive integer"),
                )
            })?;
            if n < min || n > max {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{name} must be between {min} and {max}"),
                ));
            }
            Ok(Some(n))
        }
        None => Ok(None),
    }
}

pub(super) fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

pub(super) fn parse_optional_env_u32(name: &str) -> io::Result<Option<u32>> {
    match env::var(name) {
        Ok(value) if !value.is_empty() => value.parse::<u32>().map(Some).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be a non-negative integer"),
            )
        }),
        _ => Ok(None),
    }
}

/// Parses presets from environment variables, returning both the preset map
/// and the file path (if loaded from `TRUSS_PRESETS_FILE`).
pub(super) fn parse_presets_from_env(
) -> io::Result<(HashMap<String, TransformOptionsPayload>, Option<PathBuf>)> {
    let (json_str, source, file_path) = match env::var("TRUSS_PRESETS_FILE")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(path) => {
            let content = std::fs::read_to_string(&path).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("failed to read TRUSS_PRESETS_FILE `{path}`: {e}"),
                )
            })?;
            let pb = PathBuf::from(&path);
            (content, format!("TRUSS_PRESETS_FILE `{path}`"), Some(pb))
        }
        None => match env::var("TRUSS_PRESETS").ok().filter(|v| !v.is_empty()) {
            Some(value) => (value, "TRUSS_PRESETS".to_string(), None),
            None => return Ok((HashMap::new(), None)),
        },
    };

    let presets =
        serde_json::from_str::<HashMap<String, TransformOptionsPayload>>(&json_str).map_err(
            |e| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{source} must be valid JSON: {e}"),
                )
            },
        )?;
    Ok((presets, file_path))
}

/// Parses a preset JSON file at the given path. Used by the hot-reload watcher.
pub(super) fn parse_presets_file(
    path: &std::path::Path,
) -> io::Result<HashMap<String, TransformOptionsPayload>> {
    let content = std::fs::read_to_string(path)?;
    serde_json::from_str::<HashMap<String, TransformOptionsPayload>>(&content).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid preset JSON in `{}`: {e}", path.display()),
        )
    })
}

/// Parse `TRUSS_RESPONSE_HEADERS` (a JSON object `{"Header-Name": "value", ...}`) and
/// validate that every name and value conforms to RFC 7230. Returns an empty vec when the
/// variable is unset or empty.
fn parse_response_headers_from_env() -> io::Result<Vec<(String, String)>> {
    let raw = match env::var("TRUSS_RESPONSE_HEADERS")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(value) => value,
        None => return Ok(Vec::new()),
    };

    let map: HashMap<String, String> = serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("TRUSS_RESPONSE_HEADERS must be a JSON object: {e}"),
        )
    })?;

    let mut headers = Vec::with_capacity(map.len());
    for (name, value) in map {
        validate_header_name(&name)?;
        reject_denied_header(&name)?;
        validate_header_value(&name, &value)?;
        headers.push((name, value));
    }
    // Sort for deterministic ordering in responses.
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(headers)
}

/// Validate an HTTP header name per RFC 7230 §3.2.6 (token characters).
fn validate_header_name(name: &str) -> io::Result<()> {
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TRUSS_RESPONSE_HEADERS: header name must not be empty",
        ));
    }
    // token = 1*tchar
    // tchar = "!" / "#" / "$" / "%" / "&" / "'" / "*" / "+" / "-" / "." /
    //         "^" / "_" / "`" / "|" / "~" / DIGIT / ALPHA
    for byte in name.bytes() {
        let valid = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            );
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("TRUSS_RESPONSE_HEADERS: invalid character in header name `{name}`"),
            ));
        }
    }
    Ok(())
}

/// Validate an HTTP header value per RFC 7230 §3.2.6 (visible ASCII + SP + HTAB).
fn validate_header_value(name: &str, value: &str) -> io::Result<()> {
    for byte in value.bytes() {
        let valid = byte == b'\t' || (0x20..=0x7E).contains(&byte);
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("TRUSS_RESPONSE_HEADERS: invalid character in value for header `{name}`"),
            ));
        }
    }
    Ok(())
}

/// Reject HTTP framing and hop-by-hop headers that must not be overridden by
/// operator configuration. Allowing these would risk HTTP response smuggling,
/// MIME-sniffing attacks, or broken connection handling.
fn reject_denied_header(name: &str) -> io::Result<()> {
    const DENIED: &[&str] = &[
        "content-length",
        "transfer-encoding",
        "content-encoding",
        "content-type",
        "connection",
        "host",
        "upgrade",
        "proxy-connection",
        "keep-alive",
        "te",
        "trailer",
    ];
    let lower = name.to_ascii_lowercase();
    if DENIED.contains(&lower.as_str()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "TRUSS_RESPONSE_HEADERS: header `{name}` is not allowed (framing/hop-by-hop header)"
            ),
        ));
    }
    Ok(())
}

pub(super) fn validate_public_base_url(value: String) -> io::Result<String> {
    let parsed = Url::parse(&value).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("TRUSS_PUBLIC_BASE_URL must be a valid URL: {error}"),
        )
    })?;

    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TRUSS_PUBLIC_BASE_URL must use http or https",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn keep_alive_default() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert_eq!(config.keep_alive_max_requests, 100);
    }

    #[test]
    #[serial]
    fn parse_keep_alive_env_valid() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS", "500") };
        let result = parse_env_u64_ranged("TRUSS_KEEP_ALIVE_MAX_REQUESTS", 1, 100_000);
        unsafe { env::remove_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS") };
        assert_eq!(result.unwrap(), Some(500));
    }

    #[test]
    #[serial]
    fn parse_keep_alive_env_zero_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS", "0") };
        let result = parse_env_u64_ranged("TRUSS_KEEP_ALIVE_MAX_REQUESTS", 1, 100_000);
        unsafe { env::remove_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS") };
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn parse_keep_alive_env_over_max_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS", "100001") };
        let result = parse_env_u64_ranged("TRUSS_KEEP_ALIVE_MAX_REQUESTS", 1, 100_000);
        unsafe { env::remove_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS") };
        assert!(result.is_err());
    }

    #[test]
    fn health_thresholds_default_none() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert!(config.health_cache_min_free_bytes.is_none());
        assert!(config.health_max_memory_bytes.is_none());
    }

    #[test]
    #[serial]
    fn parse_health_cache_min_free_bytes_valid() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_HEALTH_CACHE_MIN_FREE_BYTES", "1073741824") };
        let result = parse_env_u64_ranged("TRUSS_HEALTH_CACHE_MIN_FREE_BYTES", 1, u64::MAX);
        unsafe { env::remove_var("TRUSS_HEALTH_CACHE_MIN_FREE_BYTES") };
        assert_eq!(result.unwrap(), Some(1_073_741_824));
    }

    #[test]
    #[serial]
    fn parse_health_max_memory_bytes_valid() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_HEALTH_MAX_MEMORY_BYTES", "536870912") };
        let result = parse_env_u64_ranged("TRUSS_HEALTH_MAX_MEMORY_BYTES", 1, u64::MAX);
        unsafe { env::remove_var("TRUSS_HEALTH_MAX_MEMORY_BYTES") };
        assert_eq!(result.unwrap(), Some(536_870_912));
    }

    #[test]
    #[serial]
    fn parse_health_threshold_zero_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_HEALTH_CACHE_MIN_FREE_BYTES", "0") };
        let result = parse_env_u64_ranged("TRUSS_HEALTH_CACHE_MIN_FREE_BYTES", 1, u64::MAX);
        unsafe { env::remove_var("TRUSS_HEALTH_CACHE_MIN_FREE_BYTES") };
        assert!(result.is_err());
    }

    // ── shutdown_drain_secs ────────────────────────────────────────

    #[test]
    fn shutdown_drain_secs_default() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert_eq!(config.shutdown_drain_secs, DEFAULT_SHUTDOWN_DRAIN_SECS);
    }

    #[test]
    fn draining_default_false() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert!(!config.draining.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    #[serial]
    fn parse_shutdown_drain_secs_valid() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_SHUTDOWN_DRAIN_SECS", "30") };
        let result = parse_env_u64_ranged("TRUSS_SHUTDOWN_DRAIN_SECS", 0, 300);
        unsafe { env::remove_var("TRUSS_SHUTDOWN_DRAIN_SECS") };
        assert_eq!(result.unwrap(), Some(30));
    }

    #[test]
    #[serial]
    fn parse_shutdown_drain_secs_over_max_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_SHUTDOWN_DRAIN_SECS", "301") };
        let result = parse_env_u64_ranged("TRUSS_SHUTDOWN_DRAIN_SECS", 0, 300);
        unsafe { env::remove_var("TRUSS_SHUTDOWN_DRAIN_SECS") };
        assert!(result.is_err());
    }

    // ── presets ────────────────────────────────────────────────────

    #[test]
    fn presets_default_empty() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert!(config.presets.read().unwrap().is_empty());
        assert!(config.presets_file_path.is_none());
    }

    #[test]
    fn parse_presets_file_valid() {
        let dir = std::env::temp_dir().join(format!(
            "truss_test_presets_{}",
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("presets.json");
        std::fs::write(
            &path,
            r#"{"thumb":{"width":100,"height":100},"banner":{"width":1200}}"#,
        )
        .unwrap();

        let presets = super::parse_presets_file(&path).unwrap();
        assert_eq!(presets.len(), 2);
        assert_eq!(presets["thumb"].width, Some(100));
        assert_eq!(presets["thumb"].height, Some(100));
        assert_eq!(presets["banner"].width, Some(1200));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_presets_file_invalid_json() {
        let dir = std::env::temp_dir().join(format!(
            "truss_test_presets_invalid_{}",
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "not valid json {{{").unwrap();

        let result = super::parse_presets_file(&path);
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_presets_file_nonexistent() {
        let result = super::parse_presets_file(std::path::Path::new("/tmp/nonexistent_truss_test.json"));
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn parse_presets_from_env_returns_file_path() {
        let dir = std::env::temp_dir().join(format!(
            "truss_test_presets_path_{}",
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("presets.json");
        std::fs::write(&path, r#"{"thumb":{"width":100}}"#).unwrap();

        // SAFETY: test-only, single-threaded access to this env var.
        unsafe {
            env::set_var("TRUSS_PRESETS_FILE", path.to_str().unwrap());
            env::remove_var("TRUSS_PRESETS");
        }
        let (presets, file_path) = super::parse_presets_from_env().unwrap();
        unsafe {
            env::remove_var("TRUSS_PRESETS_FILE");
        }

        assert_eq!(presets.len(), 1);
        assert_eq!(file_path, Some(path));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn with_presets_sets_presets() {
        let mut map = HashMap::new();
        map.insert(
            "test".to_string(),
            super::super::TransformOptionsPayload {
                width: Some(200),
                height: None,
                fit: None,
                position: None,
                format: None,
                quality: None,
                background: None,
                rotate: None,
                auto_orient: None,
                strip_metadata: None,
                preserve_exif: None,
                crop: None,
                blur: None,
                sharpen: None,
            },
        );
        let config = ServerConfig::new(PathBuf::from("."), None).with_presets(map);
        let presets = config.presets.read().unwrap();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets["test"].width, Some(200));
    }

    // ── custom_response_headers ────────────────────────────────────

    #[test]
    fn custom_response_headers_default_empty() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert!(config.custom_response_headers.is_empty());
    }

    #[test]
    #[serial]
    fn parse_response_headers_valid_json() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe {
            env::set_var(
                "TRUSS_RESPONSE_HEADERS",
                r#"{"CDN-Cache-Control":"max-age=3600","X-Custom":"value"}"#,
            )
        };
        let result = parse_response_headers_from_env();
        unsafe { env::remove_var("TRUSS_RESPONSE_HEADERS") };
        let headers = result.unwrap();
        assert_eq!(headers.len(), 2);
        // Sorted by name.
        assert_eq!(headers[0].0, "CDN-Cache-Control");
        assert_eq!(headers[0].1, "max-age=3600");
        assert_eq!(headers[1].0, "X-Custom");
        assert_eq!(headers[1].1, "value");
    }

    #[test]
    #[serial]
    fn parse_response_headers_invalid_json() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_RESPONSE_HEADERS", "not json") };
        let result = parse_response_headers_from_env();
        unsafe { env::remove_var("TRUSS_RESPONSE_HEADERS") };
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn parse_response_headers_empty_name_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_RESPONSE_HEADERS", r#"{"":"value"}"#) };
        let result = parse_response_headers_from_env();
        unsafe { env::remove_var("TRUSS_RESPONSE_HEADERS") };
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn parse_response_headers_invalid_name_character() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_RESPONSE_HEADERS", r#"{"Bad Header":"value"}"#) };
        let result = parse_response_headers_from_env();
        unsafe { env::remove_var("TRUSS_RESPONSE_HEADERS") };
        assert!(result.is_err());
    }

    #[test]
    #[serial]
    fn parse_response_headers_invalid_value_character() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_RESPONSE_HEADERS", "{\"X-Bad\":\"val\\u0000ue\"}") };
        let result = parse_response_headers_from_env();
        unsafe { env::remove_var("TRUSS_RESPONSE_HEADERS") };
        assert!(result.is_err());
    }

    #[test]
    fn validate_header_name_valid() {
        assert!(super::validate_header_name("Cache-Control").is_ok());
        assert!(super::validate_header_name("X-Custom-Header").is_ok());
        assert!(super::validate_header_name("CDN-Cache-Control").is_ok());
    }

    #[test]
    fn validate_header_name_rejects_space() {
        assert!(super::validate_header_name("Bad Header").is_err());
    }

    #[test]
    fn validate_header_name_rejects_empty() {
        assert!(super::validate_header_name("").is_err());
    }

    #[test]
    fn validate_header_value_valid() {
        assert!(super::validate_header_value("X", "normal value").is_ok());
        assert!(super::validate_header_value("X", "max-age=3600, public").is_ok());
    }

    #[test]
    fn validate_header_value_rejects_null() {
        assert!(super::validate_header_value("X", "bad\x00value").is_err());
    }

    // ── enable_compression ─────────────────────────────────────────

    #[test]
    fn compression_enabled_by_default() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert!(config.enable_compression);
    }

    // ── log_level ─────────────────────────────────────────────────────

    #[test]
    fn log_level_default_info() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert_eq!(config.current_log_level(), LogLevel::Info);
    }

    #[test]
    fn log_level_cycle() {
        assert_eq!(LogLevel::Info.cycle(), LogLevel::Debug);
        assert_eq!(LogLevel::Debug.cycle(), LogLevel::Error);
        assert_eq!(LogLevel::Error.cycle(), LogLevel::Warn);
        assert_eq!(LogLevel::Warn.cycle(), LogLevel::Info);
    }

    #[test]
    fn log_level_from_str() {
        assert_eq!("error".parse::<LogLevel>().unwrap(), LogLevel::Error);
        assert_eq!("WARN".parse::<LogLevel>().unwrap(), LogLevel::Warn);
        assert_eq!("Info".parse::<LogLevel>().unwrap(), LogLevel::Info);
        assert_eq!("DEBUG".parse::<LogLevel>().unwrap(), LogLevel::Debug);
        assert!("invalid".parse::<LogLevel>().is_err());
    }

    #[test]
    fn log_level_display() {
        assert_eq!(LogLevel::Error.to_string(), "error");
        assert_eq!(LogLevel::Warn.to_string(), "warn");
        assert_eq!(LogLevel::Info.to_string(), "info");
        assert_eq!(LogLevel::Debug.to_string(), "debug");
    }

    #[test]
    fn log_level_from_u8_roundtrip() {
        for level in [LogLevel::Error, LogLevel::Warn, LogLevel::Info, LogLevel::Debug] {
            assert_eq!(LogLevel::from_u8(level as u8), level);
        }
        // Unknown values default to Info.
        assert_eq!(LogLevel::from_u8(42), LogLevel::Info);
    }

    #[test]
    #[serial]
    fn parse_log_level_from_env() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_LOG_LEVEL", "debug") };
        let config = ServerConfig::from_env().unwrap();
        unsafe { env::remove_var("TRUSS_LOG_LEVEL") };
        assert_eq!(config.current_log_level(), LogLevel::Debug);
    }

    #[test]
    #[serial]
    fn parse_log_level_invalid_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_LOG_LEVEL", "verbose") };
        let result = ServerConfig::from_env();
        unsafe { env::remove_var("TRUSS_LOG_LEVEL") };
        assert!(result.is_err());
    }

    #[test]
    fn log_at_filters_by_level() {
        use std::sync::Mutex;

        let messages: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let msgs = Arc::clone(&messages);
        let handler: LogHandler = Arc::new(move |msg: &str| {
            msgs.lock().unwrap().push(msg.to_string());
        });

        let mut config = ServerConfig::new(PathBuf::from("."), None);
        config.log_handler = Some(handler);
        // Set level to Warn — only Error and Warn should pass through.
        config.log_level.store(LogLevel::Warn as u8, std::sync::atomic::Ordering::Relaxed);

        config.log_at(LogLevel::Error, "err");
        config.log_at(LogLevel::Warn, "wrn");
        config.log_at(LogLevel::Info, "inf");
        config.log_at(LogLevel::Debug, "dbg");

        let logged = messages.lock().unwrap();
        assert_eq!(*logged, vec!["err", "wrn"]);
    }
}
