use crate::{
    Artifact, Fit, MediaType, Position, RawArtifact, Rgba8, Rotation, TransformError,
    TransformOptions, TransformRequest, sniff_artifact, transform_raster, transform_svg,
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use ureq::http;
use url::Url;

/// The default bind address for the development HTTP server.
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:8080";

/// The default storage root used by the server adapter.
pub const DEFAULT_STORAGE_ROOT: &str = ".";

const NOT_FOUND_BODY: &str =
    "{\"type\":\"about:blank\",\"title\":\"Not Found\",\"status\":404,\"detail\":\"not found\"}\n";
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
const MAX_UPLOAD_BODY_BYTES: usize = 100 * 1024 * 1024;
const MAX_SOURCE_BYTES: u64 = 100 * 1024 * 1024;
const MAX_REMOTE_REDIRECTS: usize = 5;
const DEFAULT_PUBLIC_MAX_AGE_SECONDS: u32 = 3600;
const DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS: u32 = 60;
const DEFAULT_CACHE_TTL_SECONDS: u64 = 3600;
const SOCKET_READ_TIMEOUT: Duration = Duration::from_secs(60);
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(60);
/// Number of worker threads for handling incoming connections concurrently.
const WORKER_THREADS: usize = 8;
type HmacSha256 = Hmac<Sha256>;

static HTTP_REQUESTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_HEALTH_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_HEALTH_LIVE_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_HEALTH_READY_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_PUBLIC_BY_PATH_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_PUBLIC_BY_URL_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_TRANSFORM_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_UPLOAD_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_METRICS_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUESTS_UNKNOWN_TOTAL: AtomicU64 = AtomicU64::new(0);

static HTTP_RESPONSES_200_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_400_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_401_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_403_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_404_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_406_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_413_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_415_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_500_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_501_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_502_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_503_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_508_TOTAL: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSES_OTHER_TOTAL: AtomicU64 = AtomicU64::new(0);

static TRANSFORMS_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
static CACHE_HITS_TOTAL: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES_TOTAL: AtomicU64 = AtomicU64::new(0);
static ORIGIN_CACHE_HITS_TOTAL: AtomicU64 = AtomicU64::new(0);
static ORIGIN_CACHE_MISSES_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Monotonically increasing counter used to generate unique temp-file suffixes
/// for cache writes.  Combined with the process ID this avoids collisions from
/// concurrent writers within the same process.
static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Maximum number of concurrent image transforms allowed. When this limit is
/// reached, new transform requests are rejected with 503 Service Unavailable.
const MAX_CONCURRENT_TRANSFORMS: u64 = 64;

/// Process start time used to compute uptime in the `/health` diagnostic endpoint.
static START_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Returns the process uptime in seconds since the first call to this function.
fn uptime_seconds() -> u64 {
    START_TIME.get_or_init(Instant::now).elapsed().as_secs()
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
    /// Optional logging callback for diagnostic messages.
    ///
    /// When set, the server routes all diagnostic messages (cache errors, connection
    /// failures, transform warnings) through this handler. When `None`, messages are
    /// written to stderr via `eprintln!`.
    pub log_handler: Option<LogHandler>,
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
            log_handler: self.log_handler.clone(),
        }
    }
}

impl fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerConfig")
            .field("storage_root", &self.storage_root)
            .field("bearer_token", &self.bearer_token)
            .field("public_base_url", &self.public_base_url)
            .field("signed_url_key_id", &self.signed_url_key_id)
            .field("signed_url_secret", &self.signed_url_secret)
            .field(
                "allow_insecure_url_sources",
                &self.allow_insecure_url_sources,
            )
            .field("cache_root", &self.cache_root)
            .field("log_handler", &self.log_handler.as_ref().map(|_| ".."))
            .finish()
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
            log_handler: None,
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

        let cache_root = env::var("TRUSS_CACHE_ROOT")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);

        Ok(Self {
            storage_root,
            bearer_token,
            public_base_url,
            signed_url_key_id,
            signed_url_secret,
            allow_insecure_url_sources: env_flag("TRUSS_ALLOW_INSECURE_URL_SOURCES"),
            cache_root,
            log_handler: None,
        })
    }
}

/// On-disk transform cache using a sharded directory layout.
///
/// The cache stores transformed image bytes under `<root>/ab/cd/ef/<sha256_hex>`, where
/// `ab`, `cd`, `ef` are the first three byte-pairs of the hex-encoded cache key. Each file
/// starts with a media-type header line (e.g. `"jpeg\n"`) followed by the raw output bytes.
///
/// Staleness is determined by file modification time. Entries older than
/// [`DEFAULT_CACHE_TTL_SECONDS`] are treated as misses and overwritten on the next transform.
///
/// The cache does not perform size-based eviction. Operators should use external tools
/// (e.g. `tmpwatch`, `tmpreaper`, or a cron job) to manage disk usage.
struct TransformCache {
    root: PathBuf,
    ttl: Duration,
    log_handler: Option<LogHandler>,
}

/// The result of a cache lookup.
#[derive(Debug)]
enum CacheLookup {
    /// The entry was found and is still fresh.
    Hit {
        media_type: MediaType,
        body: Vec<u8>,
        age: Duration,
    },
    /// The entry was not found or is stale.
    Miss,
}

impl TransformCache {
    /// Creates a new transform cache rooted at the given directory.
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECONDS),
            log_handler: None,
        }
    }

    fn with_log_handler(mut self, handler: Option<LogHandler>) -> Self {
        self.log_handler = handler;
        self
    }

    fn log(&self, msg: &str) {
        if let Some(handler) = &self.log_handler {
            handler(msg);
        } else {
            eprintln!("{msg}");
        }
    }

    /// Returns the sharded file path for the given cache key.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `key` is a 64-character hex string (SHA-256 output).
    fn entry_path(&self, key: &str) -> PathBuf {
        debug_assert!(
            key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()),
            "cache key must be a 64-character hex string"
        );
        // Layout: <root>/ab/cd/ef/<key>
        // where ab, cd, ef are the first 6 hex characters split into pairs.
        let a = &key[0..2];
        let b = &key[2..4];
        let c = &key[4..6];
        self.root.join(a).join(b).join(c).join(key)
    }

    /// Looks up a cached transform result.
    ///
    /// Returns [`CacheLookup::Hit`] if the file exists, is readable, and its modification
    /// time is within the TTL. Returns [`CacheLookup::Miss`] otherwise.
    fn get(&self, key: &str) -> CacheLookup {
        let path = self.entry_path(key);

        // Open a single file handle to avoid TOCTOU between read and metadata.
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(_) => return CacheLookup::Miss,
        };

        // Check staleness via mtime on the same file handle.
        let age = match file
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|mtime| mtime.elapsed().map_err(io::Error::other))
        {
            Ok(age) => age,
            Err(_) => return CacheLookup::Miss,
        };

        if age > self.ttl {
            return CacheLookup::Miss;
        }

        let mut data = Vec::new();
        if io::Read::read_to_end(&mut &file, &mut data).is_err() {
            return CacheLookup::Miss;
        }

        // Parse the header line: "<media_type>\n<body>"
        let newline_pos = match data.iter().position(|&b| b == b'\n') {
            Some(pos) => pos,
            None => return CacheLookup::Miss,
        };
        let media_type_str = match std::str::from_utf8(&data[..newline_pos]) {
            Ok(s) => s,
            Err(_) => return CacheLookup::Miss,
        };
        let media_type = match MediaType::from_str(media_type_str) {
            Ok(mt) => mt,
            Err(_) => return CacheLookup::Miss,
        };

        // Remove the header in-place to avoid a second allocation.
        data.drain(..=newline_pos);

        CacheLookup::Hit {
            media_type,
            body: data,
            age,
        }
    }

    /// Writes a transform result to the cache.
    ///
    /// Uses write-to-tempfile-then-rename for atomic writes, preventing readers from seeing
    /// partial data.
    fn put(&self, key: &str, media_type: MediaType, body: &[u8]) {
        let path = self.entry_path(key);
        if let Some(parent) = path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            self.log(&format!("truss: cache mkdir failed: {err}"));
            return;
        }

        // Write to a temp file with a unique suffix, then rename atomically.
        let tmp_path = path.with_extension(unique_tmp_suffix());
        let mut header = media_type.as_name().as_bytes().to_vec();
        header.push(b'\n');

        let result = (|| -> io::Result<()> {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(&header)?;
            file.write_all(body)?;
            file.sync_all()?;
            fs::rename(&tmp_path, &path)?;
            Ok(())
        })();

        if let Err(err) = result {
            self.log(&format!("truss: cache write failed: {err}"));
            // Clean up the temp file if it exists.
            let _ = fs::remove_file(&tmp_path);
        }
    }
}

/// On-disk origin response cache for remote URL fetches.
///
/// Caches raw source bytes fetched from remote URLs so repeated requests for the same
/// remote source avoid redundant HTTP round-trips. This sits in front of the transform
/// cache in the cache hierarchy (design doc §8.1).
///
/// The cache key is the SHA-256 of the canonical URL string. The stored value is the
/// raw source bytes with no header. Staleness uses the same mtime-based TTL as the
/// transform cache.
struct OriginCache {
    root: PathBuf,
    ttl: Duration,
    log_handler: Option<LogHandler>,
}

impl OriginCache {
    /// Creates a new origin cache rooted at `<cache_root>/origin/`.
    fn new(cache_root: &Path) -> Self {
        Self {
            root: cache_root.join("origin"),
            ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECONDS),
            log_handler: None,
        }
    }

    fn with_log_handler(mut self, handler: Option<LogHandler>) -> Self {
        self.log_handler = handler;
        self
    }

    fn log(&self, msg: &str) {
        if let Some(handler) = &self.log_handler {
            handler(msg);
        } else {
            eprintln!("{msg}");
        }
    }

    /// Returns the sharded file path for the given URL.
    fn entry_path(&self, url: &str) -> PathBuf {
        let key = hex::encode(Sha256::digest(url.as_bytes()));
        let a = &key[0..2];
        let b = &key[2..4];
        let c = &key[4..6];
        self.root.join(a).join(b).join(c).join(&key)
    }

    /// Looks up cached source bytes for a remote URL.
    fn get(&self, url: &str) -> Option<Vec<u8>> {
        let path = self.entry_path(url);
        let file = fs::File::open(&path).ok()?;

        let age = file
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|mtime| mtime.elapsed().map_err(io::Error::other))
            .ok()?;

        if age > self.ttl {
            return None;
        }

        let mut data = Vec::new();
        io::Read::read_to_end(&mut &file, &mut data).ok()?;
        Some(data)
    }

    /// Writes fetched source bytes to the origin cache.
    fn put(&self, url: &str, body: &[u8]) {
        let path = self.entry_path(url);
        if let Some(parent) = path.parent()
            && let Err(err) = fs::create_dir_all(parent)
        {
            self.log(&format!("truss: origin cache mkdir failed: {err}"));
            return;
        }

        let tmp_path = path.with_extension(unique_tmp_suffix());
        let result = (|| -> io::Result<()> {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(body)?;
            file.sync_all()?;
            fs::rename(&tmp_path, &path)?;
            Ok(())
        })();

        if let Err(err) = result {
            self.log(&format!("truss: origin cache write failed: {err}"));
            let _ = fs::remove_file(&tmp_path);
        }
    }
}

