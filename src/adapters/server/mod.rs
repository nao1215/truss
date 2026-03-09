mod auth;
mod cache;
mod http_parse;
mod metrics;
mod multipart;
mod negotiate;
mod remote;
mod response;
#[cfg(feature = "s3")]
pub mod s3;

use auth::{
    authorize_request, authorize_request_headers, authorize_signed_request,
    canonical_query_without_signature, extend_transform_query, parse_optional_bool_query,
    parse_optional_float_query, parse_optional_integer_query, parse_optional_u8_query,
    parse_query_params, required_query_param, signed_source_query, url_authority,
    validate_public_query_names,
};
use cache::{CacheLookup, TransformCache, compute_cache_key, try_versioned_cache_lookup};
use http_parse::{
    HttpRequest, parse_named, parse_optional_named, read_request_body, read_request_headers,
    request_has_json_content_type,
};
use metrics::{
    CACHE_HITS_TOTAL, CACHE_MISSES_TOTAL, MAX_CONCURRENT_TRANSFORMS, RouteMetric,
    TRANSFORMS_IN_FLIGHT, record_http_metrics, render_metrics_text, uptime_seconds,
};
use multipart::{parse_multipart_boundary, parse_upload_request};
use negotiate::{
    CacheHitStatus, ImageResponsePolicy, PublicSourceKind, build_image_etag,
    build_image_response_headers, if_none_match_matches, negotiate_output_format,
};
use remote::resolve_source_bytes;
use response::{
    HttpResponse, NOT_FOUND_BODY, bad_request_response, service_unavailable_response,
    transform_error_response, unsupported_media_type_response, write_response,
};

use crate::{
    Fit, MediaType, Position, RawArtifact, Rgba8, Rotation, TransformOptions, TransformRequest,
    sniff_artifact, transform_raster, transform_svg,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use url::Url;

/// The default bind address for the development HTTP server.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

/// The default storage root used by the server adapter.
pub const DEFAULT_STORAGE_ROOT: &str = ".";

const DEFAULT_PUBLIC_MAX_AGE_SECONDS: u32 = 3600;
const DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS: u32 = 60;
const SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(60);
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Number of worker threads for handling incoming connections concurrently.
const WORKER_THREADS: usize = 8;
type HmacSha256 = Hmac<Sha256>;

/// Maximum number of requests served over a single keep-alive connection before
/// the server closes it.  This prevents a single client from monopolising a
/// worker thread indefinitely.
const KEEP_ALIVE_MAX_REQUESTS: usize = 100;

/// Default wall-clock deadline for server-side transforms.
///
/// The server injects this deadline into every transform request to prevent individual
/// requests from consuming unbounded wall-clock time. Library and CLI consumers are not subject
/// to this limit by default.
const SERVER_TRANSFORM_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct PublicCacheControl {
    max_age: u32,
    stale_while_revalidate: u32,
}

#[derive(Clone, Copy)]
struct ImageResponseConfig {
    disable_accept_negotiation: bool,
    public_cache_control: PublicCacheControl,
}

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
    pub signed_url_key_id: Option<String>,
    /// The shared secret used to verify public signed GET requests.
    pub signed_url_secret: Option<String>,
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
    /// The storage backend used to resolve `Path`-based public GET requests.
    #[cfg(feature = "s3")]
    pub storage_backend: s3::StorageBackend,
    /// Shared S3 client context, present when `storage_backend` is `S3`.
    #[cfg(feature = "s3")]
    pub s3_context: Option<Arc<s3::S3Context>>,
}

impl Clone for ServerConfig {
    fn clone(&self) -> Self {
        Self {
            storage_root: self.storage_root.clone(),
            bearer_token: self.bearer_token.clone(),
            public_base_url: self.public_base_url.clone(),
            signed_url_key_id: self.signed_url_key_id.clone(),
            signed_url_secret: self.signed_url_secret.clone(),
            allow_insecure_url_sources: self.allow_insecure_url_sources,
            cache_root: self.cache_root.clone(),
            public_max_age_seconds: self.public_max_age_seconds,
            public_stale_while_revalidate_seconds: self.public_stale_while_revalidate_seconds,
            disable_accept_negotiation: self.disable_accept_negotiation,
            log_handler: self.log_handler.clone(),
            #[cfg(feature = "s3")]
            storage_backend: self.storage_backend,
            #[cfg(feature = "s3")]
            s3_context: self.s3_context.clone(),
        }
    }
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("ServerConfig");
        d.field("storage_root", &self.storage_root)
            .field("bearer_token", &self.bearer_token)
            .field("public_base_url", &self.public_base_url)
            .field("signed_url_key_id", &self.signed_url_key_id)
            .field("signed_url_secret", &self.signed_url_secret)
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
            .field("log_handler", &self.log_handler.as_ref().map(|_| ".."));
        #[cfg(feature = "s3")]
        {
            d.field("storage_backend", &self.storage_backend)
                .field("s3_context", &self.s3_context.as_ref().map(|_| ".."));
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
            && self.allow_insecure_url_sources == other.allow_insecure_url_sources
            && self.cache_root == other.cache_root
            && self.public_max_age_seconds == other.public_max_age_seconds
            && self.public_stale_while_revalidate_seconds
                == other.public_stale_while_revalidate_seconds
            && self.disable_accept_negotiation == other.disable_accept_negotiation
            && cfg_s3_eq(self, other)
    }
}

fn cfg_s3_eq(_this: &ServerConfig, _other: &ServerConfig) -> bool {
    #[cfg(feature = "s3")]
    {
        _this.storage_backend == _other.storage_backend
            && _this
                .s3_context
                .as_ref()
                .map(|c| (&c.default_bucket, &c.endpoint_url))
                == _other
                    .s3_context
                    .as_ref()
                    .map(|c| (&c.default_bucket, &c.endpoint_url))
    }
    #[cfg(not(feature = "s3"))]
    {
        true
    }
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
            allow_insecure_url_sources: false,
            cache_root: None,
            public_max_age_seconds: DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            public_stale_while_revalidate_seconds: DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
            disable_accept_negotiation: false,
            log_handler: None,
            #[cfg(feature = "s3")]
            storage_backend: s3::StorageBackend::Filesystem,
            #[cfg(feature = "s3")]
            s3_context: None,
        }
    }

    /// Emits a diagnostic message through the configured log handler, or falls
    /// back to stderr when no handler is set.
    fn log(&self, msg: &str) {
        if let Some(handler) = &self.log_handler {
            handler(msg);
        } else {
            eprintln!("{msg}");
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
        self.signed_url_key_id = Some(key_id.into());
        self.signed_url_secret = Some(secret.into());
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
        self.storage_backend = s3::StorageBackend::S3;
        self.s3_context = Some(Arc::new(context));
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
    /// - `TRUSS_STORAGE_BACKEND` *(requires the `s3` feature)*: storage backend for resolving
    ///   `Path`-based public GET requests. Accepts `filesystem` (default) or `s3`.
    /// - `TRUSS_S3_BUCKET` *(requires the `s3` feature)*: default S3 bucket name. Required when
    ///   the storage backend is `s3`.
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
        #[cfg(feature = "s3")]
        let storage_backend = match env::var("TRUSS_STORAGE_BACKEND")
            .ok()
            .filter(|v| !v.is_empty())
        {
            Some(value) => s3::StorageBackend::parse(&value)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
            None => s3::StorageBackend::Filesystem,
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

        if signed_url_key_id.is_some() && public_base_url.is_none() {
            eprintln!(
                "truss: warning: TRUSS_SIGNED_URL_KEY_ID is set but TRUSS_PUBLIC_BASE_URL is not. \
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

        #[cfg(feature = "s3")]
        let s3_context = if storage_backend == s3::StorageBackend::S3 {
            let bucket = env::var("TRUSS_S3_BUCKET")
                .ok()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "TRUSS_S3_BUCKET is required when TRUSS_STORAGE_BACKEND=s3",
                    )
                })?;
            Some(Arc::new(s3::build_s3_context(bucket)?))
        } else {
            None
        };

        Ok(Self {
            storage_root,
            bearer_token,
            public_base_url,
            signed_url_key_id,
            signed_url_secret,
            allow_insecure_url_sources: env_flag("TRUSS_ALLOW_INSECURE_URL_SOURCES"),
            cache_root,
            public_max_age_seconds,
            public_stale_while_revalidate_seconds,
            disable_accept_negotiation: env_flag("TRUSS_DISABLE_ACCEPT_NEGOTIATION"),
            log_handler: None,
            #[cfg(feature = "s3")]
            storage_backend,
            #[cfg(feature = "s3")]
            s3_context,
        })
    }
}

/// Source selector used when generating a signed public transform URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignedUrlSource {
    /// Generates a signed `GET /images/by-path` URL.
    Path {
        /// The storage-relative source path.
        path: String,
        /// An optional source version token.
        version: Option<String>,
    },
    /// Generates a signed `GET /images/by-url` URL.
    Url {
        /// The remote source URL.
        url: String,
        /// An optional source version token.
        version: Option<String>,
    },
}

