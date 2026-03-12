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
use std::sync::atomic::AtomicU64;
use url::Url;

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
    pub presets: HashMap<String, TransformOptionsPayload>,
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
            max_concurrent_transforms: self.max_concurrent_transforms,
            transform_deadline_secs: self.transform_deadline_secs,
            max_input_pixels: self.max_input_pixels,
            max_upload_bytes: self.max_upload_bytes,
            keep_alive_max_requests: self.keep_alive_max_requests,
            metrics_token: self.metrics_token.clone(),
            disable_metrics: self.disable_metrics,
            transforms_in_flight: Arc::clone(&self.transforms_in_flight),
            presets: self.presets.clone(),
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
            .field("presets", &self.presets.keys().collect::<Vec<_>>());
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
            && self.presets == other.presets
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
            max_concurrent_transforms: DEFAULT_MAX_CONCURRENT_TRANSFORMS,
            transform_deadline_secs: DEFAULT_TRANSFORM_DEADLINE_SECS,
            max_input_pixels: DEFAULT_MAX_INPUT_PIXELS,
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BODY_BYTES,
            keep_alive_max_requests: DEFAULT_KEEP_ALIVE_MAX_REQUESTS,
            metrics_token: None,
            disable_metrics: false,
            transforms_in_flight: Arc::new(AtomicU64::new(0)),
            presets: HashMap::new(),
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

    /// Emits a diagnostic message through the configured log handler, or falls
    /// back to stderr when no handler is set.
    pub(super) fn log(&self, msg: &str) {
        if let Some(handler) = &self.log_handler {
            handler(msg);
        } else {
            stderr_write(msg);
        }
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
        self.presets = presets;
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

        let presets = parse_presets_from_env()?;

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
            max_concurrent_transforms,
            transform_deadline_secs,
            max_input_pixels,
            max_upload_bytes,
            keep_alive_max_requests,
            metrics_token,
            disable_metrics,
            transforms_in_flight: Arc::new(AtomicU64::new(0)),
            presets,
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

pub(super) fn parse_presets_from_env() -> io::Result<HashMap<String, TransformOptionsPayload>> {
    let (json_str, source) = match env::var("TRUSS_PRESETS_FILE")
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
            (content, format!("TRUSS_PRESETS_FILE `{path}`"))
        }
        None => match env::var("TRUSS_PRESETS").ok().filter(|v| !v.is_empty()) {
            Some(value) => (value, "TRUSS_PRESETS".to_string()),
            None => return Ok(HashMap::new()),
        },
    };

    serde_json::from_str::<HashMap<String, TransformOptionsPayload>>(&json_str).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{source} must be valid JSON: {e}"),
        )
    })
}

/// Validate that the `TRUSS_KEEP_ALIVE_MAX_REQUESTS` environment variable is
/// correctly loaded into [`ServerConfig`].
///
/// ```
/// # use std::path::PathBuf;
/// let config = truss::ServerConfig::new(PathBuf::from("."), None);
/// assert_eq!(config.keep_alive_max_requests, 100);
/// ```
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

    #[test]
    fn keep_alive_default() {
        let config = ServerConfig::new(PathBuf::from("."), None);
        assert_eq!(config.keep_alive_max_requests, 100);
    }

    #[test]
    fn parse_keep_alive_env_valid() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS", "500") };
        let result = parse_env_u64_ranged("TRUSS_KEEP_ALIVE_MAX_REQUESTS", 1, 100_000);
        unsafe { env::remove_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS") };
        assert_eq!(result.unwrap(), Some(500));
    }

    #[test]
    fn parse_keep_alive_env_zero_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS", "0") };
        let result = parse_env_u64_ranged("TRUSS_KEEP_ALIVE_MAX_REQUESTS", 1, 100_000);
        unsafe { env::remove_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS") };
        assert!(result.is_err());
    }

    #[test]
    fn parse_keep_alive_env_over_max_rejected() {
        // SAFETY: test-only, single-threaded access to this env var.
        unsafe { env::set_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS", "100001") };
        let result = parse_env_u64_ranged("TRUSS_KEEP_ALIVE_MAX_REQUESTS", 1, 100_000);
        unsafe { env::remove_var("TRUSS_KEEP_ALIVE_MAX_REQUESTS") };
        assert!(result.is_err());
    }
}