/// Returns a unique temporary-file suffix for cache writes.
///
/// The suffix combines the process ID with a monotonically increasing counter
/// so that concurrent writers within the same process never collide on the
/// same temp path (the previous PID-only scheme could).
fn unique_tmp_suffix() -> String {
    let seq = CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("tmp.{}.{seq}", std::process::id())
}

/// Computes a SHA-256 cache key from the source identifier, transform options, and
/// optionally the negotiated Accept value.
///
/// The canonical form follows the design specification (§8.2):
/// ```text
/// SHA256(
///   canonical_source_identifier + "\n" +
///   canonical_transform_parameters + "\n" +
///   normalized_accept_if_negotiation_enabled_and_format_absent
/// )
/// ```
///
/// Auth-related parameters (`keyId`, `expires`, `signature`) are excluded. The `deadline`
/// field is excluded because it is an adapter concern, not a transform identity.
fn compute_cache_key(
    source_identifier: &str,
    options: &TransformOptions,
    negotiated_accept: Option<&str>,
) -> String {
    let mut canonical = String::new();
    canonical.push_str(source_identifier);
    canonical.push('\n');

    // Build sorted canonical transform parameters.
    //
    // Where the core `TransformOptions::normalize()` method fills in defaults
    // (e.g. fit → Contain, position → Center when width+height are set), we
    // replicate the same defaults here so that the omitted-vs-explicit-default
    // distinction does not produce different cache keys for identical transforms.
    let has_bounded_resize = options.width.is_some() && options.height.is_some();

    let mut params: Vec<(&str, String)> = Vec::new();
    if options.auto_orient {
        params.push(("autoOrient", "true".to_string()));
    }
    if let Some(bg) = &options.background {
        params.push((
            "background",
            format!("{:02x}{:02x}{:02x}{:02x}", bg.r, bg.g, bg.b, bg.a),
        ));
    }
    if has_bounded_resize {
        let fit = options.fit.unwrap_or(Fit::Contain);
        params.push(("fit", fit.as_name().to_string()));
    }
    if let Some(format) = options.format {
        params.push(("format", format.as_name().to_string()));
    }
    if let Some(h) = options.height {
        params.push(("height", h.to_string()));
    }
    if has_bounded_resize {
        let pos = options.position.unwrap_or(Position::Center);
        params.push(("position", pos.as_name().to_string()));
    }
    if options.preserve_exif {
        params.push(("preserveExif", "true".to_string()));
    }
    if let Some(q) = options.quality {
        params.push(("quality", q.to_string()));
    }
    if options.rotate != Rotation::Deg0 {
        params.push(("rotate", options.rotate.as_degrees().to_string()));
    }
    if options.strip_metadata {
        params.push(("stripMetadata", "true".to_string()));
    }
    if let Some(w) = options.width {
        params.push(("width", w.to_string()));
    }
    // Sort to guarantee a stable canonical form regardless of insertion order.
    params.sort_by_key(|(k, _)| *k);
    for (i, (k, v)) in params.iter().enumerate() {
        if i > 0 {
            canonical.push('&');
        }
        canonical.push_str(k);
        canonical.push('=');
        canonical.push_str(v);
    }

    canonical.push('\n');
    if let Some(accept) = negotiated_accept {
        canonical.push_str(accept);
    }

    let digest = Sha256::digest(canonical.as_bytes());
    hex::encode(digest)
}

/// Attempts a cache lookup using a version-based source hash, which avoids reading
/// the full source bytes. Returns `Some(response)` on a cache hit (including `304`
/// for conditional requests). Returns `None` on miss or when a version-based lookup
/// is not possible (no version, no cache, or format not yet known).
fn try_versioned_cache_lookup(
    versioned_hash: Option<&str>,
    options: &TransformOptions,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    config: &ServerConfig,
) -> Option<HttpResponse> {
    let source_hash = versioned_hash?;
    let cache_root = config.cache_root.as_ref()?;
    // We can only do a pre-lookup when the output format is already set, because
    // Accept negotiation requires sniffing the source to know the input type.
    options.format?;

    let cache =
        TransformCache::new(cache_root.clone()).with_log_handler(config.log_handler.clone());
    let cache_key = compute_cache_key(source_hash, options, None);
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
        );
        headers.push(("Age".to_string(), age.as_secs().to_string()));
        if matches!(response_policy, ImageResponsePolicy::PublicGet)
            && if_none_match_matches(request.header("if-none-match"), &etag)
        {
            return Some(HttpResponse::empty("304 Not Modified", headers));
        }
        return Some(HttpResponse::binary_with_headers(
            "200 OK",
            media_type.as_mime(),
            headers,
            body,
        ));
    }
    None
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find_map(|(header_name, value)| (header_name == name).then_some(value.as_str()))
    }

    fn path(&self) -> &str {
        self.target
            .split('?')
            .next()
            .unwrap_or(self.target.as_str())
    }

    fn query(&self) -> Option<&str> {
        self.target.split_once('?').map(|(_, query)| query)
    }
}

/// Partially parsed HTTP request containing only the request line and headers.
/// The body has not been read yet. Used to perform early authentication before
/// consuming the (potentially large) request body.
struct PartialHttpRequest {
    method: String,
    target: String,
    version: String,
    headers: Vec<(String, String)>,
    /// Bytes already buffered beyond the header terminator during header reading.
    /// These belong to the body and are passed to `read_request_body`.
    overflow: Vec<u8>,
    /// The validated Content-Length value.
    content_length: usize,
}

impl PartialHttpRequest {
    fn path(&self) -> &str {
        self.target
            .split('?')
            .next()
            .unwrap_or(self.target.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpResponse {
    status: &'static str,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn json(status: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some("application/json".to_string()),
            headers: Vec::new(),
            body,
        }
    }

    fn problem(status: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some("application/problem+json".to_string()),
            headers: Vec::new(),
            body,
        }
    }

    fn problem_with_headers(
        status: &'static str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            content_type: Some("application/problem+json".to_string()),
            headers,
            body,
        }
    }

    fn binary_with_headers(
        status: &'static str,
        content_type: &str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Self {
        Self {
            status,
            content_type: Some(content_type.to_string()),
            headers,
            body,
        }
    }

    fn text(status: &'static str, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            content_type: Some(content_type.to_string()),
            headers: Vec::new(),
            body,
        }
    }

    fn empty(status: &'static str, headers: Vec<(String, String)>) -> Self {
        Self {
            status,
            content_type: None,
            headers,
            body: Vec::new(),
        }
    }
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
        let (kind, reference, version) = match self {
            Self::Path { path, version } => ("path", path.as_str(), version.as_deref()),
            Self::Url { url, version } => ("url", url.as_str(), version.as_deref()),
        };
        let version = version?;
        // Use newline separators so that values containing colons cannot collide
        // with different (reference, version) pairs. Include configuration boundaries
        // to prevent cross-instance cache poisoning.
        let mut id = String::new();
        id.push_str(kind);
        id.push('\n');
        id.push_str(reference);
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
            deadline: defaults.deadline,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MultipartPart {
    name: String,
    content_type: Option<String>,
    /// Byte range within the original request body, avoiding a copy of the part data.
    body_range: std::ops::Range<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteMetric {
    Health,
    HealthLive,
    HealthReady,
    PublicByPath,
    PublicByUrl,
    Transform,
    Upload,
    Metrics,
    Unknown,
}

impl RouteMetric {
    const fn as_label(self) -> &'static str {
        match self {
            Self::Health => "/health",
            Self::HealthLive => "/health/live",
            Self::HealthReady => "/health/ready",
            Self::PublicByPath => "/images/by-path",
            Self::PublicByUrl => "/images/by-url",
            Self::Transform => "/images:transform",
            Self::Upload => "/images",
            Self::Metrics => "/metrics",
            Self::Unknown => "<unknown>",
        }
    }
}

fn handle_stream(mut stream: TcpStream, config: &ServerConfig) -> io::Result<()> {
    // Prevent slow or stalled clients from blocking the accept loop indefinitely.
    // Errors from set_*_timeout are non-fatal: the worst case is falling back to the
    // OS default (usually no timeout), which is the pre-existing behaviour.
    if let Err(err) = stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT)) {
        config.log(&format!("failed to set socket read timeout: {err}"));
    }
    if let Err(err) = stream.set_write_timeout(Some(SOCKET_WRITE_TIMEOUT)) {
        config.log(&format!("failed to set socket write timeout: {err}"));
    }

    // Phase 1: Read headers only — no body bytes are consumed yet.
    let partial = match read_request_headers(&mut stream) {
        Ok(partial) => partial,
        Err(response) => return write_response(&mut stream, response),
    };

    // Phase 2: Authenticate private POST routes *before* reading the body.
    // This prevents unauthenticated clients from forcing the server to buffer
    // up to MAX_UPLOAD_BODY_BYTES of request body.
    let requires_auth = matches!(
        (partial.method.as_str(), partial.path()),
        ("POST", "/images:transform") | ("POST", "/images")
    );
    if requires_auth {
        if let Err(response) = authorize_request_headers(&partial.headers, config) {
            return write_response(&mut stream, response);
        }
    }

    // Phase 3: Read the body now that authentication has passed.
    let request = match read_request_body(&mut stream, partial) {
        Ok(request) => request,
        Err(response) => return write_response(&mut stream, response),
    };
    let route = classify_route(&request);
    let response = route_request(request, config);
    record_http_metrics(route, response.status);

    write_response(&mut stream, response)
}

fn route_request(request: HttpRequest, config: &ServerConfig) -> HttpResponse {
    let method = request.method.clone();
    let path = request.path().to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/health") => handle_health(config),
        ("GET", "/health/live") => handle_health_live(),
        ("GET", "/health/ready") => handle_health_ready(config),
        ("GET", "/images/by-path") => handle_public_path_request(request, config),
        ("GET", "/images/by-url") => handle_public_url_request(request, config),
        ("POST", "/images:transform") => handle_transform_request(request, config),
        ("POST", "/images") => handle_upload_request(request, config),
        ("GET", "/metrics") => handle_metrics_request(request, config),
        _ => HttpResponse::problem("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec()),
    }
}