/// Builds a signed public transform URL for the server adapter.
///
/// The resulting URL targets either `GET /images/by-path` or `GET /images/by-url` depending on
/// `source`. `base_url` must be an absolute `http` or `https` URL that points at the externally
/// visible server origin. The helper applies the same canonical query and HMAC-SHA256 signature
/// scheme that the server adapter verifies at request time.
///
/// The helper serializes only explicitly requested transform options and omits fields that would
/// resolve to the documented defaults on the server side.
///
/// # Errors
///
/// Returns an error string when `base_url` is not an absolute `http` or `https` URL, when the
/// visible authority cannot be determined, or when the HMAC state cannot be initialized.
///
/// # Examples
///
/// ```
/// use truss::adapters::server::{sign_public_url, SignedUrlSource};
/// use truss::{MediaType, TransformOptions};
///
/// let url = sign_public_url(
///     "https://cdn.example.com",
///     SignedUrlSource::Path {
///         path: "/image.png".to_string(),
///         version: None,
///     },
///     &TransformOptions {
///         format: Some(MediaType::Jpeg),
///         ..TransformOptions::default()
///     },
///     "public-dev",
///     "secret-value",
///     4_102_444_800,
/// )
/// .unwrap();
///
/// assert!(url.starts_with("https://cdn.example.com/images/by-path?"));
/// assert!(url.contains("keyId=public-dev"));
/// assert!(url.contains("signature="));
/// ```
pub fn sign_public_url(
    base_url: &str,
    source: SignedUrlSource,
    options: &TransformOptions,
    key_id: &str,
    secret: &str,
    expires: u64,
) -> Result<String, String> {
    let base_url = Url::parse(base_url).map_err(|error| format!("base URL is invalid: {error}"))?;
    match base_url.scheme() {
        "http" | "https" => {}
        _ => return Err("base URL must use the http or https scheme".to_string()),
    }

    let route_path = match source {
        SignedUrlSource::Path { .. } => "/images/by-path",
        SignedUrlSource::Url { .. } => "/images/by-url",
    };
    let mut endpoint = base_url
        .join(route_path)
        .map_err(|error| format!("failed to resolve the public endpoint URL: {error}"))?;
    let authority = url_authority(&endpoint)?;
    let mut query = signed_source_query(source);
    extend_transform_query(&mut query, options);
    query.insert("keyId".to_string(), key_id.to_string());
    query.insert("expires".to_string(), expires.to_string());

    let canonical = format!(
        "GET\n{}\n{}\n{}",
        authority,
        endpoint.path(),
        canonical_query_without_signature(&query)
    );
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| format!("failed to initialize signed URL HMAC: {error}"))?;
    mac.update(canonical.as_bytes());
    query.insert(
        "signature".to_string(),
        hex::encode(mac.finalize().into_bytes()),
    );

    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in query {
        serializer.append_pair(&name, &value);
    }
    endpoint.set_query(Some(&serializer.finish()));
    Ok(endpoint.into())
}

/// Returns the bind address for the HTTP server adapter.
///
/// The adapter reads `TRUSS_BIND_ADDR` when it is present. Otherwise it falls back to
/// [`DEFAULT_BIND_ADDR`].
pub fn bind_addr() -> String {
    env::var("TRUSS_BIND_ADDR").unwrap_or_else(|_| DEFAULT_BIND_ADDR.to_string())
}

/// Serves requests until the listener stops producing connections.
///
/// This helper loads [`ServerConfig`] from the process environment and then delegates to
/// [`serve_with_config`]. Health endpoints remain available even when the private API is not
/// configured, but authenticated transform requests will return `503 Service Unavailable`
/// unless `TRUSS_BEARER_TOKEN` is set.
///
/// # Errors
///
/// Returns an [`io::Error`] when the storage root cannot be resolved, when accepting the next
/// connection fails, or when a response cannot be written to the socket.
pub fn serve(listener: TcpListener) -> io::Result<()> {
    let config = ServerConfig::from_env()?;
    serve_with_config(listener, &config)
}

/// Serves requests with an explicit server configuration.
///
/// This is the adapter entry point for tests and embedding scenarios that want deterministic
/// configuration instead of environment-variable lookup.
///
/// # Errors
///
/// Returns an [`io::Error`] when accepting the next connection fails or when a response cannot
/// be written to the socket.
pub fn serve_with_config(listener: TcpListener, config: &ServerConfig) -> io::Result<()> {
    let config = Arc::new(config.clone());
    let (sender, receiver) = std::sync::mpsc::channel::<TcpStream>();

    // Spawn a fixed-size pool of worker threads. Each thread pulls connections
    // from the shared channel and handles them independently, so a slow request
    // no longer blocks all other clients.
    let receiver = Arc::new(std::sync::Mutex::new(receiver));
    let mut workers = Vec::with_capacity(WORKER_THREADS);
    for _ in 0..WORKER_THREADS {
        let rx = Arc::clone(&receiver);
        let cfg = Arc::clone(&config);
        workers.push(std::thread::spawn(move || {
            while let Ok(stream) = rx.lock().expect("worker lock poisoned").recv() {
                if let Err(err) = handle_stream(stream, &cfg) {
                    cfg.log(&format!("failed to handle connection: {err}"));
                }
            }
        }));
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if sender.send(stream).is_err() {
                    break;
                }
            }
            Err(err) => return Err(err),
        }
    }

    drop(sender);
    for worker in workers {
        let _ = worker.join();
    }

    Ok(())
}

/// Serves exactly one request using configuration loaded from the environment.
///
/// This helper is primarily useful in tests that want to drive the server over a real TCP
/// socket but do not need a long-running loop.
///
/// # Errors
///
/// Returns an [`io::Error`] when the storage root cannot be resolved, when accepting the next
/// connection fails, or when a response cannot be written to the socket.
pub fn serve_once(listener: TcpListener) -> io::Result<()> {
    let config = ServerConfig::from_env()?;
    serve_once_with_config(listener, &config)
}

/// Serves exactly one request with an explicit server configuration.
///
/// # Errors
///
/// Returns an [`io::Error`] when accepting the next connection fails or when a response cannot
/// be written to the socket.
pub fn serve_once_with_config(listener: TcpListener, config: &ServerConfig) -> io::Result<()> {
    let (stream, _) = listener.accept()?;
    handle_stream(stream, config)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TransformImageRequestPayload {
    source: TransformSourcePayload,
    #[serde(default)]
    options: TransformOptionsPayload,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum TransformSourcePayload {
    Path {
        path: String,
        version: Option<String>,
    },
    Url {
        url: String,
        version: Option<String>,
    },
    #[cfg(feature = "s3")]
    Storage {
        bucket: Option<String>,
        key: String,
        version: Option<String>,
    },
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
    fn versioned_source_hash(&self, config: &ServerConfig) -> Option<String> {
        let (kind, reference, version): (&str, std::borrow::Cow<'_, str>, Option<&str>) = match self
        {
            Self::Path { path, version } => ("path", path.as_str().into(), version.as_deref()),
            Self::Url { url, version } => ("url", url.as_str().into(), version.as_deref()),
            #[cfg(feature = "s3")]
            Self::Storage {
                bucket,
                key,
                version,
            } => {
                let effective_bucket = bucket.as_deref().or(config
                    .s3_context
                    .as_ref()
                    .map(|ctx| ctx.default_bucket.as_str()))?;
                (
                    "storage",
                    format!("s3://{effective_bucket}/{key}").into(),
                    version.as_deref(),
                )
            }
        };
        let version = version?;
        // Use newline separators so that values containing colons cannot collide
        // with different (reference, version) pairs. Include configuration boundaries
        // to prevent cross-instance cache poisoning.
        let mut id = String::new();
        id.push_str(kind);
        id.push('\n');
        id.push_str(&reference);
        id.push('\n');
        id.push_str(version);
        id.push('\n');
        id.push_str(config.storage_root.to_string_lossy().as_ref());
        id.push('\n');
        id.push_str(if config.allow_insecure_url_sources {
            "insecure"
        } else {
            "strict"
        });
        #[cfg(feature = "s3")]
        {
            id.push('\n');
            id.push_str(match config.storage_backend {
                s3::StorageBackend::S3 => "s3-backend",
                s3::StorageBackend::Filesystem => "fs-backend",
            });
            if let Some(ref ctx) = config.s3_context
                && let Some(ref endpoint) = ctx.endpoint_url
            {
                id.push('\n');
                id.push_str(endpoint);
            }
        }
        Some(hex::encode(Sha256::digest(id.as_bytes())))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct TransformOptionsPayload {
    width: Option<u32>,
    height: Option<u32>,
    fit: Option<String>,
    position: Option<String>,
    format: Option<String>,
    quality: Option<u8>,
    background: Option<String>,
    rotate: Option<u16>,
    auto_orient: Option<bool>,
    strip_metadata: Option<bool>,
    preserve_exif: Option<bool>,
    blur: Option<f32>,
}

impl TransformOptionsPayload {
    fn into_options(self) -> Result<TransformOptions, HttpResponse> {
        let defaults = TransformOptions::default();

        Ok(TransformOptions {
            width: self.width,
            height: self.height,
            fit: parse_optional_named(self.fit.as_deref(), "fit", Fit::from_str)?,
            position: parse_optional_named(
                self.position.as_deref(),
                "position",
                Position::from_str,
            )?,
            format: parse_optional_named(self.format.as_deref(), "format", MediaType::from_str)?,
            quality: self.quality,
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
            strip_metadata: self.strip_metadata.unwrap_or(defaults.strip_metadata),
            preserve_exif: self.preserve_exif.unwrap_or(defaults.preserve_exif),
            blur: self.blur,
            deadline: defaults.deadline,
        })
    }
}

fn handle_stream(mut stream: TcpStream, config: &ServerConfig) -> io::Result<()> {
    // Prevent slow or stalled clients from blocking the accept loop indefinitely.
    if let Err(err) = stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT)) {
        config.log(&format!("failed to set socket read timeout: {err}"));
    }
    if let Err(err) = stream.set_write_timeout(Some(SOCKET_WRITE_TIMEOUT)) {
        config.log(&format!("failed to set socket write timeout: {err}"));
    }

    let mut requests_served: usize = 0;

    loop {
        let partial = match read_request_headers(&mut stream) {
            Ok(partial) => partial,
            Err(response) => {
                if requests_served > 0 {
                    return Ok(());
                }
                let _ = write_response(&mut stream, response, true);
                return Ok(());
            }
        };

        let client_wants_close = partial
            .headers
            .iter()
            .any(|(name, value)| name == "connection" && value.eq_ignore_ascii_case("close"));

        let is_head = partial.method == "HEAD";

        let requires_auth = matches!(
            (partial.method.as_str(), partial.path()),
            ("POST", "/images:transform") | ("POST", "/images")
        );
        if requires_auth && let Err(response) = authorize_request_headers(&partial.headers, config)
        {
            let _ = write_response(&mut stream, response, true);
            return Ok(());
        }

        let request = match read_request_body(&mut stream, partial) {
            Ok(request) => request,
            Err(response) => {
                let _ = write_response(&mut stream, response, true);
                return Ok(());
            }
        };
        let route = classify_route(&request);
        let mut response = route_request(request, config);
        record_http_metrics(route, response.status);

        if is_head {
            response.body = Vec::new();
        }

        requests_served += 1;
        let close_after = client_wants_close || requests_served >= KEEP_ALIVE_MAX_REQUESTS;

        write_response(&mut stream, response, close_after)?;

        if close_after {
            return Ok(());
        }
    }
}

fn route_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    let method = request.method.clone();
    let path = request.path().to_string();

    match (method.as_str(), path.as_str()) {
        ("GET" | "HEAD", "/health") => handle_health(config),
        ("GET" | "HEAD", "/health/live") => handle_health_live(),
        ("GET" | "HEAD", "/health/ready") => handle_health_ready(config),
        ("GET" | "HEAD", "/images/by-path") => handle_public_path_request(request, config),
        ("GET" | "HEAD", "/images/by-url") => handle_public_url_request(request, config),
        ("POST", "/images:transform") => handle_transform_request(request, config),
        ("POST", "/images") => handle_upload_request(request, config),
        ("GET" | "HEAD", "/metrics") => handle_metrics_request(request, config),
        _ => HttpResponse::problem("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec()),
    }
}

fn classify_route(request: &HttpRequest) -> RouteMetric {
    match (request.method.as_str(), request.path()) {
        ("GET" | "HEAD", "/health") => RouteMetric::Health,
        ("GET" | "HEAD", "/health/live") => RouteMetric::HealthLive,
        ("GET" | "HEAD", "/health/ready") => RouteMetric::HealthReady,
        ("GET" | "HEAD", "/images/by-path") => RouteMetric::PublicByPath,
        ("GET" | "HEAD", "/images/by-url") => RouteMetric::PublicByUrl,
        ("POST", "/images:transform") => RouteMetric::Transform,
        ("POST", "/images") => RouteMetric::Upload,
        ("GET" | "HEAD", "/metrics") => RouteMetric::Metrics,
        _ => RouteMetric::Unknown,
    }
}

fn handle_transform_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
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
    if let Some(response) = try_versioned_cache_lookup(
        versioned_hash.as_deref(),
        &options,
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
    ) {
        return response;
    }

    let source_bytes = match resolve_source_bytes(payload.source, config) {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    transform_source_bytes(
        source_bytes,
        options,
        versioned_hash.as_deref(),
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
    )
}

fn handle_public_path_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    handle_public_get_request(request, config, PublicSourceKind::Path)
}

fn handle_public_url_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    handle_public_get_request(request, config, PublicSourceKind::Url)
}

fn handle_public_get_request(
    request: HttpRequest,
    config: &ServerConfig,
    source_kind: PublicSourceKind,
) -> HttpResponse {
    let query = match parse_query_params(&request) {
        Ok(query) => query,
        Err(response) => return response,
    };
    if let Err(response) = authorize_signed_request(&request, &query, config) {
        return response;
    }
    let (source, options) = match parse_public_get_request(&query, source_kind) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };

    // When the storage backend is S3, convert Path sources to Storage sources so
    // that the `path` query parameter is resolved as an S3 key.
    #[cfg(feature = "s3")]
    let source = if config.storage_backend == s3::StorageBackend::S3 {
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
    ) {
        return response;
    }

    let source_bytes = match resolve_source_bytes(source, config) {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };

    transform_source_bytes(
        source_bytes,
        options,
        versioned_hash.as_deref(),
        &request,
        ImageResponsePolicy::PublicGet,
        config,
    )
}

