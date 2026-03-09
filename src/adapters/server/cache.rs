use super::LogHandler;
use crate::MediaType;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::metrics::CACHE_HITS_TOTAL;
use super::negotiate::{
    CacheHitStatus, ImageResponsePolicy, build_image_etag, build_image_response_headers,
    if_none_match_matches,
};
use super::response::HttpResponse;
use super::http_parse::HttpRequest;
use super::ServerConfig;
use crate::{Fit, Position, Rotation, TransformOptions};

pub(super) const DEFAULT_CACHE_TTL_SECONDS: u64 = 3600;

/// Monotonically increasing counter used to generate unique temp-file suffixes
/// for cache writes.  Combined with the process ID this avoids collisions from
/// concurrent writers within the same process.
pub(super) static CACHE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

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
pub(super) struct TransformCache {
    pub(super) root: PathBuf,
    pub(super) ttl: Duration,
    pub(super) log_handler: Option<LogHandler>,
}

/// The result of a cache lookup.
#[derive(Debug)]
pub(super) enum CacheLookup {
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
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECONDS),
            log_handler: None,
        }
    }

    pub(super) fn with_log_handler(mut self, handler: Option<LogHandler>) -> Self {
        self.log_handler = handler;
        self
    }

    pub(super) fn log(&self, msg: &str) {
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
    pub(super) fn entry_path(&self, key: &str) -> PathBuf {
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
    pub(super) fn get(&self, key: &str) -> CacheLookup {
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
    pub(super) fn put(&self, key: &str, media_type: MediaType, body: &[u8]) {
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
/// cache in the cache hierarchy (design doc section 8.1).
///
/// The cache key is the SHA-256 of the canonical URL string. The stored value is the
/// raw source bytes with no header. Staleness uses the same mtime-based TTL as the
/// transform cache.
pub(super) struct OriginCache {
    root: PathBuf,
    pub(super) ttl: Duration,
    log_handler: Option<LogHandler>,
}

impl OriginCache {
    /// Creates a new origin cache rooted at `<cache_root>/origin/`.
    pub(super) fn new(cache_root: &Path) -> Self {
        Self {
            root: cache_root.join("origin"),
            ttl: Duration::from_secs(DEFAULT_CACHE_TTL_SECONDS),
            log_handler: None,
        }
    }

    pub(super) fn with_log_handler(mut self, handler: Option<LogHandler>) -> Self {
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
    pub(super) fn get(&self, url: &str) -> Option<Vec<u8>> {
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
    pub(super) fn put(&self, url: &str, body: &[u8]) {
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
pub(super) fn unique_tmp_suffix() -> String {
    let seq = CACHE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("tmp.{}.{seq}", std::process::id())
}

/// Computes a SHA-256 cache key from the source identifier, transform options, and
/// optionally the negotiated Accept value.
///
/// The canonical form follows the design specification (section 8.2):
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
pub(super) fn compute_cache_key(
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
    // (e.g. fit -> Contain, position -> Center when width+height are set), we
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
pub(super) fn try_versioned_cache_lookup(
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
            config.public_max_age_seconds,
            config.public_stale_while_revalidate_seconds,
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