fn classify_route(request: &HttpRequest) -> RouteMetric {
    match (request.method.as_str(), request.path()) {
        ("GET", "/health") => RouteMetric::Health,
        ("GET", "/health/live") => RouteMetric::HealthLive,
        ("GET", "/health/ready") => RouteMetric::HealthReady,
        ("GET", "/images/by-path") => RouteMetric::PublicByPath,
        ("GET", "/images/by-url") => RouteMetric::PublicByUrl,
        ("POST", "/images:transform") => RouteMetric::Transform,
        ("POST", "/images") => RouteMetric::Upload,
        ("GET", "/metrics") => RouteMetric::Metrics,
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

    // Try a version-based cache lookup before reading the source, avoiding I/O on hit.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicSourceKind {
    Path,
    Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageResponsePolicy {
    PublicGet,
    PrivateTransform,
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

    // Try a version-based cache lookup before reading the source, avoiding I/O on hit.
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
///
/// This endpoint is intended for Kubernetes liveness probes and should never
/// perform I/O or check external dependencies.
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
///
/// Returns 503 Service Unavailable when any check fails, so that load balancers
/// and orchestrators stop sending traffic to this instance.
fn handle_health_ready(config: &ServerConfig) -> HttpResponse {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut all_ok = true;

    // Check storage_root is accessible.
    let storage_ok = config.storage_root.is_dir();
    checks.push(json!({
        "name": "storageRoot",
        "status": if storage_ok { "ok" } else { "fail" },
    }));
    if !storage_ok {
        all_ok = false;
    }

    // Check cache_root is accessible when configured.
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

    // Check that the transform pool is not saturated.
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

/// Returns a comprehensive diagnostic health response with version information,
/// uptime, and detailed readiness checks.
///
/// This endpoint is designed for human operators and monitoring dashboards that
/// want a single view of the server's state.
fn handle_health(config: &ServerConfig) -> HttpResponse {
    let mut checks: Vec<serde_json::Value> = Vec::new();
    let mut all_ok = true;

    let storage_ok = config.storage_root.is_dir();
    checks.push(json!({
        "name": "storageRoot",
        "status": if storage_ok { "ok" } else { "fail" },
    }));
    if !storage_ok {
        all_ok = false;
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
        deadline: defaults.deadline,
    };

    Ok((source, options))
}

/// Default wall-clock deadline for server-side transforms.
///
/// The server injects this deadline into every transform request to prevent individual
/// requests from consuming unbounded wall-clock time. Library and CLI consumers are not subject
/// to this limit by default.
const SERVER_TRANSFORM_DEADLINE: Duration = Duration::from_secs(30);

fn transform_source_bytes(
    source_bytes: Vec<u8>,
    options: TransformOptions,
    versioned_hash: Option<&str>,
    request: &HttpRequest,
    response_policy: ImageResponsePolicy,
    config: &ServerConfig,
) -> HttpResponse {
    // When a version-based source hash was pre-computed by the caller, use it so that
    // cache writes go to the same key that the versioned pre-lookup checked. Otherwise
    // fall back to a SHA-256 of the content bytes.
    let content_hash;
    let source_hash = match versioned_hash {
        Some(hash) => hash,
        None => {
            content_hash = hex::encode(Sha256::digest(&source_bytes));
            &content_hash
        }
    };

    // Try the transform cache before acquiring a backpressure slot.
    let cache = config
        .cache_root
        .as_ref()
        .map(|root| TransformCache::new(root.clone()).with_log_handler(config.log_handler.clone()));

    if let Some(ref cache) = cache {
        // We need to sniff + negotiate to compute the full cache key, but we can
        // do a quick pre-check with just the options if format is already set.
        if options.format.is_some() {
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
    }

    // Backpressure: reject when too many transforms are already in flight.
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
) -> HttpResponse {
    if options.deadline.is_none() {
        options.deadline = Some(SERVER_TRANSFORM_DEADLINE);
    }
    let artifact = match sniff_artifact(RawArtifact::new(source_bytes, None)) {
        Ok(artifact) => artifact,
        Err(error) => return transform_error_response(error),
    };
    let negotiation_used = if options.format.is_none() {
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

    // Check the cache now that Accept negotiation is complete.
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

    // Store the result in the transform cache.
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
    );

    if matches!(response_policy, ImageResponsePolicy::PublicGet)
        && if_none_match_matches(request.header("if-none-match"), &etag)
    {
        return HttpResponse::empty("304 Not Modified", headers);
    }

    HttpResponse::binary_with_headers("200 OK", output.media_type.as_mime(), headers, output.bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcceptRange {
    Exact(&'static str),
    TypeWildcard(&'static str),
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcceptPreference {
    range: AcceptRange,
    q_millis: u16,
    specificity: u8,
}

fn negotiate_output_format(
    accept_header: Option<&str>,
    artifact: &Artifact,
) -> Result<Option<MediaType>, HttpResponse> {
    let Some(accept_header) = accept_header
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let (preferences, had_any_segment) = parse_accept_header(accept_header);
    if preferences.is_empty() {
        // The header contained segments but none were recognized image types
        // (e.g. Accept: application/json). This is an explicit mismatch.
        if had_any_segment {
            return Err(not_acceptable_response(
                "Accept does not allow any supported output media type",
            ));
        }
        return Ok(None);
    }

    let mut best_candidate = None;
    let mut best_q = 0_u16;

    for candidate in preferred_output_media_types(artifact) {
        let (candidate_q, _) = match_accept_preferences(candidate, &preferences);
        if candidate_q > best_q {
            best_q = candidate_q;
            best_candidate = Some(candidate);
        }
    }

    if best_q == 0 {
        return Err(not_acceptable_response(
            "Accept does not allow any supported output media type",
        ));
    }

    Ok(best_candidate)
}

/// Parses Accept header segments. Returns `(recognized, had_any_segments)` where
/// `had_any_segments` is true if the header contained at least one parseable media range
/// (even if not recognized by this server). This distinction lets the caller differentiate
/// "empty/malformed header" from "explicit but unsupported types".
fn parse_accept_header(value: &str) -> (Vec<AcceptPreference>, bool) {
    let mut preferences = Vec::new();
    let mut had_any_segment = false;
    for segment in value.split(',') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        had_any_segment = true;
        if let Some(pref) = parse_accept_segment(segment) {
            preferences.push(pref);
        }
    }
    (preferences, had_any_segment)
}

fn parse_accept_segment(segment: &str) -> Option<AcceptPreference> {
    if segment.is_empty() {
        return None;
    }

    let mut parts = segment.split(';');
    let media_range = parts.next()?.trim().to_ascii_lowercase();
    let (range, specificity) = parse_accept_range(&media_range)?;
    let mut q_millis = 1000_u16;

    for parameter in parts {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("q") {
            q_millis = parse_accept_qvalue(value.trim())?;
        }
    }

    Some(AcceptPreference {
        range,
        q_millis,
        specificity,
    })
}

fn parse_accept_range(value: &str) -> Option<(AcceptRange, u8)> {
    match value {
        "*/*" => Some((AcceptRange::Any, 0)),
        "image/*" => Some((AcceptRange::TypeWildcard("image"), 1)),
        "image/jpeg" => Some((AcceptRange::Exact("image/jpeg"), 2)),
        "image/png" => Some((AcceptRange::Exact("image/png"), 2)),
        "image/webp" => Some((AcceptRange::Exact("image/webp"), 2)),
        "image/avif" => Some((AcceptRange::Exact("image/avif"), 2)),
        "image/bmp" => Some((AcceptRange::Exact("image/bmp"), 2)),
        "image/svg+xml" => Some((AcceptRange::Exact("image/svg+xml"), 2)),
        _ => None,
    }
}

fn parse_accept_qvalue(value: &str) -> Option<u16> {
    let parsed = value.parse::<f32>().ok()?;
    if !(0.0..=1.0).contains(&parsed) {
        return None;
    }

    Some((parsed * 1000.0).round() as u16)
}

/// Returns the list of candidate output media types for Accept negotiation,
/// ordered by server preference.
///
/// The input format is always included so that "preserve the input format"
/// is a valid negotiation outcome (matching the OpenAPI spec). SVG is included
/// when the input is SVG.
fn preferred_output_media_types(artifact: &Artifact) -> Vec<MediaType> {
    let base: &[MediaType] = if artifact.metadata.has_alpha == Some(true) {
        &[
            MediaType::Avif,
            MediaType::Webp,
            MediaType::Png,
            MediaType::Jpeg,
        ]
    } else {
        &[
            MediaType::Avif,
            MediaType::Webp,
            MediaType::Jpeg,
            MediaType::Png,
        ]
    };

    let input = artifact.media_type;
    if base.contains(&input) {
        base.to_vec()
    } else {
        // Input format (e.g. SVG, BMP) is not in the base list — prepend it
        // so the client can request the original format via Accept negotiation.
        let mut candidates = vec![input];
        candidates.extend_from_slice(base);
        candidates
    }
}

fn match_accept_preferences(media_type: MediaType, preferences: &[AcceptPreference]) -> (u16, u8) {
    let mut best_q = 0_u16;
    let mut best_specificity = 0_u8;

    for preference in preferences {
        if accept_range_matches(preference.range, media_type)
            && (preference.q_millis > best_q
                || (preference.q_millis == best_q && preference.specificity > best_specificity))
        {
            best_q = preference.q_millis;
            best_specificity = preference.specificity;
        }
    }

    (best_q, best_specificity)
}

fn accept_range_matches(range: AcceptRange, media_type: MediaType) -> bool {
    match range {
        AcceptRange::Exact(expected) => media_type.as_mime() == expected,
        AcceptRange::TypeWildcard(expected_type) => media_type
            .as_mime()
            .split('/')
            .next()
            .is_some_and(|actual_type| actual_type == expected_type),
        AcceptRange::Any => true,
    }
}

fn build_image_etag(body: &[u8]) -> String {
    let digest = Sha256::digest(body);
    format!("\"sha256-{}\"", hex::encode(digest))
}

/// Whether a transform response was served from the cache or freshly computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheHitStatus {
    /// The response was served from the on-disk transform cache.
    Hit,
    /// The transform was freshly computed (no cache entry or stale).
    Miss,
    /// No cache is configured.
    Disabled,
}

fn build_image_response_headers(
    media_type: MediaType,
    etag: &str,
    response_policy: ImageResponsePolicy,
    negotiation_used: bool,
    cache_status: CacheHitStatus,
) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "Cache-Control".to_string(),
            match response_policy {
                ImageResponsePolicy::PublicGet => format!(
                    "public, max-age={DEFAULT_PUBLIC_MAX_AGE_SECONDS}, stale-while-revalidate={DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS}"
                ),
                ImageResponsePolicy::PrivateTransform => "no-store".to_string(),
            },
        ),
        ("ETag".to_string(), etag.to_string()),
        ("X-Content-Type-Options".to_string(), "nosniff".to_string()),
        (
            "Content-Disposition".to_string(),
            format!("inline; filename=\"truss.{}\"", media_type.as_name()),
        ),
    ];

    if negotiation_used {
        headers.push(("Vary".to_string(), "Accept".to_string()));
    }

    // SVG outputs get a Content-Security-Policy sandbox to prevent script execution
    // when served inline. This mitigates XSS risk from user-supplied SVG content.
    if media_type == MediaType::Svg {
        headers.push(("Content-Security-Policy".to_string(), "sandbox".to_string()));
    }

    // Cache-Status per RFC 9211.
    let cache_status_value = match cache_status {
        CacheHitStatus::Hit => "\"truss\"; hit".to_string(),
        CacheHitStatus::Miss | CacheHitStatus::Disabled => "\"truss\"; fwd=miss".to_string(),
    };
    headers.push(("Cache-Status".to_string(), cache_status_value));

    headers
}

fn if_none_match_matches(value: Option<&str>, etag: &str) -> bool {
    let Some(value) = value else {
        return false;
    };

    value
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate == etag)
}

fn parse_query_params(request: &HttpRequest) -> Result<BTreeMap<String, String>, HttpResponse> {
    let Some(query) = request.query() else {
        return Ok(BTreeMap::new());
    };

    let mut params = BTreeMap::new();
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let name = name.into_owned();
        let value = value.into_owned();
        if params.insert(name.clone(), value).is_some() {
            return Err(bad_request_response(&format!(
                "query parameter `{name}` must not be repeated"
            )));
        }
    }

    Ok(params)
}

fn authorize_signed_request(
    request: &HttpRequest,
    query: &BTreeMap<String, String>,
    config: &ServerConfig,
) -> Result<(), HttpResponse> {
    let expected_key_id = config
        .signed_url_key_id
        .as_deref()
        .ok_or_else(|| service_unavailable_response("public signed URL key is not configured"))?;
    let secret = config.signed_url_secret.as_deref().ok_or_else(|| {
        service_unavailable_response("public signed URL secret is not configured")
    })?;
    let key_id = required_auth_query_param(query, "keyId")?;
    let expires = required_auth_query_param(query, "expires")?;
    let signature = required_auth_query_param(query, "signature")?;

    if key_id != expected_key_id {
        return Err(signed_url_unauthorized_response(
            "signed URL is invalid or expired",
        ));
    }

    let expires = expires.parse::<u64>().map_err(|_| {
        bad_request_response("query parameter `expires` must be a positive integer")
    })?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| {
            internal_error_response(&format!("failed to read the current time: {error}"))
        })?
        .as_secs();
    if expires < now {
        return Err(signed_url_unauthorized_response(
            "signed URL is invalid or expired",
        ));
    }

    let authority = canonical_request_authority(request, config)?;
    let canonical_query = canonical_query_without_signature(query);
    let canonical = format!(
        "{}\n{}\n{}\n{}",
        request.method,
        authority,
        request.path(),
        canonical_query
    );

    let provided_signature = hex::decode(signature)
        .map_err(|_| signed_url_unauthorized_response("signed URL is invalid or expired"))?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|error| {
        internal_error_response(&format!(
            "failed to initialize signed URL verification: {error}"
        ))
    })?;
    mac.update(canonical.as_bytes());
    mac.verify_slice(&provided_signature)
        .map_err(|_| signed_url_unauthorized_response("signed URL is invalid or expired"))
}

fn canonical_request_authority(
    request: &HttpRequest,
    config: &ServerConfig,
) -> Result<String, HttpResponse> {
    if let Some(public_base_url) = &config.public_base_url {
        let parsed = Url::parse(public_base_url).map_err(|error| {
            internal_error_response(&format!(
                "configured public base URL is invalid at runtime: {error}"
            ))
        })?;
        return url_authority(&parsed).map_err(|message| internal_error_response(&message));
    }

    request
        .header("host")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| bad_request_response("public GET requests require a Host header"))
}