fn handle_upload_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    let boundary = match parse_multipart_boundary(&request) {
        Ok(boundary) => boundary,
        Err(response) => return response,
    };
    let (file_bytes, options) = match parse_upload_request(&request.body, &boundary) {
        Ok(parts) => parts,
        Err(response) => return response,
    };
    transform_source_bytes(
        file_bytes,
        options,
        None,
        &request,
        ImageResponsePolicy::PrivateTransform,
        config,
    )
}

/// Returns a minimal liveness response confirming the process is running.
fn handle_health_live() -> HttpResponse {
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

/// Returns a readiness response after checking that critical dependencies are
/// available (storage root, cache root if configured, and transform capacity).
fn handle_health_ready(config: &ServerConfig) -> HttpResponse {
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

    let in_flight = TRANSFORMS_IN_FLIGHT.load(Ordering::Relaxed);
    let overloaded = in_flight >= MAX_CONCURRENT_TRANSFORMS;
    checks.push(json!({
        "name": "transformCapacity",
        "status": if overloaded { "fail" } else { "ok" },
    }));
    if overloaded {
        all_ok = false;
    }

    let status_str = if all_ok { "ok" } else { "fail" };
    let mut body = serde_json::to_vec(&json!({
        "status": status_str,
        "checks": checks,
    }))
    .expect("serialize readiness");
    body.push(b'\n');

    if all_ok {
        HttpResponse::json("200 OK", body)
    } else {
        HttpResponse::json("503 Service Unavailable", body)
    }
}

/// Returns a comprehensive diagnostic health response.
fn storage_health_check(config: &ServerConfig) -> Vec<(bool, &'static str)> {
    #[allow(unused_mut)]
    let mut checks = vec![(config.storage_root.is_dir(), "storageRoot")];
    #[cfg(feature = "s3")]
    if config.storage_backend == s3::StorageBackend::S3 {
        checks.push((config.s3_context.is_some(), "s3Client"));
    }
    checks
}

fn handle_health(config: &ServerConfig) -> HttpResponse {
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

    let in_flight = TRANSFORMS_IN_FLIGHT.load(Ordering::Relaxed);
    let overloaded = in_flight >= MAX_CONCURRENT_TRANSFORMS;
    checks.push(json!({
        "name": "transformCapacity",
        "status": if overloaded { "fail" } else { "ok" },
    }));
    if overloaded {
        all_ok = false;
    }

    let status_str = if all_ok { "ok" } else { "fail" };
    let mut body = serde_json::to_vec(&json!({
        "status": status_str,
        "service": "truss",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSeconds": uptime_seconds(),
        "checks": checks,
    }))
    .expect("serialize health");
    body.push(b'\n');

    HttpResponse::json("200 OK", body)
}

fn handle_metrics_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    if let Err(response) = authorize_request(&request, config) {
        return response;
    }

    HttpResponse::text(
        "200 OK",
        "text/plain; version=0.0.4; charset=utf-8",
        render_metrics_text().into_bytes(),
    )
}

fn parse_public_get_request(
    query: &BTreeMap<String, String>,
    source_kind: PublicSourceKind,
) -> Result<(TransformSourcePayload, TransformOptions), HttpResponse> {
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

    let defaults = TransformOptions::default();
    let options = TransformOptions {
        width: parse_optional_integer_query(query, "width")?,
        height: parse_optional_integer_query(query, "height")?,
        fit: parse_optional_named(query.get("fit").map(String::as_str), "fit", Fit::from_str)?,
        position: parse_optional_named(
            query.get("position").map(String::as_str),
            "position",
            Position::from_str,
        )?,
        format: parse_optional_named(
            query.get("format").map(String::as_str),
            "format",
            MediaType::from_str,
        )?,
        quality: parse_optional_u8_query(query, "quality")?,
        background: parse_optional_named(
            query.get("background").map(String::as_str),
            "background",
            Rgba8::from_hex,
        )?,
        rotate: match query.get("rotate") {
            Some(value) => parse_named(value, "rotate", Rotation::from_str)?,
            None => defaults.rotate,
        },
        auto_orient: parse_optional_bool_query(query, "autoOrient")?
            .unwrap_or(defaults.auto_orient),
        strip_metadata: parse_optional_bool_query(query, "stripMetadata")?
            .unwrap_or(defaults.strip_metadata),
        preserve_exif: parse_optional_bool_query(query, "preserveExif")?
            .unwrap_or(defaults.preserve_exif),
        blur: parse_optional_float_query(query, "blur")?,
        deadline: defaults.deadline,
    };

    Ok((source, options))
}

fn transform_source_bytes(
    source_bytes: Vec<u8>,
    options: TransformOptions,
    versioned_hash: Option<&str>,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    config: &ServerConfig,
) -> HttpResponse {
    let content_hash;
    let source_hash = match versioned_hash {
        Some(hash) => hash,
        None => {
            content_hash = hex::encode(Sha256::digest(&source_bytes));
            &content_hash
        }
    };

    let cache = config
        .cache_root
        .as_ref()
        .map(|root| TransformCache::new(root.clone()).with_log_handler(config.log_handler.clone()));

    if let Some(ref cache) = cache
        && options.format.is_some()
    {
        let cache_key = compute_cache_key(source_hash, &options, None);
        if let CacheLookup::Hit {
            media_type,
            body,
            age,
        } = cache.get(&cache_key)
        {
            CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
            let etag = build_image_etag(&body);
            let mut headers = build_image_response_headers(
                media_type,
                &etag,
                response_policy,
                false,
                CacheHitStatus::Hit,
                config.public_max_age_seconds,
                config.public_stale_while_revalidate_seconds,
            );
            headers.push(("Age".to_string(), age.as_secs().to_string()));
            if matches!(response_policy, ImageResponsePolicy::PublicGet)
                && if_none_match_matches(request.header("if-none-match"), &etag)
            {
                return HttpResponse::empty("304 Not Modified", headers);
            }
            return HttpResponse::binary_with_headers(
                "200 OK",
                media_type.as_mime(),
                headers,
                body,
            );
        }
    }

    let in_flight = TRANSFORMS_IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    if in_flight >= MAX_CONCURRENT_TRANSFORMS {
        TRANSFORMS_IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
        return service_unavailable_response("too many concurrent transforms; retry later");
    }
    let response = transform_source_bytes_inner(
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
        },
    );
    TRANSFORMS_IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    response
}