fn url_authority(url: &Url) -> Result<String, String> {
    let host = url
        .host_str()
        .ok_or_else(|| "configured public base URL must include a host".to_string())?;
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

fn canonical_query_without_signature(query: &BTreeMap<String, String>) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (name, value) in query {
        if name != "signature" {
            serializer.append_pair(name, value);
        }
    }
    serializer.finish()
}

fn signed_source_query(source: SignedUrlSource) -> BTreeMap<String, String> {
    let mut query = BTreeMap::new();
    match source {
        SignedUrlSource::Path { path, version } => {
            query.insert("path".to_string(), path);
            if let Some(version) = version {
                query.insert("version".to_string(), version);
            }
        }
        SignedUrlSource::Url { url, version } => {
            query.insert("url".to_string(), url);
            if let Some(version) = version {
                query.insert("version".to_string(), version);
            }
        }
    }
    query
}

fn extend_transform_query(query: &mut BTreeMap<String, String>, options: &TransformOptions) {
    if let Some(width) = options.width {
        query.insert("width".to_string(), width.to_string());
    }
    if let Some(height) = options.height {
        query.insert("height".to_string(), height.to_string());
    }
    if let Some(fit) = options.fit {
        query.insert("fit".to_string(), fit.as_name().to_string());
    }
    if let Some(position) = options.position {
        query.insert("position".to_string(), position.as_name().to_string());
    }
    if let Some(format) = options.format {
        query.insert("format".to_string(), format.as_name().to_string());
    }
    if let Some(quality) = options.quality {
        query.insert("quality".to_string(), quality.to_string());
    }
    if let Some(background) = options.background {
        query.insert("background".to_string(), encode_background(background));
    }
    if options.rotate != Rotation::Deg0 {
        query.insert(
            "rotate".to_string(),
            options.rotate.as_degrees().to_string(),
        );
    }
    if !options.auto_orient {
        query.insert("autoOrient".to_string(), "false".to_string());
    }
    if !options.strip_metadata {
        query.insert("stripMetadata".to_string(), "false".to_string());
    }
    if options.preserve_exif {
        query.insert("preserveExif".to_string(), "true".to_string());
    }
}

fn encode_background(color: Rgba8) -> String {
    if color.a == u8::MAX {
        format!("{:02X}{:02X}{:02X}", color.r, color.g, color.b)
    } else {
        format!(
            "{:02X}{:02X}{:02X}{:02X}",
            color.r, color.g, color.b, color.a
        )
    }
}

fn validate_public_query_names(
    query: &BTreeMap<String, String>,
    source_kind: PublicSourceKind,
) -> Result<(), HttpResponse> {
    for name in query.keys() {
        let allowed = matches!(
            name.as_str(),
            "keyId"
                | "expires"
                | "signature"
                | "version"
                | "width"
                | "height"
                | "fit"
                | "position"
                | "format"
                | "quality"
                | "background"
                | "rotate"
                | "autoOrient"
                | "stripMetadata"
                | "preserveExif"
        ) || matches!(
            (source_kind, name.as_str()),
            (PublicSourceKind::Path, "path") | (PublicSourceKind::Url, "url")
        );

        if !allowed {
            return Err(bad_request_response(&format!(
                "query parameter `{name}` is not supported for this endpoint"
            )));
        }
    }

    Ok(())
}

fn required_query_param<'a>(
    query: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, HttpResponse> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_request_response(&format!("query parameter `{name}` is required")))
}

fn required_auth_query_param<'a>(
    query: &'a BTreeMap<String, String>,
    name: &str,
) -> Result<&'a str, HttpResponse> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| signed_url_unauthorized_response("signed URL is invalid or expired"))
}

fn parse_optional_integer_query(
    query: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<u32>, HttpResponse> {
    match query.get(name) {
        Some(value) => value.parse::<u32>().map(Some).map_err(|_| {
            bad_request_response(&format!("query parameter `{name}` must be an integer"))
        }),
        None => Ok(None),
    }
}

fn parse_optional_u8_query(
    query: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<u8>, HttpResponse> {
    match query.get(name) {
        Some(value) => value.parse::<u8>().map(Some).map_err(|_| {
            bad_request_response(&format!("query parameter `{name}` must be an integer"))
        }),
        None => Ok(None),
    }
}

fn parse_optional_bool_query(
    query: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<bool>, HttpResponse> {
    match query.get(name).map(String::as_str) {
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(bad_request_response(&format!(
            "query parameter `{name}` must be `true` or `false`"
        ))),
        None => Ok(None),
    }
}

fn parse_upload_request(
    body: &[u8],
    boundary: &str,
) -> Result<(Vec<u8>, TransformOptions), HttpResponse> {
    let parts = parse_multipart_form_data(body, boundary)?;
    let mut file_range = None;
    let mut options = None;

    for part in &parts {
        match part.name.as_str() {
            "file" => {
                if file_range.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `file` fields",
                    ));
                }
                if part.body_range.is_empty() {
                    return Err(bad_request_response(
                        "multipart upload `file` field must not be empty",
                    ));
                }
                file_range = Some(part.body_range.clone());
            }
            "options" => {
                if options.is_some() {
                    return Err(bad_request_response(
                        "multipart upload must not include multiple `options` fields",
                    ));
                }
                let part_body = &body[part.body_range.clone()];
                if let Some(content_type) = part.content_type.as_deref()
                    && !content_type_matches(content_type, "application/json")
                {
                    return Err(bad_request_response(
                        "multipart upload `options` field must use application/json when a content type is provided",
                    ));
                }
                let payload = if part_body.is_empty() {
                    TransformOptionsPayload::default()
                } else {
                    serde_json::from_slice::<TransformOptionsPayload>(part_body).map_err(
                        |error| {
                            bad_request_response(&format!(
                                "multipart upload `options` field must contain valid JSON: {error}"
                            ))
                        },
                    )?
                };
                options = Some(payload.into_options()?);
            }
            field_name => {
                return Err(bad_request_response(&format!(
                    "multipart upload contains an unsupported field `{field_name}`"
                )));
            }
        }
    }

    let file_range = file_range
        .ok_or_else(|| bad_request_response("multipart upload requires a `file` field"))?;

    Ok((body[file_range].to_vec(), options.unwrap_or_default()))
}

fn record_http_metrics(route: RouteMetric, status: &str) {
    HTTP_REQUESTS_TOTAL.fetch_add(1, Ordering::Relaxed);
    route_counter(route).fetch_add(1, Ordering::Relaxed);
    status_counter(status).fetch_add(1, Ordering::Relaxed);
}

fn render_metrics_text() -> String {
    let mut body = String::new();
    body.push_str(
        "# HELP truss_process_up Whether the server adapter considers the process alive.\n",
    );
    body.push_str("# TYPE truss_process_up gauge\n");
    body.push_str("truss_process_up 1\n");

    body.push_str(
        "# HELP truss_transforms_in_flight Number of image transforms currently executing.\n",
    );
    body.push_str("# TYPE truss_transforms_in_flight gauge\n");
    body.push_str(&format!(
        "truss_transforms_in_flight {}\n",
        TRANSFORMS_IN_FLIGHT.load(Ordering::Relaxed)
    ));

    body.push_str(
        "# HELP truss_transforms_max_concurrent Maximum allowed concurrent transforms.\n",
    );
    body.push_str("# TYPE truss_transforms_max_concurrent gauge\n");
    body.push_str(&format!(
        "truss_transforms_max_concurrent {MAX_CONCURRENT_TRANSFORMS}\n"
    ));

    body.push_str("# HELP truss_http_requests_total Total parsed HTTP requests handled by the server adapter.\n");
    body.push_str("# TYPE truss_http_requests_total counter\n");
    body.push_str(&format!(
        "truss_http_requests_total {}\n",
        HTTP_REQUESTS_TOTAL.load(Ordering::Relaxed)
    ));

    body.push_str(
        "# HELP truss_http_requests_by_route_total Total parsed HTTP requests handled by route.\n",
    );
    body.push_str("# TYPE truss_http_requests_by_route_total counter\n");
    for route in [
        RouteMetric::Health,
        RouteMetric::HealthLive,
        RouteMetric::HealthReady,
        RouteMetric::PublicByPath,
        RouteMetric::PublicByUrl,
        RouteMetric::Transform,
        RouteMetric::Upload,
        RouteMetric::Metrics,
        RouteMetric::Unknown,
    ] {
        body.push_str(&format!(
            "truss_http_requests_by_route_total{{route=\"{}\"}} {}\n",
            route.as_label(),
            route_counter(route).load(Ordering::Relaxed)
        ));
    }

    body.push_str("# HELP truss_cache_hits_total Total transform cache hits.\n");
    body.push_str("# TYPE truss_cache_hits_total counter\n");
    body.push_str(&format!(
        "truss_cache_hits_total {}\n",
        CACHE_HITS_TOTAL.load(Ordering::Relaxed)
    ));

    body.push_str("# HELP truss_cache_misses_total Total transform cache misses.\n");
    body.push_str("# TYPE truss_cache_misses_total counter\n");
    body.push_str(&format!(
        "truss_cache_misses_total {}\n",
        CACHE_MISSES_TOTAL.load(Ordering::Relaxed)
    ));

    body.push_str("# HELP truss_origin_cache_hits_total Total origin response cache hits.\n");
    body.push_str("# TYPE truss_origin_cache_hits_total counter\n");
    body.push_str(&format!(
        "truss_origin_cache_hits_total {}\n",
        ORIGIN_CACHE_HITS_TOTAL.load(Ordering::Relaxed)
    ));

    body.push_str("# HELP truss_origin_cache_misses_total Total origin response cache misses.\n");
    body.push_str("# TYPE truss_origin_cache_misses_total counter\n");
    body.push_str(&format!(
        "truss_origin_cache_misses_total {}\n",
        ORIGIN_CACHE_MISSES_TOTAL.load(Ordering::Relaxed)
    ));

    body.push_str(
        "# HELP truss_http_responses_total Total HTTP responses emitted by status code.\n",
    );
    body.push_str("# TYPE truss_http_responses_total counter\n");
    for status in [
        "200", "400", "401", "403", "404", "406", "413", "415", "500", "501", "502", "503", "508",
        "other",
    ] {
        body.push_str(&format!(
            "truss_http_responses_total{{status=\"{status}\"}} {}\n",
            status_counter_value(status)
        ));
    }

    body
}

fn route_counter(route: RouteMetric) -> &'static AtomicU64 {
    match route {
        RouteMetric::Health => &HTTP_REQUESTS_HEALTH_TOTAL,
        RouteMetric::HealthLive => &HTTP_REQUESTS_HEALTH_LIVE_TOTAL,
        RouteMetric::HealthReady => &HTTP_REQUESTS_HEALTH_READY_TOTAL,
        RouteMetric::PublicByPath => &HTTP_REQUESTS_PUBLIC_BY_PATH_TOTAL,
        RouteMetric::PublicByUrl => &HTTP_REQUESTS_PUBLIC_BY_URL_TOTAL,
        RouteMetric::Transform => &HTTP_REQUESTS_TRANSFORM_TOTAL,
        RouteMetric::Upload => &HTTP_REQUESTS_UPLOAD_TOTAL,
        RouteMetric::Metrics => &HTTP_REQUESTS_METRICS_TOTAL,
        RouteMetric::Unknown => &HTTP_REQUESTS_UNKNOWN_TOTAL,
    }
}

fn status_counter(status: &str) -> &'static AtomicU64 {
    match status_code(status) {
        Some("200") => &HTTP_RESPONSES_200_TOTAL,
        Some("400") => &HTTP_RESPONSES_400_TOTAL,
        Some("401") => &HTTP_RESPONSES_401_TOTAL,
        Some("403") => &HTTP_RESPONSES_403_TOTAL,
        Some("404") => &HTTP_RESPONSES_404_TOTAL,
        Some("406") => &HTTP_RESPONSES_406_TOTAL,
        Some("413") => &HTTP_RESPONSES_413_TOTAL,
        Some("415") => &HTTP_RESPONSES_415_TOTAL,
        Some("500") => &HTTP_RESPONSES_500_TOTAL,
        Some("501") => &HTTP_RESPONSES_501_TOTAL,
        Some("502") => &HTTP_RESPONSES_502_TOTAL,
        Some("503") => &HTTP_RESPONSES_503_TOTAL,
        Some("508") => &HTTP_RESPONSES_508_TOTAL,
        _ => &HTTP_RESPONSES_OTHER_TOTAL,
    }
}

fn status_counter_value(status: &str) -> u64 {
    match status {
        "200" => HTTP_RESPONSES_200_TOTAL.load(Ordering::Relaxed),
        "400" => HTTP_RESPONSES_400_TOTAL.load(Ordering::Relaxed),
        "401" => HTTP_RESPONSES_401_TOTAL.load(Ordering::Relaxed),
        "403" => HTTP_RESPONSES_403_TOTAL.load(Ordering::Relaxed),
        "404" => HTTP_RESPONSES_404_TOTAL.load(Ordering::Relaxed),
        "406" => HTTP_RESPONSES_406_TOTAL.load(Ordering::Relaxed),
        "413" => HTTP_RESPONSES_413_TOTAL.load(Ordering::Relaxed),
        "415" => HTTP_RESPONSES_415_TOTAL.load(Ordering::Relaxed),
        "500" => HTTP_RESPONSES_500_TOTAL.load(Ordering::Relaxed),
        "501" => HTTP_RESPONSES_501_TOTAL.load(Ordering::Relaxed),
        "502" => HTTP_RESPONSES_502_TOTAL.load(Ordering::Relaxed),
        "503" => HTTP_RESPONSES_503_TOTAL.load(Ordering::Relaxed),
        "508" => HTTP_RESPONSES_508_TOTAL.load(Ordering::Relaxed),
        _ => HTTP_RESPONSES_OTHER_TOTAL.load(Ordering::Relaxed),
    }
}

fn status_code(status: &str) -> Option<&str> {
    status.split_whitespace().next()
}

fn authorize_request(request: &HttpRequest, config: &ServerConfig) -> Result<(), HttpResponse> {
    authorize_request_headers(&request.headers, config)
}

/// Authenticates a request using only the parsed header list. This is used by
/// `handle_stream` to reject unauthenticated requests *before* reading the
/// (potentially large) request body.
fn authorize_request_headers(
    headers: &[(String, String)],
    config: &ServerConfig,
) -> Result<(), HttpResponse> {
    let expected = config.bearer_token.as_deref().ok_or_else(|| {
        service_unavailable_response("private API bearer token is not configured")
    })?;
    let provided = headers
        .iter()
        .find_map(|(name, value)| (name == "authorization").then_some(value.as_str()))
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim);

    match provided {
        Some(token) if token == expected => Ok(()),
        _ => Err(auth_required_response("authorization required")),
    }
}

fn parse_multipart_boundary(request: &HttpRequest) -> Result<String, HttpResponse> {
    let Some(content_type) = request.header("content-type") else {
        return Err(unsupported_media_type_response(
            "content-type must be multipart/form-data",
        ));
    };

    let mut segments = content_type.split(';');
    let Some(media_type) = segments.next() else {
        return Err(unsupported_media_type_response(
            "content-type must be multipart/form-data",
        ));
    };
    if !content_type_matches(media_type, "multipart/form-data") {
        return Err(unsupported_media_type_response(
            "content-type must be multipart/form-data",
        ));
    }

    for segment in segments {
        let Some((name, value)) = segment.split_once('=') else {
            return Err(bad_request_response(
                "multipart content-type parameters must use name=value syntax",
            ));
        };
        if name.trim().eq_ignore_ascii_case("boundary") {
            let boundary = value.trim().trim_matches('"');
            if boundary.is_empty() {
                return Err(bad_request_response(
                    "multipart content-type boundary must not be empty",
                ));
            }
            return Ok(boundary.to_string());
        }
    }

    Err(bad_request_response(
        "multipart content-type requires a boundary parameter",
    ))
}

fn parse_multipart_form_data(
    body: &[u8],
    boundary: &str,
) -> Result<Vec<MultipartPart>, HttpResponse> {
    let opening = format!("--{boundary}").into_bytes();
    let delimiter = format!("\r\n--{boundary}").into_bytes();

    if !body.starts_with(&opening) {
        return Err(bad_request_response(
            "multipart body does not start with the declared boundary",
        ));
    }

    let mut cursor = 0;
    let mut parts = Vec::new();

    loop {
        if !body[cursor..].starts_with(&opening) {
            return Err(bad_request_response(
                "multipart boundary sequence is malformed",
            ));
        }
        cursor += opening.len();

        if body[cursor..].starts_with(b"--") {
            cursor += 2;
            if !body[cursor..].is_empty() && body[cursor..] != b"\r\n"[..] {
                return Err(bad_request_response(
                    "multipart closing boundary has unexpected trailing data",
                ));
            }
            break;
        }

        if !body[cursor..].starts_with(b"\r\n") {
            return Err(bad_request_response(
                "multipart boundary must be followed by CRLF",
            ));
        }
        cursor += 2;

        let header_end = find_subslice(&body[cursor..], b"\r\n\r\n")
            .ok_or_else(|| bad_request_response("multipart part is missing a header terminator"))?;
        let header_bytes = &body[cursor..(cursor + header_end)];
        let headers = parse_part_headers(header_bytes)?;
        cursor += header_end + 4;

        let body_end = find_subslice(&body[cursor..], &delimiter).ok_or_else(|| {
            bad_request_response("multipart part is missing the next boundary delimiter")
        })?;
        let body_range = cursor..(cursor + body_end);
        let part_name = parse_multipart_part_name(&headers)?;
        let content_type = header_value(&headers, "content-type").map(str::to_string);
        parts.push(MultipartPart {
            name: part_name,
            content_type,
            body_range,
        });

        cursor += body_end + 2;
    }

    Ok(parts)
}