fn transform_source_bytes_inner(
    source_bytes: Vec<u8>,
    mut options: TransformOptions,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    cache: Option<&TransformCache>,
    source_hash: &str,
    response_config: ImageResponseConfig,
) -> HttpResponse {
    if options.deadline.is_none() {
        options.deadline = Some(SERVER_TRANSFORM_DEADLINE);
    }
    let artifact = match sniff_artifact(RawArtifact::new(source_bytes, None)) {
        Ok(artifact) => artifact,
        Err(error) => return transform_error_response(error),
    };
    let negotiation_used =
        if options.format.is_none() && !response_config.disable_accept_negotiation {
            match negotiate_output_format(request.header("accept"), &artifact) {
                Ok(Some(format)) => {
                    options.format = Some(format);
                    true
                }
                Ok(None) => false,
                Err(response) => return response,
            }
        } else {
            false
        };

    let negotiated_accept = if negotiation_used {
        request.header("accept")
    } else {
        None
    };
    let cache_key = compute_cache_key(source_hash, &options, negotiated_accept);

    if let Some(cache) = cache
        && let CacheLookup::Hit {
            media_type,
            body,
            age,
        } = cache.get(&cache_key)
    {
        CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
        let etag = build_image_etag(&body);
        let mut headers = build_image_response_headers(
            media_type,
            &etag,
            response_policy,
            negotiation_used,
            CacheHitStatus::Hit,
            response_config.public_cache_control.max_age,
            response_config.public_cache_control.stale_while_revalidate,
        );
        headers.push(("Age".to_string(), age.as_secs().to_string()));
        if matches!(response_policy, ImageResponsePolicy::PublicGet)
            && if_none_match_matches(request.header("if-none-match"), &etag)
        {
            return HttpResponse::empty("304 Not Modified", headers);
        }
        return HttpResponse::binary_with_headers("200 OK", media_type.as_mime(), headers, body);
    }

    if cache.is_some() {
        CACHE_MISSES_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    let is_svg = artifact.media_type == MediaType::Svg;
    let result = if is_svg {
        match transform_svg(TransformRequest::new(artifact, options)) {
            Ok(result) => result,
            Err(error) => return transform_error_response(error),
        }
    } else {
        match transform_raster(TransformRequest::new(artifact, options)) {
            Ok(result) => result,
            Err(error) => return transform_error_response(error),
        }
    };

    for warning in &result.warnings {
        let msg = format!("truss: {warning}");
        if let Some(c) = cache
            && let Some(handler) = &c.log_handler
        {
            handler(&msg);
        } else {
            eprintln!("{msg}");
        }
    }

    let output = result.artifact;

    if let Some(cache) = cache {
        cache.put(&cache_key, output.media_type, &output.bytes);
    }

    let cache_hit_status = if cache.is_some() {
        CacheHitStatus::Miss
    } else {
        CacheHitStatus::Disabled
    };

    let etag = build_image_etag(&output.bytes);
    let headers = build_image_response_headers(
        output.media_type,
        &etag,
        response_policy,
        negotiation_used,
        cache_hit_status,
        response_config.public_cache_control.max_age,
        response_config.public_cache_control.stale_while_revalidate,
    );

    if matches!(response_policy, ImageResponsePolicy::PublicGet)
        && if_none_match_matches(request.header("if-none-match"), &etag)
    {
        return HttpResponse::empty("304 Not Modified", headers);
    }

    HttpResponse::binary_with_headers("200 OK", output.media_type.as_mime(), headers, output.bytes)
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

fn parse_optional_env_u32(name: &str) -> io::Result<Option<u32>> {
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

fn validate_public_base_url(value: String) -> io::Result<String> {
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
    use super::http_parse::{
        HttpRequest, find_header_terminator, read_request_body, read_request_headers,
        resolve_storage_path,
    };
    use super::multipart::parse_multipart_form_data;
    use super::remote::{PinnedResolver, prepare_remote_fetch_target};
    use super::response::auth_required_response;
    use super::response::{HttpResponse, bad_request_response};
    use super::{
        CacheHitStatus, DEFAULT_BIND_ADDR, DEFAULT_PUBLIC_MAX_AGE_SECONDS,
        DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS, ImageResponsePolicy,
        MAX_CONCURRENT_TRANSFORMS, PublicSourceKind, ServerConfig, SignedUrlSource,
        TRANSFORMS_IN_FLIGHT, TransformSourcePayload, authorize_signed_request, bind_addr,
        build_image_etag, build_image_response_headers, canonical_query_without_signature,
        negotiate_output_format, parse_public_get_request, route_request, serve_once_with_config,
        sign_public_url, transform_source_bytes,
    };
    use crate::{
        Artifact, ArtifactMetadata, MediaType, RawArtifact, TransformOptions, sniff_artifact,
    };
    use hmac::{Hmac, Mac};
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    use sha2::Sha256;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Test-only convenience wrapper that reads headers + body in one shot,
    /// preserving the original `read_request` semantics for existing tests.
    fn read_request<R: Read>(stream: &mut R) -> Result<HttpRequest, HttpResponse> {
        let partial = read_request_headers(stream)?;
        read_request_body(stream, partial)
    }

    fn png_bytes() -> Vec<u8> {
        let image = RgbaImage::from_pixel(4, 3, Rgba([10, 20, 30, 255]));
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&image, 4, 3, ColorType::Rgba8.into())
            .expect("encode png");
        bytes
    }

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("truss-server-{name}-{unique}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_png(path: &Path) {
        fs::write(path, png_bytes()).expect("write png fixture");
    }

    fn artifact_with_alpha(has_alpha: bool) -> Artifact {
        Artifact::new(
            png_bytes(),
            MediaType::Png,
            ArtifactMetadata {
                width: Some(4),
                height: Some(3),
                frame_count: 1,
                duration: None,
                has_alpha: Some(has_alpha),
            },
        )
    }

    fn sign_public_query(
        method: &str,
        authority: &str,
        path: &str,
        query: &BTreeMap<String, String>,
        secret: &str,
    ) -> String {
        let canonical = format!(
            "{method}\n{authority}\n{path}\n{}",
            canonical_query_without_signature(query)
        );
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("create hmac");
        mac.update(canonical.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    type FixtureResponse = (String, Vec<(String, String)>, Vec<u8>);

    fn read_fixture_request(stream: &mut TcpStream) {
        stream
            .set_nonblocking(false)
            .expect("configure fixture stream blocking mode");
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .expect("configure fixture stream timeout");

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 1024];
        let header_end = loop {
            let read = match stream.read(&mut chunk) {
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("read fixture request headers: {error}"),
            };
            if read == 0 {
                panic!("fixture request ended before headers were complete");
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(index) = find_header_terminator(&buffer) {
                break index;
            }
        };

        let header_text = std::str::from_utf8(&buffer[..header_end]).expect("fixture request utf8");
        let content_length = header_text
            .split("\r\n")
            .filter_map(|line| line.split_once(':'))
            .find_map(|(name, value)| {
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .map(|value| {
                value
                    .parse::<usize>()
                    .expect("fixture content-length should be numeric")
            })
            .unwrap_or(0);

        let mut body = buffer.len().saturating_sub(header_end + 4);
        while body < content_length {
            let read = match stream.read(&mut chunk) {
                Ok(read) => read,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) && std::time::Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("read fixture request body: {error}"),
            };
            if read == 0 {
                panic!("fixture request body was truncated");
            }
            body += read;
        }
    }

    fn spawn_http_server(responses: Vec<FixtureResponse>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture server");
        listener
            .set_nonblocking(true)
            .expect("configure fixture server");
        let addr = listener.local_addr().expect("fixture server addr");
        let url = format!("http://{addr}/image");

        let handle = thread::spawn(move || {
            for (status, headers, body) in responses {
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                let mut accepted = None;
                while std::time::Instant::now() < deadline {
                    match listener.accept() {
                        Ok(stream) => {
                            accepted = Some(stream);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept fixture request: {error}"),
                    }
                }

                let Some((mut stream, _)) = accepted else {
                    break;
                };
                read_fixture_request(&mut stream);
                let mut header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    header.push_str(&format!("{name}: {value}\r\n"));
                }
                header.push_str("\r\n");
                stream
                    .write_all(header.as_bytes())
                    .expect("write fixture headers");
                stream.write_all(&body).expect("write fixture body");
                stream.flush().expect("flush fixture response");
            }
        });

        (url, handle)
    }

    fn transform_request(path: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            target: "/images:transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: format!(
                "{{\"source\":{{\"kind\":\"path\",\"path\":\"{path}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
            )
            .into_bytes(),
        }
    }

    fn transform_url_request(url: &str) -> HttpRequest {
        HttpRequest {
            method: "POST".to_string(),
            target: "/images:transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: format!(
                "{{\"source\":{{\"kind\":\"url\",\"url\":\"{url}\"}},\"options\":{{\"format\":\"jpeg\"}}}}"
            )
            .into_bytes(),
        }
    }

    fn upload_request(file_bytes: &[u8], options_json: Option<&str>) -> HttpRequest {
        let boundary = "truss-test-boundary";
        let mut body = Vec::new();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(file_bytes);
        body.extend_from_slice(b"\r\n");

        if let Some(options_json) = options_json {
            body.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{options_json}\r\n"
                )
                .as_bytes(),
            );
        }

        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                (
                    "content-type".to_string(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ],
            body,
        }
    }

    fn metrics_request(with_auth: bool) -> HttpRequest {
        let mut headers = Vec::new();
        if with_auth {
            headers.push(("authorization".to_string(), "Bearer secret".to_string()));
        }

        HttpRequest {
            method: "GET".to_string(),
            target: "/metrics".to_string(),
            version: "HTTP/1.1".to_string(),
            headers,
            body: Vec::new(),
        }
    }

    fn response_body(response: &HttpResponse) -> String {
        String::from_utf8(response.body.clone()).expect("utf8 response body")
    }

    fn signed_public_request(target: &str, host: &str, secret: &str) -> HttpRequest {
        let (path, query) = target.split_once('?').expect("target has query");
        let mut query = url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect::<BTreeMap<_, _>>();
        let signature = sign_public_query("GET", host, path, &query, secret);
        query.insert("signature".to_string(), signature);
        let final_query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(
                query
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            )
            .finish();

        HttpRequest {
            method: "GET".to_string(),
            target: format!("{path}?{final_query}"),
            version: "HTTP/1.1".to_string(),
            headers: vec![("host".to_string(), host.to_string())],
            body: Vec::new(),
        }
    }

    #[test]
    fn uses_default_bind_addr_when_env_is_missing() {
        unsafe { std::env::remove_var("TRUSS_BIND_ADDR") };
        assert_eq!(bind_addr(), DEFAULT_BIND_ADDR);
    }

    #[test]
    fn authorize_signed_request_accepts_a_valid_signature() {
        let request = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=public-dev&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-value",
        );
        let query = super::auth::parse_query_params(&request).expect("parse query");
        let config = ServerConfig::new(temp_dir("public-auth"), None)
            .with_signed_url_credentials("public-dev", "secret-value");

        authorize_signed_request(&request, &query, &config).expect("signed auth should pass");
    }

    #[test]
    fn authorize_signed_request_uses_public_base_url_authority() {
        let request = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=public-dev&expires=4102444800&format=jpeg",
            "cdn.example.com",
            "secret-value",
        );
        let query = super::auth::parse_query_params(&request).expect("parse query");
        let mut config = ServerConfig::new(temp_dir("public-authority"), None)
            .with_signed_url_credentials("public-dev", "secret-value");
        config.public_base_url = Some("https://cdn.example.com".to_string());

        authorize_signed_request(&request, &query, &config).expect("signed auth should pass");
    }

    #[test]
    fn negotiate_output_format_prefers_alpha_safe_formats_for_transparent_inputs() {
        let format =
            negotiate_output_format(Some("image/jpeg,image/png"), &artifact_with_alpha(true))
                .expect("negotiate output format")
                .expect("resolved output format");

        assert_eq!(format, MediaType::Png);
    }

    #[test]
    fn negotiate_output_format_prefers_avif_for_wildcard_accept() {
        let format = negotiate_output_format(Some("image/*"), &artifact_with_alpha(false))
            .expect("negotiate output format")
            .expect("resolved output format");

        assert_eq!(format, MediaType::Avif);
    }

    #[test]
    fn build_image_response_headers_include_cache_and_safety_metadata() {
        let headers = build_image_response_headers(
            MediaType::Webp,
            &build_image_etag(b"demo"),
            ImageResponsePolicy::PublicGet,
            true,
            CacheHitStatus::Disabled,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
        );

        assert!(headers.contains(&(
            "Cache-Control".to_string(),
            "public, max-age=3600, stale-while-revalidate=60".to_string()
        )));
        assert!(headers.contains(&("Vary".to_string(), "Accept".to_string())));
        assert!(headers.contains(&("X-Content-Type-Options".to_string(), "nosniff".to_string())));
        assert!(headers.contains(&(
            "Content-Disposition".to_string(),
            "inline; filename=\"truss.webp\"".to_string()
        )));
        assert!(headers.contains(&(
            "Cache-Status".to_string(),
            "\"truss\"; fwd=miss".to_string()
        )));
    }

    #[test]
    fn build_image_response_headers_include_csp_sandbox_for_svg() {
        let headers = build_image_response_headers(
            MediaType::Svg,
            &build_image_etag(b"svg-data"),
            ImageResponsePolicy::PublicGet,
            true,
            CacheHitStatus::Disabled,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
        );

        assert!(headers.contains(&("Content-Security-Policy".to_string(), "sandbox".to_string())));
    }

    #[test]
    fn build_image_response_headers_omit_csp_sandbox_for_raster() {
        let headers = build_image_response_headers(
            MediaType::Png,
            &build_image_etag(b"png-data"),
            ImageResponsePolicy::PublicGet,
            true,
            CacheHitStatus::Disabled,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
        );

        assert!(!headers.iter().any(|(k, _)| k == "Content-Security-Policy"));
    }

    /// RAII guard that restores `TRANSFORMS_IN_FLIGHT` to its previous value
    /// on drop, even if the test panics.
    struct InFlightGuard {
        previous: u64,
    }

    impl InFlightGuard {
        fn set(value: u64) -> Self {
            let previous = TRANSFORMS_IN_FLIGHT.load(Ordering::Relaxed);
            TRANSFORMS_IN_FLIGHT.store(value, Ordering::Relaxed);
            Self { previous }
        }
    }

    impl Drop for InFlightGuard {
        fn drop(&mut self) {
            TRANSFORMS_IN_FLIGHT.store(self.previous, Ordering::Relaxed);
        }
    }

    #[test]
    fn backpressure_rejects_when_at_capacity() {
        let _guard = InFlightGuard::set(MAX_CONCURRENT_TRANSFORMS);

        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/transform".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let png_bytes = {
            let mut buf = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(&mut buf);
            encoder
                .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
                .unwrap();
            buf
        };

        let config = ServerConfig::new(std::env::temp_dir(), None);
        let response = transform_source_bytes(
            png_bytes,
            TransformOptions::default(),
            None,
            &request,
            ImageResponsePolicy::PrivateTransform,
            &config,
        );

        assert!(response.status.contains("503"));

        assert_eq!(
            TRANSFORMS_IN_FLIGHT.load(Ordering::Relaxed),
            MAX_CONCURRENT_TRANSFORMS
        );
    }

    #[test]
    fn compute_cache_key_is_deterministic() {
        let opts = TransformOptions {
            width: Some(300),
            height: Some(200),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let key1 = super::cache::compute_cache_key("source-abc", &opts, None);
        let key2 = super::cache::compute_cache_key("source-abc", &opts, None);
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 64);
    }

    #[test]
    fn compute_cache_key_differs_for_different_options() {
        let opts1 = TransformOptions {
            width: Some(300),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let opts2 = TransformOptions {
            width: Some(400),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let key1 = super::cache::compute_cache_key("same-source", &opts1, None);
        let key2 = super::cache::compute_cache_key("same-source", &opts2, None);
        assert_ne!(key1, key2);
    }

    #[test]
    fn compute_cache_key_includes_accept_when_present() {
        let opts = TransformOptions::default();
        let key_no_accept = super::cache::compute_cache_key("src", &opts, None);
        let key_with_accept = super::cache::compute_cache_key("src", &opts, Some("image/webp"));
        assert_ne!(key_no_accept, key_with_accept);
    }

    #[test]
    fn transform_cache_put_and_get_round_trips() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        cache.put(
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            MediaType::Png,
            b"png-data",
        );
        let result = cache.get("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890");

        match result {
            super::cache::CacheLookup::Hit {
                media_type, body, ..
            } => {
                assert_eq!(media_type, MediaType::Png);
                assert_eq!(body, b"png-data");
            }
            super::cache::CacheLookup::Miss => panic!("expected cache hit"),
        }
    }

    #[test]
    fn transform_cache_miss_for_unknown_key() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        let result = cache.get("0000001234567890abcdef1234567890abcdef1234567890abcdef1234567890");
        assert!(matches!(result, super::cache::CacheLookup::Miss));
    }

    #[test]
    fn transform_cache_uses_sharded_layout() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        cache.put(key, MediaType::Jpeg, b"jpeg-data");

        let expected = dir.path().join("ab").join("cd").join("ef").join(key);
        assert!(
            expected.exists(),
            "sharded file should exist at {expected:?}"
        );
    }

    #[test]
    fn transform_cache_expired_entry_is_miss() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let mut cache = super::cache::TransformCache::new(dir.path().to_path_buf());
        cache.ttl = Duration::from_secs(0);

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        cache.put(key, MediaType::Png, b"data");

        std::thread::sleep(Duration::from_millis(10));

        let result = cache.get(key);
        assert!(matches!(result, super::cache::CacheLookup::Miss));
    }

    #[test]
    fn transform_cache_handles_corrupted_entry_as_miss() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::TransformCache::new(dir.path().to_path_buf());

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let path = cache.entry_path(key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"corrupted-data-without-header").unwrap();

        let result = cache.get(key);
        assert!(matches!(result, super::cache::CacheLookup::Miss));
    }

    #[test]
    fn cache_status_header_reflects_hit() {
        let headers = build_image_response_headers(
            MediaType::Png,
            &build_image_etag(b"data"),
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Hit,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
        );
        assert!(headers.contains(&("Cache-Status".to_string(), "\"truss\"; hit".to_string())));
    }

    #[test]
    fn cache_status_header_reflects_miss() {
        let headers = build_image_response_headers(
            MediaType::Png,
            &build_image_etag(b"data"),
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Miss,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
        );
        assert!(headers.contains(&(
            "Cache-Status".to_string(),
            "\"truss\"; fwd=miss".to_string()
        )));
    }

    #[test]
    fn origin_cache_put_and_get_round_trips() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        cache.put("https://example.com/image.png", b"raw-source-bytes");
        let result = cache.get("https://example.com/image.png");

        assert_eq!(result.as_deref(), Some(b"raw-source-bytes".as_ref()));
    }

    #[test]
    fn origin_cache_miss_for_unknown_url() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        assert!(
            cache
                .get("https://unknown.example.com/missing.png")
                .is_none()
        );
    }

    #[test]
    fn origin_cache_expired_entry_is_none() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let mut cache = super::cache::OriginCache::new(dir.path());
        cache.ttl = Duration::from_secs(0);

        cache.put("https://example.com/img.png", b"data");
        std::thread::sleep(Duration::from_millis(10));

        assert!(cache.get("https://example.com/img.png").is_none());
    }

    #[test]
    fn origin_cache_uses_origin_subdirectory() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        cache.put("https://example.com/test.png", b"bytes");

        let origin_dir = dir.path().join("origin");
        assert!(origin_dir.exists(), "origin subdirectory should exist");
    }

    #[test]
    fn sign_public_url_builds_a_signed_path_url() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/image.png".to_string(),
                version: Some("v1".to_string()),
            },
            &crate::TransformOptions {
                format: Some(MediaType::Jpeg),
                width: Some(320),
                ..crate::TransformOptions::default()
            },
            "public-dev",
            "secret-value",
            4_102_444_800,
        )
        .expect("sign public URL");

        assert!(url.starts_with("https://cdn.example.com/images/by-path?"));
        assert!(url.contains("path=%2Fimage.png"));
        assert!(url.contains("version=v1"));
        assert!(url.contains("width=320"));
        assert!(url.contains("format=jpeg"));
        assert!(url.contains("keyId=public-dev"));
        assert!(url.contains("expires=4102444800"));
        assert!(url.contains("signature="));
    }

    #[test]
    fn parse_public_get_request_rejects_unknown_query_parameters() {
        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("signature".to_string(), "deadbeef".to_string()),
            ("unexpected".to_string(), "value".to_string()),
        ]);

        let response = parse_public_get_request(&query, PublicSourceKind::Path)
            .expect_err("unknown query should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("is not supported"));
    }

    #[test]
    fn prepare_remote_fetch_target_pins_the_validated_netloc() {
        let target = prepare_remote_fetch_target(
            "http://1.1.1.1/image.png",
            &ServerConfig::new(temp_dir("pin"), Some("secret".to_string())),
        )
        .expect("prepare remote target");

        assert_eq!(target.netloc, "1.1.1.1:80");
        assert_eq!(target.addrs, vec![SocketAddr::from(([1, 1, 1, 1], 80))]);
    }

    #[test]
    fn pinned_resolver_rejects_unexpected_netlocs() {
        use ureq::unversioned::resolver::Resolver;

        let resolver = PinnedResolver {
            expected_netloc: "example.com:443".to_string(),
            addrs: vec![SocketAddr::from(([93, 184, 216, 34], 443))],
        };

        let config = ureq::config::Config::builder().build();
        let timeout = ureq::unversioned::transport::NextTimeout {
            after: ureq::unversioned::transport::time::Duration::Exact(
                std::time::Duration::from_secs(30),
            ),
            reason: ureq::Timeout::Resolve,
        };

        let uri: ureq::http::Uri = "https://example.com/path".parse().unwrap();
        let result = resolver
            .resolve(&uri, &config, timeout)
            .expect("resolve expected netloc");
        assert_eq!(&result[..], &[SocketAddr::from(([93, 184, 216, 34], 443))]);

        let bad_uri: ureq::http::Uri = "https://proxy.example:8080/path".parse().unwrap();
        let timeout2 = ureq::unversioned::transport::NextTimeout {
            after: ureq::unversioned::transport::time::Duration::Exact(
                std::time::Duration::from_secs(30),
            ),
            reason: ureq::Timeout::Resolve,
        };
        let error = resolver
            .resolve(&bad_uri, &config, timeout2)
            .expect_err("unexpected netloc should fail");
        assert!(matches!(error, ureq::Error::HostNotFound));
    }

    #[test]
    fn health_live_returns_status_service_version() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/live".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(temp_dir("live"), None));

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse live body");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "truss");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn health_ready_returns_ok_when_storage_exists() {
        let storage = temp_dir("ready-ok");
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(storage, None));

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready body");
        assert_eq!(body["status"], "ok");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "storageRoot" && c["status"] == "ok")
        );
    }

    #[test]
    fn health_ready_returns_503_when_storage_missing() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let config = ServerConfig::new(PathBuf::from("/nonexistent-truss-test-dir"), None);
        let response = route_request(request, &config);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready fail body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "storageRoot" && c["status"] == "fail")
        );
    }

    #[test]
    fn health_ready_returns_503_when_cache_root_missing() {
        let storage = temp_dir("ready-cache-fail");
        let mut config = ServerConfig::new(storage, None);
        config.cache_root = Some(PathBuf::from("/nonexistent-truss-cache-dir"));

        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &config);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready cache body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "cacheRoot" && c["status"] == "fail")
        );
    }

    #[test]
    fn health_returns_comprehensive_diagnostic() {
        let storage = temp_dir("health-diag");
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(storage, None));

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        assert_eq!(body["status"], "ok");
        assert_eq!(body["service"], "truss");
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["uptimeSeconds"].is_u64());
        assert!(body["checks"].is_array());
    }

    #[test]
    fn unknown_path_returns_not_found() {
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/unknown".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let response = route_request(request, &ServerConfig::new(temp_dir("not-found"), None));

        assert_eq!(response.status, "404 Not Found");
        assert_eq!(
            response.content_type.as_deref(),
            Some("application/problem+json")
        );
        let body = response_body(&response);
        assert!(body.contains("\"type\":\"about:blank\""));
        assert!(body.contains("\"title\":\"Not Found\""));
        assert!(body.contains("\"status\":404"));
        assert!(body.contains("not found"));
    }

    #[test]
    fn transform_endpoint_requires_authentication() {
        let storage_root = temp_dir("auth");
        write_png(&storage_root.join("image.png"));
        let mut request = transform_request("/image.png");
        request.headers.retain(|(name, _)| name != "authorization");

        let response = route_request(
            request,
            &ServerConfig::new(storage_root, Some("secret".to_string())),
        );

        assert_eq!(response.status, "401 Unauthorized");
        assert!(response_body(&response).contains("authorization required"));
    }

    #[test]
    fn transform_endpoint_returns_service_unavailable_without_configured_token() {
        let storage_root = temp_dir("token");
        write_png(&storage_root.join("image.png"));

        let response = route_request(
            transform_request("/image.png"),
            &ServerConfig::new(storage_root, None),
        );

        assert_eq!(response.status, "503 Service Unavailable");
        assert!(response_body(&response).contains("bearer token is not configured"));
    }

    #[test]
    fn transform_endpoint_transforms_a_path_source() {
        let storage_root = temp_dir("transform");
        write_png(&storage_root.join("image.png"));

        let response = route_request(
            transform_request("/image.png"),
            &ServerConfig::new(storage_root, Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type.as_deref(), Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
        assert_eq!(artifact.metadata.width, Some(4));
        assert_eq!(artifact.metadata.height, Some(3));
    }

    #[test]
    fn transform_endpoint_rejects_private_url_sources_by_default() {
        let response = route_request(
            transform_url_request("http://127.0.0.1:8080/image.png"),
            &ServerConfig::new(temp_dir("url-blocked"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "403 Forbidden");
        assert!(response_body(&response).contains("port is not allowed"));
    }

    #[test]
    fn transform_endpoint_transforms_a_url_source_when_insecure_allowance_is_enabled() {
        let (url, handle) = spawn_http_server(vec![(
            "200 OK".to_string(),
            vec![("Content-Type".to_string(), "image/png".to_string())],
            png_bytes(),
        )]);

        let response = route_request(
            transform_url_request(&url),
            &ServerConfig::new(temp_dir("url"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type.as_deref(), Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn transform_endpoint_follows_remote_redirects() {
        let (redirect_url, handle) = spawn_http_server(vec![
            (
                "302 Found".to_string(),
                vec![("Location".to_string(), "/final-image".to_string())],
                Vec::new(),
            ),
            (
                "200 OK".to_string(),
                vec![("Content-Type".to_string(), "image/png".to_string())],
                png_bytes(),
            ),
        ]);

        let response = route_request(
            transform_url_request(&redirect_url),
            &ServerConfig::new(temp_dir("redirect"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "200 OK");
        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn upload_endpoint_transforms_uploaded_file() {
        let response = route_request(
            upload_request(&png_bytes(), Some(r#"{"format":"jpeg"}"#)),
            &ServerConfig::new(temp_dir("upload"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
        assert_eq!(response.content_type.as_deref(), Some("image/jpeg"));

        let artifact = sniff_artifact(RawArtifact::new(response.body, None)).expect("sniff output");
        assert_eq!(artifact.media_type, MediaType::Jpeg);
    }

    #[test]
    fn upload_endpoint_requires_a_file_field() {
        let boundary = "truss-test-boundary";
        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                (
                    "content-type".to_string(),
                    format!("multipart/form-data; boundary={boundary}"),
                ),
            ],
            body: format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"jpeg\"}}\r\n--{boundary}--\r\n"
            )
            .into_bytes(),
        };

        let response = route_request(
            request,
            &ServerConfig::new(temp_dir("upload-missing-file"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("requires a `file` field"));
    }

    #[test]
    fn upload_endpoint_rejects_non_multipart_content_type() {
        let request = HttpRequest {
            method: "POST".to_string(),
            target: "/images".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("authorization".to_string(), "Bearer secret".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
            body: br#"{"file":"not-really-json"}"#.to_vec(),
        };

        let response = route_request(
            request,
            &ServerConfig::new(temp_dir("upload-content-type"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "415 Unsupported Media Type");
        assert!(response_body(&response).contains("multipart/form-data"));
    }

    #[test]
    fn parse_upload_request_extracts_file_and_options() {
        let request = upload_request(&png_bytes(), Some(r#"{"width":8,"format":"jpeg"}"#));
        let boundary =
            super::multipart::parse_multipart_boundary(&request).expect("parse boundary");
        let (file_bytes, options) =
            super::multipart::parse_upload_request(&request.body, &boundary)
                .expect("parse upload body");

        assert_eq!(file_bytes, png_bytes());
        assert_eq!(options.width, Some(8));
        assert_eq!(options.format, Some(MediaType::Jpeg));
    }

    #[test]
    fn metrics_endpoint_requires_authentication() {
        let response = route_request(
            metrics_request(false),
            &ServerConfig::new(temp_dir("metrics-auth"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "401 Unauthorized");
        assert!(response_body(&response).contains("authorization required"));
    }

    #[test]
    fn metrics_endpoint_returns_prometheus_text() {
        super::metrics::record_http_metrics(super::metrics::RouteMetric::Health, "200 OK");
        let response = route_request(
            metrics_request(true),
            &ServerConfig::new(temp_dir("metrics"), Some("secret".to_string())),
        );
        let body = response_body(&response);

        assert_eq!(response.status, "200 OK");
        assert_eq!(
            response.content_type.as_deref(),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        assert!(body.contains("truss_http_requests_total"));
        assert!(body.contains("truss_http_requests_by_route_total{route=\"/health\"}"));
        assert!(body.contains("truss_http_responses_total{status=\"200\"}"));
    }

    #[test]
    fn transform_endpoint_rejects_unsupported_remote_content_encoding() {
        let (url, handle) = spawn_http_server(vec![(
            "200 OK".to_string(),
            vec![
                ("Content-Type".to_string(), "image/png".to_string()),
                ("Content-Encoding".to_string(), "compress".to_string()),
            ],
            png_bytes(),
        )]);

        let response = route_request(
            transform_url_request(&url),
            &ServerConfig::new(temp_dir("encoding"), Some("secret".to_string()))
                .with_insecure_url_sources(true),
        );

        handle.join().expect("join fixture server");

        assert_eq!(response.status, "502 Bad Gateway");
        assert!(response_body(&response).contains("unsupported content-encoding"));
    }

    #[test]
    fn resolve_storage_path_rejects_parent_segments() {
        let storage_root = temp_dir("resolve");
        let response = resolve_storage_path(&storage_root, "../escape.png")
            .expect_err("parent segments should be rejected");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("must not contain root"));
    }

    #[test]
    fn read_request_parses_headers_and_body() {
        let request_bytes = b"POST /images:transform HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(request_bytes);
        let request = read_request(&mut cursor).expect("parse request");

        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/images:transform");
        assert_eq!(request.version, "HTTP/1.1");
        assert_eq!(request.header("host"), Some("localhost"));
        assert_eq!(request.body, b"{}");
    }

    #[test]
    fn read_request_rejects_duplicate_content_length() {
        let request_bytes =
            b"POST /images:transform HTTP/1.1\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}";
        let mut cursor = Cursor::new(request_bytes);
        let response = read_request(&mut cursor).expect_err("duplicate headers should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("content-length"));
    }

    #[test]
    fn serve_once_handles_a_tcp_request() {
        let storage_root = temp_dir("serve-once");
        let config = ServerConfig::new(storage_root, None);
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("read local addr");

        let server = thread::spawn(move || serve_once_with_config(listener, &config));

        let mut stream = TcpStream::connect(addr).expect("connect to test server");
        stream
            .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .expect("write request");

        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read response");

        server
            .join()
            .expect("join test server thread")
            .expect("serve one request");

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("Content-Type: application/json"));
        assert!(response.contains("\"status\":\"ok\""));
        assert!(response.contains("\"service\":\"truss\""));
        assert!(response.contains("\"version\":"));
    }

    #[test]
    fn helper_error_responses_use_rfc7807_problem_details() {
        let response = auth_required_response("authorization required");
        let bad_request = bad_request_response("bad input");

        assert_eq!(
            response.content_type.as_deref(),
            Some("application/problem+json"),
            "error responses must use application/problem+json"
        );
        assert_eq!(
            bad_request.content_type.as_deref(),
            Some("application/problem+json"),
        );

        let auth_body = response_body(&response);
        assert!(auth_body.contains("authorization required"));
        assert!(auth_body.contains("\"type\":\"about:blank\""));
        assert!(auth_body.contains("\"title\":\"Unauthorized\""));
        assert!(auth_body.contains("\"status\":401"));

        let bad_body = response_body(&bad_request);
        assert!(bad_body.contains("bad input"));
        assert!(bad_body.contains("\"type\":\"about:blank\""));
        assert!(bad_body.contains("\"title\":\"Bad Request\""));
        assert!(bad_body.contains("\"status\":400"));
    }

    #[test]
    fn parse_headers_rejects_duplicate_host() {
        let lines = "Host: example.com\r\nHost: evil.com\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_authorization() {
        let lines = "Authorization: Bearer a\r\nAuthorization: Bearer b\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_content_type() {
        let lines = "Content-Type: application/json\r\nContent-Type: text/plain\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_transfer_encoding() {
        let lines = "Transfer-Encoding: chunked\r\nTransfer-Encoding: gzip\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_single_transfer_encoding() {
        let lines = "Host: example.com\r\nTransfer-Encoding: chunked\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        let err = result.unwrap_err();
        assert!(
            err.status.starts_with("501"),
            "expected 501 status, got: {}",
            err.status
        );
        assert!(
            String::from_utf8_lossy(&err.body).contains("Transfer-Encoding"),
            "error response should mention Transfer-Encoding"
        );
    }

    #[test]
    fn parse_headers_rejects_transfer_encoding_identity() {
        let lines = "Transfer-Encoding: identity\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_allows_single_instances_of_singleton_headers() {
        let lines =
            "Host: example.com\r\nAuthorization: Bearer tok\r\nContent-Type: application/json\r\n";
        let result = super::http_parse::parse_headers(lines.split("\r\n"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 3);
    }

    #[test]
    fn max_body_for_multipart_uses_upload_limit() {
        let headers = vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=abc".to_string(),
        )];
        assert_eq!(
            super::http_parse::max_body_for_headers(&headers),
            super::http_parse::MAX_UPLOAD_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_json_uses_default_limit() {
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        assert_eq!(
            super::http_parse::max_body_for_headers(&headers),
            super::http_parse::MAX_REQUEST_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_no_content_type_uses_default_limit() {
        let headers: Vec<(String, String)> = vec![];
        assert_eq!(
            super::http_parse::max_body_for_headers(&headers),
            super::http_parse::MAX_REQUEST_BODY_BYTES
        );
    }

    fn make_test_config() -> ServerConfig {
        ServerConfig::new(std::env::temp_dir(), None)
    }

    #[test]
    fn versioned_source_hash_returns_none_without_version() {
        let source = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: None,
        };
        assert!(source.versioned_source_hash(&make_test_config()).is_none());
    }

    #[test]
    fn versioned_source_hash_is_deterministic() {
        let cfg = make_test_config();
        let source = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let hash1 = source.versioned_source_hash(&cfg).unwrap();
        let hash2 = source.versioned_source_hash(&cfg).unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn versioned_source_hash_differs_by_version() {
        let cfg = make_test_config();
        let v1 = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let v2 = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v2".to_string()),
        };
        assert_ne!(
            v1.versioned_source_hash(&cfg).unwrap(),
            v2.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    fn versioned_source_hash_differs_by_kind() {
        let cfg = make_test_config();
        let path = TransformSourcePayload::Path {
            path: "example.com/image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let url = TransformSourcePayload::Url {
            url: "example.com/image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            path.versioned_source_hash(&cfg).unwrap(),
            url.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    fn versioned_source_hash_differs_by_storage_root() {
        let cfg1 = ServerConfig::new(PathBuf::from("/data/images"), None);
        let cfg2 = ServerConfig::new(PathBuf::from("/other/images"), None);
        let source = TransformSourcePayload::Path {
            path: "/photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg1).unwrap(),
            source.versioned_source_hash(&cfg2).unwrap()
        );
    }

    #[test]
    fn versioned_source_hash_differs_by_insecure_flag() {
        let mut cfg1 = make_test_config();
        cfg1.allow_insecure_url_sources = false;
        let mut cfg2 = make_test_config();
        cfg2.allow_insecure_url_sources = true;
        let source = TransformSourcePayload::Url {
            url: "http://example.com/img.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg1).unwrap(),
            source.versioned_source_hash(&cfg2).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_variant_is_deterministic() {
        let cfg = make_test_config();
        let source = TransformSourcePayload::Storage {
            bucket: Some("my-bucket".to_string()),
            key: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let hash1 = source.versioned_source_hash(&cfg).unwrap();
        let hash2 = source.versioned_source_hash(&cfg).unwrap();
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_differs_from_path() {
        let cfg = make_test_config();
        let path_source = TransformSourcePayload::Path {
            path: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let storage_source = TransformSourcePayload::Storage {
            bucket: Some("my-bucket".to_string()),
            key: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            path_source.versioned_source_hash(&cfg).unwrap(),
            storage_source.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_differs_by_bucket() {
        let cfg = make_test_config();
        let s1 = TransformSourcePayload::Storage {
            bucket: Some("bucket-a".to_string()),
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        let s2 = TransformSourcePayload::Storage {
            bucket: Some("bucket-b".to_string()),
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            s1.versioned_source_hash(&cfg).unwrap(),
            s2.versioned_source_hash(&cfg).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_differs_by_backend() {
        let cfg_fs = make_test_config();
        let mut cfg_s3 = make_test_config();
        cfg_s3.storage_backend = super::s3::StorageBackend::S3;

        let source = TransformSourcePayload::Path {
            path: "photos/hero.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg_fs).unwrap(),
            source.versioned_source_hash(&cfg_s3).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_storage_differs_by_endpoint() {
        let mut cfg_a = make_test_config();
        cfg_a.storage_backend = super::s3::StorageBackend::S3;
        cfg_a.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "shared",
            Some("http://minio-a:9000"),
        )));

        let mut cfg_b = make_test_config();
        cfg_b.storage_backend = super::s3::StorageBackend::S3;
        cfg_b.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "shared",
            Some("http://minio-b:9000"),
        )));

        let source = TransformSourcePayload::Storage {
            bucket: None,
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        assert_ne!(
            source.versioned_source_hash(&cfg_a).unwrap(),
            source.versioned_source_hash(&cfg_b).unwrap(),
        );
        assert_ne!(cfg_a, cfg_b);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_backend_default_is_filesystem() {
        let cfg = make_test_config();
        assert_eq!(cfg.storage_backend, super::s3::StorageBackend::Filesystem);
        assert!(cfg.s3_context.is_none());
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_payload_deserializes_storage_variant() {
        let json = r#"{"source":{"kind":"storage","key":"photos/hero.jpg"},"options":{}}"#;
        let payload: super::TransformImageRequestPayload = serde_json::from_str(json).unwrap();
        match payload.source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert!(bucket.is_none());
                assert_eq!(key, "photos/hero.jpg");
                assert!(version.is_none());
            }
            _ => panic!("expected Storage variant"),
        }
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_payload_deserializes_with_bucket() {
        let json = r#"{"source":{"kind":"storage","bucket":"my-bucket","key":"img.png","version":"v2"},"options":{}}"#;
        let payload: super::TransformImageRequestPayload = serde_json::from_str(json).unwrap();
        match payload.source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert_eq!(bucket.as_deref(), Some("my-bucket"));
                assert_eq!(key, "img.png");
                assert_eq!(version.as_deref(), Some("v2"));
            }
            _ => panic!("expected Storage variant"),
        }
    }

    // -----------------------------------------------------------------------
    // S3: default_bucket fallback with bucket: None
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_uses_default_bucket_when_bucket_is_none() {
        let mut cfg_a = make_test_config();
        cfg_a.storage_backend = super::s3::StorageBackend::S3;
        cfg_a.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "bucket-a", None,
        )));

        let mut cfg_b = make_test_config();
        cfg_b.storage_backend = super::s3::StorageBackend::S3;
        cfg_b.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "bucket-b", None,
        )));

        let source = TransformSourcePayload::Storage {
            bucket: None,
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        // Different default_bucket ⇒ different hash
        assert_ne!(
            source.versioned_source_hash(&cfg_a).unwrap(),
            source.versioned_source_hash(&cfg_b).unwrap(),
        );
        // PartialEq also distinguishes them
        assert_ne!(cfg_a, cfg_b);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn versioned_source_hash_returns_none_without_bucket_or_context() {
        let mut cfg = make_test_config();
        cfg.storage_backend = super::s3::StorageBackend::S3;
        cfg.s3_context = None;

        let source = TransformSourcePayload::Storage {
            bucket: None,
            key: "image.jpg".to_string(),
            version: Some("v1".to_string()),
        };
        // No bucket available ⇒ None (falls back to content-hash)
        assert!(source.versioned_source_hash(&cfg).is_none());
    }

    // -----------------------------------------------------------------------
    // S3: from_env branches
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_rejects_invalid_storage_backend() {
        let storage = temp_dir("env-bad-backend");
        unsafe {
            std::env::set_var("TRUSS_STORAGE_ROOT", storage.to_str().unwrap());
            std::env::set_var("TRUSS_STORAGE_BACKEND", "gcs");
        }
        let result = ServerConfig::from_env();
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_BACKEND");
            std::env::remove_var("TRUSS_STORAGE_ROOT");
        }
        let _ = std::fs::remove_dir_all(storage);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("unknown storage backend"), "got: {msg}");
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_rejects_s3_without_bucket() {
        let storage = temp_dir("env-no-bucket");
        unsafe {
            std::env::set_var("TRUSS_STORAGE_ROOT", storage.to_str().unwrap());
            std::env::set_var("TRUSS_STORAGE_BACKEND", "s3");
            std::env::remove_var("TRUSS_S3_BUCKET");
        }
        let result = ServerConfig::from_env();
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_BACKEND");
            std::env::remove_var("TRUSS_STORAGE_ROOT");
        }
        let _ = std::fs::remove_dir_all(storage);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("TRUSS_S3_BUCKET"), "got: {msg}");
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_accepts_s3_with_bucket() {
        let storage = temp_dir("env-s3-ok");
        unsafe {
            std::env::set_var("TRUSS_STORAGE_ROOT", storage.to_str().unwrap());
            std::env::set_var("TRUSS_STORAGE_BACKEND", "s3");
            std::env::set_var("TRUSS_S3_BUCKET", "my-images");
        }
        let result = ServerConfig::from_env();
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_BACKEND");
            std::env::remove_var("TRUSS_S3_BUCKET");
            std::env::remove_var("TRUSS_STORAGE_ROOT");
        }
        let _ = std::fs::remove_dir_all(storage);
        let cfg = result.expect("from_env should succeed with s3 + bucket");
        assert_eq!(cfg.storage_backend, super::s3::StorageBackend::S3);
        let ctx = cfg.s3_context.expect("s3_context should be Some");
        assert_eq!(ctx.default_bucket, "my-images");
    }

    // -----------------------------------------------------------------------
    // S3: health endpoint
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "s3")]
    fn health_ready_s3_returns_503_when_context_missing() {
        let storage = temp_dir("health-s3-no-ctx");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::s3::StorageBackend::S3;
        config.s3_context = None;

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "s3Client" && c["status"] == "fail"),
            "expected s3Client fail check in {body}",
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn health_ready_s3_includes_s3_client_check() {
        let storage = temp_dir("health-s3-ok");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::s3::StorageBackend::S3;
        config.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "test-bucket",
            None,
        )));

        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/health/ready".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "s3Client" && c["status"] == "ok"),
            "expected s3Client ok check in {body}",
        );
    }

    // -----------------------------------------------------------------------
    // S3: public by-path remap (leading slash trimmed, Storage variant used)
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "s3")]
    fn public_by_path_s3_remaps_to_storage_and_trims_leading_slash() {
        let storage = temp_dir("by-path-s3-remap");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::s3::StorageBackend::S3;
        config.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "my-bucket",
            None,
        )));

        // Craft a minimal GET /images/by-path?path=/photos/hero.jpg&version=v1 request.
        // We expect a 502 (S3 unreachable) rather than a filesystem error, proving the
        // Storage branch is taken.
        let request = super::http_parse::HttpRequest {
            method: "GET".to_string(),
            target: "/images/by-path?path=/photos/hero.jpg&version=v1".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("Accept".to_string(), "image/jpeg".to_string())],
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        let _ = std::fs::remove_dir_all(storage);

        // Without a live S3 endpoint the request must fail with "S3 storage backend
        // is not configured" (s3_context is set but the client points nowhere real)
        // or 502 from the SDK. Either way it must NOT be a filesystem 404.
        assert_ne!(
            response.status, "404 Not Found",
            "expected S3 resolution, not filesystem lookup",
        );
    }

    #[test]
    fn read_request_rejects_json_body_over_1mib() {
        let body = vec![b'x'; super::http_parse::MAX_REQUEST_BODY_BYTES + 1];
        let content_length = body.len();
        let raw = format!(
            "POST /images:transform HTTP/1.1\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {content_length}\r\n\r\n"
        );
        let mut data = raw.into_bytes();
        data.extend_from_slice(&body);
        let result = read_request(&mut data.as_slice());
        assert!(result.is_err());
    }

    #[test]
    fn read_request_accepts_multipart_body_over_1mib() {
        let payload_size = super::http_parse::MAX_REQUEST_BODY_BYTES + 100;
        let body_content = vec![b'A'; payload_size];
        let boundary = "test-boundary-123";
        let mut body = Vec::new();
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.jpg\"\r\n\r\n").as_bytes());
        body.extend_from_slice(&body_content);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let content_length = body.len();
        let raw = format!(
            "POST /images HTTP/1.1\r\n\
             Content-Type: multipart/form-data; boundary={boundary}\r\n\
             Content-Length: {content_length}\r\n\r\n"
        );
        let mut data = raw.into_bytes();
        data.extend_from_slice(&body);
        let result = read_request(&mut data.as_slice());
        assert!(
            result.is_ok(),
            "multipart upload over 1 MiB should be accepted"
        );
    }

    #[test]
    fn multipart_boundary_in_payload_does_not_split_part() {
        let boundary = "abc123";
        let fake_boundary_in_payload = format!("\r\n--{boundary}NOTREAL");
        let part_body = format!("before{fake_boundary_in_payload}after");
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\
             {part_body}\r\n\
             --{boundary}--\r\n"
        );

        let parts = parse_multipart_form_data(body.as_bytes(), boundary)
            .expect("should parse despite boundary-like string in payload");
        assert_eq!(parts.len(), 1, "should have exactly one part");

        let part_data = &body.as_bytes()[parts[0].body_range.clone()];
        let part_text = std::str::from_utf8(part_data).unwrap();
        assert!(
            part_text.contains("NOTREAL"),
            "part body should contain the full fake boundary string"
        );
    }

    #[test]
    fn multipart_normal_two_parts_still_works() {
        let boundary = "testboundary";
        let body = format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"field1\"\r\n\r\n\
             value1\r\n\
             --{boundary}\r\n\
             Content-Disposition: form-data; name=\"field2\"\r\n\r\n\
             value2\r\n\
             --{boundary}--\r\n"
        );

        let parts = parse_multipart_form_data(body.as_bytes(), boundary)
            .expect("should parse two normal parts");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "field1");
        assert_eq!(parts[1].name, "field2");
    }
}