fn parse_part_headers(header_bytes: &[u8]) -> Result<Vec<(String, String)>, HttpResponse> {
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| bad_request_response("multipart part headers must be valid UTF-8"))?;
    parse_headers(header_text.split("\r\n"))
}

fn parse_multipart_part_name(headers: &[(String, String)]) -> Result<String, HttpResponse> {
    let Some(disposition) = header_value(headers, "content-disposition") else {
        return Err(bad_request_response(
            "multipart part is missing a Content-Disposition header",
        ));
    };

    let mut segments = disposition.split(';');
    let Some(kind) = segments.next() else {
        return Err(bad_request_response(
            "multipart Content-Disposition header is malformed",
        ));
    };
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return Err(bad_request_response(
            "multipart Content-Disposition header must use form-data",
        ));
    }

    for segment in segments {
        let Some((name, value)) = segment.split_once('=') else {
            return Err(bad_request_response(
                "multipart Content-Disposition parameters must use name=value syntax",
            ));
        };
        if name.trim().eq_ignore_ascii_case("name") {
            let value = value.trim().trim_matches('"');
            if value.is_empty() {
                return Err(bad_request_response(
                    "multipart part name must not be empty",
                ));
            }
            return Ok(value.to_string());
        }
    }

    Err(bad_request_response(
        "multipart Content-Disposition header must include a name parameter",
    ))
}

fn resolve_source_bytes(
    source: TransformSourcePayload,
    config: &ServerConfig,
) -> Result<Vec<u8>, HttpResponse> {
    match source {
        TransformSourcePayload::Path { path, .. } => {
            let path = resolve_storage_path(&config.storage_root, &path)?;
            let metadata = fs::metadata(&path).map_err(map_source_io_error)?;
            if metadata.len() > MAX_SOURCE_BYTES {
                return Err(payload_too_large_response("source file is too large"));
            }

            fs::read(&path).map_err(map_source_io_error)
        }
        TransformSourcePayload::Url { url, .. } => read_remote_source_bytes(&url, config),
    }
}

fn read_remote_source_bytes(url: &str, config: &ServerConfig) -> Result<Vec<u8>, HttpResponse> {
    // Validate the URL against current security policy (scheme, port, IP range)
    // *before* checking the origin cache. This ensures that cached responses from
    // a permissive configuration cannot be served after tightening restrictions.
    let _ = prepare_remote_fetch_target(url, config)?;

    // Check the origin response cache before making an HTTP request.
    let origin_cache = config
        .cache_root
        .as_ref()
        .map(|root| OriginCache::new(root).with_log_handler(config.log_handler.clone()));

    if let Some(ref cache) = origin_cache
        && let Some(bytes) = cache.get(url)
    {
        ORIGIN_CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
        return Ok(bytes);
    }

    if origin_cache.is_some() {
        ORIGIN_CACHE_MISSES_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    let mut current_url = url.to_string();

    for redirect_index in 0..=MAX_REMOTE_REDIRECTS {
        let target = prepare_remote_fetch_target(&current_url, config)?;
        let agent = build_remote_agent(&target);

        match agent.get(target.url.as_str()).call() {
            Ok(response) => {
                let status = response.status().as_u16();
                if is_redirect_status(status) {
                    current_url = next_redirect_url(&target.url, &response, redirect_index)?;
                } else if status >= 400 {
                    return Err(bad_gateway_response(&format!(
                        "failed to fetch remote URL: upstream HTTP {status}"
                    )));
                } else {
                    let bytes = read_remote_response_body(target.url.as_str(), response)?;
                    if let Some(cache) = origin_cache {
                        cache.put(url, &bytes);
                    }
                    return Ok(bytes);
                }
            }
            Err(error) => {
                return Err(bad_gateway_response(&format!(
                    "failed to fetch remote URL: {error}"
                )));
            }
        }
    }

    Err(too_many_redirects_response(
        "remote URL exceeded the redirect limit",
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteFetchTarget {
    url: Url,
    netloc: String,
    addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PinnedResolver {
    expected_netloc: String,
    addrs: Vec<SocketAddr>,
}

impl ureq::unversioned::resolver::Resolver for PinnedResolver {
    fn resolve(
        &self,
        uri: &http::Uri,
        _config: &ureq::config::Config,
        _timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let authority = uri.authority().ok_or(ureq::Error::HostNotFound)?;
        let port = authority
            .port_u16()
            .or_else(|| match uri.scheme_str() {
                Some("https") => Some(443),
                Some("http") => Some(80),
                _ => None,
            })
            .ok_or(ureq::Error::HostNotFound)?;
        let requested_netloc = format!("{}:{}", authority.host(), port);
        if requested_netloc == self.expected_netloc {
            if self.addrs.is_empty() {
                return Err(ureq::Error::HostNotFound);
            }
            // ResolvedSocketAddrs is ArrayVec<SocketAddr, 16>. Push from our validated addrs,
            // capping at 16 (the ArrayVec capacity).
            let mut result = ureq::unversioned::resolver::ResolvedSocketAddrs::from_fn(|_| {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0)
            });
            for addr in self.addrs.iter().take(16) {
                result.push(*addr);
            }
            Ok(result)
        } else {
            Err(ureq::Error::HostNotFound)
        }
    }
}

fn prepare_remote_fetch_target(
    value: &str,
    config: &ServerConfig,
) -> Result<RemoteFetchTarget, HttpResponse> {
    let url = parse_remote_url(value)?;
    let addrs = validate_remote_url(&url, config)?;
    let host = url
        .host_str()
        .ok_or_else(|| bad_request_response("remote URL must include a host"))?
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| bad_request_response("remote URL must resolve to a known port"))?;

    Ok(RemoteFetchTarget {
        url,
        netloc: format!("{host}:{port}"),
        addrs,
    })
}

fn build_remote_agent(target: &RemoteFetchTarget) -> ureq::Agent {
    let config = ureq::config::Config::builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_body(Some(Duration::from_secs(30)))
        .proxy(None)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .build();

    // Pin the connection target to the validated resolution for this request so
    // the outbound fetch cannot race to a different DNS answer after validation.
    let resolver = PinnedResolver {
        expected_netloc: target.netloc.clone(),
        addrs: target.addrs.clone(),
    };

    ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::default(),
        resolver,
    )
}

fn next_redirect_url(
    current_url: &Url,
    response: &http::Response<ureq::Body>,
    redirect_index: usize,
) -> Result<String, HttpResponse> {
    if redirect_index == MAX_REMOTE_REDIRECTS {
        return Err(too_many_redirects_response(
            "remote URL exceeded the redirect limit",
        ));
    }

    let location = response
        .headers()
        .get("Location")
        .and_then(|v: &http::HeaderValue| v.to_str().ok());
    let Some(location) = location else {
        return Err(bad_gateway_response(
            "remote redirect response is missing a Location header",
        ));
    };
    let next_url = current_url.join(location).map_err(|error| {
        bad_gateway_response(&format!(
            "remote redirect location could not be resolved: {error}"
        ))
    })?;

    Ok(next_url.to_string())
}

fn parse_remote_url(value: &str) -> Result<Url, HttpResponse> {
    Url::parse(value)
        .map_err(|error| bad_request_response(&format!("remote URL is invalid: {error}")))
}

fn validate_remote_url(url: &Url, config: &ServerConfig) -> Result<Vec<SocketAddr>, HttpResponse> {
    match url.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(bad_request_response(
                "remote URL must use the http or https scheme",
            ));
        }
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad_request_response(
            "remote URL must not embed user information",
        ));
    }

    let Some(host) = url.host_str() else {
        return Err(bad_request_response("remote URL must include a host"));
    };
    let Some(port) = url.port_or_known_default() else {
        return Err(bad_request_response(
            "remote URL must resolve to a known port",
        ));
    };

    if !config.allow_insecure_url_sources && port != 80 && port != 443 {
        return Err(forbidden_response(
            "remote URL port is not allowed by the current server policy",
        ));
    }

    let addrs = url.socket_addrs(|| None).map_err(|error| {
        bad_gateway_response(&format!("failed to resolve remote host `{host}`: {error}"))
    })?;
    if addrs.is_empty() {
        return Err(bad_gateway_response(&format!(
            "failed to resolve remote host `{host}`"
        )));
    }

    if !config.allow_insecure_url_sources
        && addrs
            .iter()
            .map(|addr| addr.ip())
            .any(is_disallowed_remote_ip)
    {
        return Err(forbidden_response(
            "remote URL resolves to a disallowed IP range",
        ));
    }

    Ok(addrs)
}

fn read_remote_response_body(
    url: &str,
    response: http::Response<ureq::Body>,
) -> Result<Vec<u8>, HttpResponse> {
    validate_remote_content_encoding(&response)?;

    if response
        .headers()
        .get("Content-Length")
        .and_then(|v: &http::HeaderValue| v.to_str().ok())
        .and_then(|value: &str| value.parse::<u64>().ok())
        .is_some_and(|len| len > MAX_SOURCE_BYTES)
    {
        return Err(payload_too_large_response(&format!(
            "remote response exceeds {MAX_SOURCE_BYTES} bytes"
        )));
    }

    let mut reader = response
        .into_body()
        .into_reader()
        .take(MAX_SOURCE_BYTES + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|error| {
        bad_gateway_response(&format!("failed to read remote URL `{url}`: {error}"))
    })?;

    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(payload_too_large_response(&format!(
            "remote response exceeds {MAX_SOURCE_BYTES} bytes"
        )));
    }

    Ok(bytes)
}

fn validate_remote_content_encoding(
    response: &http::Response<ureq::Body>,
) -> Result<(), HttpResponse> {
    let Some(content_encoding) = response
        .headers()
        .get("Content-Encoding")
        .and_then(|v: &http::HeaderValue| v.to_str().ok())
    else {
        return Ok(());
    };

    for encoding in content_encoding
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !matches!(encoding, "gzip" | "br" | "identity") {
            return Err(bad_gateway_response(&format!(
                "remote response uses unsupported content-encoding `{encoding}`"
            )));
        }
    }

    Ok(())
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn is_disallowed_remote_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_disallowed_ipv4(ip),
        IpAddr::V6(ip) => is_disallowed_ipv6(ip),
    }
}

fn is_disallowed_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
        || (octets[0] == 198 && matches!(octets[1], 18 | 19))
        || (octets[0] & 0b1111_0000) == 240
}

fn is_disallowed_ipv6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
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

fn resolve_storage_path(storage_root: &Path, source_path: &str) -> Result<PathBuf, HttpResponse> {
    let trimmed = source_path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(bad_request_response("source path must not be empty"));
    }

    let mut relative_path = PathBuf::new();
    for component in Path::new(trimmed).components() {
        match component {
            Component::Normal(segment) => relative_path.push(segment),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(bad_request_response(
                    "source path must not contain root, current-directory, or parent-directory segments",
                ));
            }
        }
    }

    if relative_path.as_os_str().is_empty() {
        return Err(bad_request_response("source path must not be empty"));
    }

    let canonical_root = storage_root.canonicalize().map_err(|error| {
        internal_error_response(&format!("failed to resolve storage root: {error}"))
    })?;
    let candidate = storage_root.join(relative_path);
    let canonical_candidate = candidate.canonicalize().map_err(map_source_io_error)?;

    if !canonical_candidate.starts_with(&canonical_root) {
        return Err(bad_request_response("source path escapes the storage root"));
    }

    Ok(canonical_candidate)
}

/// Returns the maximum allowed body size for a request. Multipart uploads
/// (identified by `content-type: multipart/form-data`) are allowed up to
/// [`MAX_UPLOAD_BODY_BYTES`] because real-world photographs easily exceed the
/// 1 MiB default. All other requests keep the tighter [`MAX_REQUEST_BODY_BYTES`]
/// limit to bound JSON parsing and header-only endpoints.
fn max_body_for_headers(headers: &[(String, String)]) -> usize {
    let is_multipart = headers.iter().any(|(name, value)| {
        name == "content-type" && content_type_matches(value, "multipart/form-data")
    });
    if is_multipart {
        MAX_UPLOAD_BODY_BYTES
    } else {
        MAX_REQUEST_BODY_BYTES
    }
}

/// Reads only the HTTP request line and headers from `stream`, returning a
/// [`PartialHttpRequest`]. Any bytes already buffered beyond the header
/// terminator are stored in `overflow` so that `read_request_body` can
/// continue from the right position.
///
/// This split allows callers to inspect the method, path, and headers (e.g.
/// for authentication) *before* committing resources to read a potentially
/// large request body.
fn read_request_headers<R>(stream: &mut R) -> Result<PartialHttpRequest, HttpResponse>
where
    R: Read,
{
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).map_err(|error| {
            internal_error_response(&format!("failed to read request: {error}"))
        })?;
        if read == 0 {
            return Err(bad_request_response(
                "request ended before the HTTP headers were complete",
            ));
        }

        buffer.extend_from_slice(&chunk[..read]);

        // While searching for the header terminator we only enforce the
        // header-size limit.  Body-size limits are checked after headers are
        // parsed (in `read_request_body`).
        if buffer.len() > MAX_HEADER_BYTES + MAX_UPLOAD_BODY_BYTES {
            return Err(payload_too_large_response("request is too large"));
        }

        if let Some(index) = find_header_terminator(&buffer) {
            break index;
        }

        if buffer.len() > MAX_HEADER_BYTES {
            return Err(payload_too_large_response("request headers are too large"));
        }
    };

    let header_text = std::str::from_utf8(&buffer[..header_end])
        .map_err(|_| bad_request_response("request headers must be valid UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let (method, target, version) = parse_request_line(request_line)?;
    let headers = parse_headers(lines)?;
    let content_length = parse_content_length(&headers)?;
    let max_body = max_body_for_headers(&headers);
    if content_length > max_body {
        return Err(payload_too_large_response("request body is too large"));
    }

    let overflow = buffer[(header_end + 4)..].to_vec();

    Ok(PartialHttpRequest {
        method,
        target,
        version,
        headers,
        overflow,
        content_length,
    })
}

/// Reads the remaining body bytes for a [`PartialHttpRequest`] and assembles
/// the final [`HttpRequest`].
fn read_request_body<R>(
    stream: &mut R,
    partial: PartialHttpRequest,
) -> Result<HttpRequest, HttpResponse>
where
    R: Read,
{
    let mut body = partial.overflow;
    let mut chunk = [0_u8; 4096];
    while body.len() < partial.content_length {
        let read = stream.read(&mut chunk).map_err(|error| {
            internal_error_response(&format!("failed to read request: {error}"))
        })?;
        if read == 0 {
            return Err(bad_request_response("request body was truncated"));
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(partial.content_length);

    Ok(HttpRequest {
        method: partial.method,
        target: partial.target,
        version: partial.version,
        headers: partial.headers,
        body,
    })
}

fn parse_request_line(request_line: &str) -> Result<(String, String, String), HttpResponse> {
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| bad_request_response("request line is missing an HTTP method"))?;
    let target = parts
        .next()
        .ok_or_else(|| bad_request_response("request line is missing a target path"))?;
    let version = parts
        .next()
        .ok_or_else(|| bad_request_response("request line is missing an HTTP version"))?;

    if parts.next().is_some() {
        return Err(bad_request_response("request line has too many fields"));
    }

    Ok((method.to_string(), target.to_string(), version.to_string()))
}

/// Headers that must appear at most once per request. Duplicates of these create
/// interpretation differences between proxies and the origin, which is a security
/// concern when the server runs behind a reverse proxy.
const SINGLETON_HEADERS: &[&str] = &[
    "host",
    "authorization",
    "content-length",
    "content-type",
    "transfer-encoding",
];

fn parse_headers<'a, I>(lines: I) -> Result<Vec<(String, String)>, HttpResponse>
where
    I: Iterator<Item = &'a str>,
{
    let mut headers = Vec::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }

        let Some((name, value)) = line.split_once(':') else {
            return Err(bad_request_response(
                "request headers must use `name: value` syntax",
            ));
        };

        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    for &singleton in SINGLETON_HEADERS {
        let count = headers.iter().filter(|(name, _)| name == singleton).count();
        if count > 1 {
            return Err(bad_request_response(&format!(
                "duplicate `{singleton}` header is not allowed"
            )));
        }
    }

    Ok(headers)
}

fn parse_content_length(headers: &[(String, String)]) -> Result<usize, HttpResponse> {
    // Duplicate content-length is already rejected by the SINGLETON_HEADERS check
    // in parse_headers, so we only need to find the first (and only) value here.
    let Some(value) = headers
        .iter()
        .find_map(|(name, value)| (name == "content-length").then_some(value.as_str()))
    else {
        return Ok(0);
    };

    value
        .parse::<usize>()
        .map_err(|_| bad_request_response("content-length must be a non-negative integer"))
}

fn request_has_json_content_type(request: &HttpRequest) -> bool {
    request
        .header("content-type")
        .is_some_and(|value| content_type_matches(value, "application/json"))
}

fn transform_error_response(error: TransformError) -> HttpResponse {
    match error {
        TransformError::InvalidInput(reason)
        | TransformError::InvalidOptions(reason)
        | TransformError::DecodeFailed(reason) => bad_request_response(&reason),
        TransformError::UnsupportedInputMediaType(reason) => {
            unsupported_media_type_response(&reason)
        }
        TransformError::UnsupportedOutputMediaType(media_type) => unsupported_media_type_response(
            &format!("output format `{}` is not supported", media_type.as_name()),
        ),
        TransformError::EncodeFailed(reason) => {
            internal_error_response(&format!("failed to encode transformed artifact: {reason}"))
        }
        TransformError::CapabilityMissing(reason) => not_implemented_response(&reason),
        TransformError::LimitExceeded(reason) => payload_too_large_response(&reason),
    }
}

fn map_source_io_error(error: io::Error) -> HttpResponse {
    match error.kind() {
        io::ErrorKind::NotFound => not_found_response("source artifact was not found"),
        _ => internal_error_response(&format!("failed to access source artifact: {error}")),
    }
}

fn parse_optional_named<T, F>(
    value: Option<&str>,
    field_name: &str,
    parser: F,
) -> Result<Option<T>, HttpResponse>
where
    F: Fn(&str) -> Result<T, String>,
{
    match value {
        Some(value) => parse_named(value, field_name, parser).map(Some),
        None => Ok(None),
    }
}

fn parse_named<T, F>(value: &str, field_name: &str, parser: F) -> Result<T, HttpResponse>
where
    F: Fn(&str) -> Result<T, String>,
{
    parser(value).map_err(|reason| bad_request_response(&format!("{field_name}: {reason}")))
}

fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find_map(|(header_name, value)| (header_name == name).then_some(value.as_str()))
}

fn content_type_matches(value: &str, expected: &str) -> bool {
    value
        .split(';')
        .next()
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_header_terminator(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn write_response(stream: &mut TcpStream, response: HttpResponse) -> io::Result<()> {
    let mut header = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.body.len()
    );

    if let Some(content_type) = response.content_type {
        header.push_str(&format!("Content-Type: {content_type}\r\n"));
    }

    for (name, value) in response.headers {
        header.push_str(&format!("{name}: {value}\r\n"));
    }

    header.push_str("\r\n");

    stream.write_all(header.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn bad_request_response(message: &str) -> HttpResponse {
    problem_response("400 Bad Request", 400, "Bad Request", message)
}

fn auth_required_response(message: &str) -> HttpResponse {
    HttpResponse::problem_with_headers(
        "401 Unauthorized",
        vec![("WWW-Authenticate".to_string(), "Bearer".to_string())],
        problem_detail_body(401, "Unauthorized", message),
    )
}

fn signed_url_unauthorized_response(message: &str) -> HttpResponse {
    problem_response("401 Unauthorized", 401, "Unauthorized", message)
}

fn not_found_response(message: &str) -> HttpResponse {
    problem_response("404 Not Found", 404, "Not Found", message)
}

fn forbidden_response(message: &str) -> HttpResponse {
    problem_response("403 Forbidden", 403, "Forbidden", message)
}

fn unsupported_media_type_response(message: &str) -> HttpResponse {
    problem_response(
        "415 Unsupported Media Type",
        415,
        "Unsupported Media Type",
        message,
    )
}

fn not_acceptable_response(message: &str) -> HttpResponse {
    problem_response("406 Not Acceptable", 406, "Not Acceptable", message)
}

fn payload_too_large_response(message: &str) -> HttpResponse {
    problem_response("413 Payload Too Large", 413, "Payload Too Large", message)
}

fn internal_error_response(message: &str) -> HttpResponse {
    problem_response(
        "500 Internal Server Error",
        500,
        "Internal Server Error",
        message,
    )
}

fn bad_gateway_response(message: &str) -> HttpResponse {
    problem_response("502 Bad Gateway", 502, "Bad Gateway", message)
}

fn service_unavailable_response(message: &str) -> HttpResponse {
    problem_response(
        "503 Service Unavailable",
        503,
        "Service Unavailable",
        message,
    )
}

fn too_many_redirects_response(message: &str) -> HttpResponse {
    problem_response("508 Loop Detected", 508, "Loop Detected", message)
}

fn not_implemented_response(message: &str) -> HttpResponse {
    problem_response("501 Not Implemented", 501, "Not Implemented", message)
}

/// Builds an RFC 7807 Problem Details error response.
///
/// The response uses `application/problem+json` content type and includes
/// `type`, `title`, `status`, and `detail` fields as specified by RFC 7807.
/// The `type` field uses `about:blank` to indicate that the HTTP status code
/// itself is sufficient to describe the problem type.
fn problem_response(
    status: &'static str,
    status_code: u16,
    title: &str,
    detail: &str,
) -> HttpResponse {
    HttpResponse::problem(status, problem_detail_body(status_code, title, detail))
}

/// Serializes an RFC 7807 Problem Details JSON body.
fn problem_detail_body(status: u16, title: &str, detail: &str) -> Vec<u8> {
    let mut body = serde_json::to_vec(&json!({
        "type": "about:blank",
        "title": title,
        "status": status,
        "detail": detail,
    }))
    .expect("serialize problem detail body");
    body.push(b'\n');
    body
}

#[cfg(test)]
mod tests {
    use super::{
        CacheHitStatus, DEFAULT_BIND_ADDR, HttpRequest, ImageResponsePolicy,
        MAX_CONCURRENT_TRANSFORMS, PinnedResolver, PublicSourceKind, ServerConfig, SignedUrlSource,
        TRANSFORMS_IN_FLIGHT, TransformSourcePayload, auth_required_response,
        authorize_signed_request, bad_request_response, bind_addr, build_image_etag,
        build_image_response_headers, canonical_query_without_signature, find_header_terminator,
        negotiate_output_format, parse_public_get_request, prepare_remote_fetch_target,
        read_request_body, read_request_headers, resolve_storage_path, route_request,
        serve_once_with_config, sign_public_url,
        transform_source_bytes,
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
    fn read_request<R: Read>(stream: &mut R) -> Result<super::HttpRequest, super::HttpResponse> {
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

    fn response_body(response: &super::HttpResponse) -> String {
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
        // SAFETY: This test runs in a single-threaded context; no other thread
        // reads this environment variable concurrently.
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
        let query = super::parse_query_params(&request).expect("parse query");
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
        let query = super::parse_query_params(&request).expect("parse query");
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
        );

        assert!(!headers.iter().any(|(k, _)| k == "Content-Security-Policy"));
    }

    #[test]
    fn backpressure_rejects_when_at_capacity() {
        // Simulate being at capacity by loading the counter to MAX_CONCURRENT_TRANSFORMS.
        TRANSFORMS_IN_FLIGHT.store(MAX_CONCURRENT_TRANSFORMS, Ordering::Relaxed);

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

        // Must have been rejected with 503.
        assert!(response.status.contains("503"));

        // The counter should be back to what we set (not incremented).
        assert_eq!(
            TRANSFORMS_IN_FLIGHT.load(Ordering::Relaxed),
            MAX_CONCURRENT_TRANSFORMS
        );

        // Reset for other tests.
        TRANSFORMS_IN_FLIGHT.store(0, Ordering::Relaxed);
    }

    #[test]
    fn compute_cache_key_is_deterministic() {
        let opts = TransformOptions {
            width: Some(300),
            height: Some(200),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let key1 = super::compute_cache_key("source-abc", &opts, None);
        let key2 = super::compute_cache_key("source-abc", &opts, None);
        assert_eq!(key1, key2);
        assert_eq!(key1.len(), 64); // SHA-256 hex
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
        let key1 = super::compute_cache_key("same-source", &opts1, None);
        let key2 = super::compute_cache_key("same-source", &opts2, None);
        assert_ne!(key1, key2);
    }

    #[test]
    fn compute_cache_key_includes_accept_when_present() {
        let opts = TransformOptions::default();
        let key_no_accept = super::compute_cache_key("src", &opts, None);
        let key_with_accept = super::compute_cache_key("src", &opts, Some("image/webp"));
        assert_ne!(key_no_accept, key_with_accept);
    }

    #[test]
    fn transform_cache_put_and_get_round_trips() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::TransformCache::new(dir.path().to_path_buf());

        cache.put(
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890",
            MediaType::Png,
            b"png-data",
        );
        let result = cache.get("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890");

        match result {
            super::CacheLookup::Hit {
                media_type, body, ..
            } => {
                assert_eq!(media_type, MediaType::Png);
                assert_eq!(body, b"png-data");
            }
            super::CacheLookup::Miss => panic!("expected cache hit"),
        }
    }

    #[test]
    fn transform_cache_miss_for_unknown_key() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::TransformCache::new(dir.path().to_path_buf());

        let result = cache.get("0000001234567890abcdef1234567890abcdef1234567890abcdef1234567890");
        assert!(matches!(result, super::CacheLookup::Miss));
    }

    #[test]
    fn transform_cache_uses_sharded_layout() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::TransformCache::new(dir.path().to_path_buf());

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        cache.put(key, MediaType::Jpeg, b"jpeg-data");

        // Verify the sharded directory structure: ab/cd/ef/<key>
        let expected = dir.path().join("ab").join("cd").join("ef").join(key);
        assert!(
            expected.exists(),
            "sharded file should exist at {expected:?}"
        );
    }

    #[test]
    fn transform_cache_expired_entry_is_miss() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let mut cache = super::TransformCache::new(dir.path().to_path_buf());
        cache.ttl = Duration::from_secs(0); // Expire immediately.

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        cache.put(key, MediaType::Png, b"data");

        // Wait a moment so the mtime is in the past.
        std::thread::sleep(Duration::from_millis(10));

        let result = cache.get(key);
        assert!(matches!(result, super::CacheLookup::Miss));
    }

    #[test]
    fn transform_cache_handles_corrupted_entry_as_miss() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::TransformCache::new(dir.path().to_path_buf());

        let key = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let path = cache.entry_path(key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Write a file with no newline (no media type header).
        fs::write(&path, b"corrupted-data-without-header").unwrap();

        let result = cache.get(key);
        assert!(matches!(result, super::CacheLookup::Miss));
    }

    #[test]
    fn cache_status_header_reflects_hit() {
        let headers = build_image_response_headers(
            MediaType::Png,
            &build_image_etag(b"data"),
            ImageResponsePolicy::PublicGet,
            false,
            CacheHitStatus::Hit,
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
        );
        assert!(headers.contains(&(
            "Cache-Status".to_string(),
            "\"truss\"; fwd=miss".to_string()
        )));
    }

    #[test]
    fn origin_cache_put_and_get_round_trips() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::OriginCache::new(dir.path());

        cache.put("https://example.com/image.png", b"raw-source-bytes");
        let result = cache.get("https://example.com/image.png");

        assert_eq!(result.as_deref(), Some(b"raw-source-bytes".as_ref()));
    }

    #[test]
    fn origin_cache_miss_for_unknown_url() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::OriginCache::new(dir.path());

        assert!(
            cache
                .get("https://unknown.example.com/missing.png")
                .is_none()
        );
    }

    #[test]
    fn origin_cache_expired_entry_is_none() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let mut cache = super::OriginCache::new(dir.path());
        cache.ttl = Duration::from_secs(0);

        cache.put("https://example.com/img.png", b"data");
        std::thread::sleep(Duration::from_millis(10));

        assert!(cache.get("https://example.com/img.png").is_none());
    }

    #[test]
    fn origin_cache_uses_origin_subdirectory() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::OriginCache::new(dir.path());

        cache.put("https://example.com/test.png", b"bytes");

        // The origin cache root should be <dir>/origin/
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
        let boundary = super::parse_multipart_boundary(&request).expect("parse boundary");
        let (file_bytes, options) =
            super::parse_upload_request(&request.body, &boundary).expect("parse upload body");

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
        super::record_http_metrics(super::RouteMetric::Health, "200 OK");
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
            .write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\n\r\n")
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

    // --- Fix 2: Duplicate header rejection ---

    #[test]
    fn parse_headers_rejects_duplicate_host() {
        let lines = "Host: example.com\r\nHost: evil.com\r\n";
        let result = super::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_authorization() {
        let lines = "Authorization: Bearer a\r\nAuthorization: Bearer b\r\n";
        let result = super::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_content_type() {
        let lines = "Content-Type: application/json\r\nContent-Type: text/plain\r\n";
        let result = super::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_rejects_duplicate_transfer_encoding() {
        let lines = "Transfer-Encoding: chunked\r\nTransfer-Encoding: gzip\r\n";
        let result = super::parse_headers(lines.split("\r\n"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_headers_allows_single_instances_of_singleton_headers() {
        let lines =
            "Host: example.com\r\nAuthorization: Bearer tok\r\nContent-Type: application/json\r\n";
        let result = super::parse_headers(lines.split("\r\n"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 3);
    }

    // --- Fix 3: Upload body limit ---

    #[test]
    fn max_body_for_multipart_uses_upload_limit() {
        let headers = vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=abc".to_string(),
        )];
        assert_eq!(
            super::max_body_for_headers(&headers),
            super::MAX_UPLOAD_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_json_uses_default_limit() {
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        assert_eq!(
            super::max_body_for_headers(&headers),
            super::MAX_REQUEST_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_no_content_type_uses_default_limit() {
        let headers: Vec<(String, String)> = vec![];
        assert_eq!(
            super::max_body_for_headers(&headers),
            super::MAX_REQUEST_BODY_BYTES
        );
    }

    // --- Fix 4: Version-based cache key ---

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

    // --- Fix 5: Cache miss metric only when cache is enabled ---

    #[test]
    fn read_request_rejects_json_body_over_1mib() {
        let body = vec![b'x'; super::MAX_REQUEST_BODY_BYTES + 1];
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
        // Build a minimal multipart body that exceeds 1 MiB.
        let payload_size = super::MAX_REQUEST_BODY_BYTES + 100;
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
}
