//! HTTP image-transform server.
//!
//! # Threading model
//!
//! The server uses **synchronous, blocking I/O** with one OS thread per TCP
//! connection. This is a deliberate design choice, not a limitation:
//!
//! - **Simplicity:** No async runtime (tokio/async-std) dependency for the core
//!   server. This reduces binary size, compile time, and cognitive overhead.
//! - **Predictable resource usage:** Each connection consumes a fixed stack
//!   allocation. There is no task queue, no hidden buffering, and no executor
//!   scheduling overhead.
//! - **Bounded concurrency:** `TRUSS_MAX_CONCURRENT_TRANSFORMS` (default 64)
//!   caps the number of simultaneous image transforms via a semaphore-like
//!   `TransformSlot` guard. Excess requests receive 503 Service Unavailable.
//!
//! **Trade-off:** Slow clients (slow uploads, slow TLS handshakes) block their
//! thread for the duration of the connection. In production deployments, a
//! reverse proxy (nginx, envoy, CloudFront) should handle slow-client buffering.
//!
//! This design may be reconsidered if the server needs to handle thousands of
//! concurrent idle connections (e.g., WebSocket or SSE), but for a
//! request-response image API the current model is sufficient.

mod auth;
#[cfg(feature = "azure")]
pub mod azure;
mod cache;
mod config;
#[cfg(feature = "gcs")]
pub mod gcs;
mod handler;
mod http_parse;
mod lifecycle;
mod metrics;
mod multipart;
mod negotiate;
mod rate_limit;
mod remote;
mod response;
mod routing;
#[cfg(feature = "s3")]
pub mod s3;
mod signing;

// --- Public re-exports (crate-level API surface) ---

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub use config::StorageBackend;
pub use config::{DEFAULT_BIND_ADDR, DEFAULT_STORAGE_ROOT, LogHandler, LogLevel, ServerConfig};
pub use handler::TransformOptionsPayload;
pub use lifecycle::{serve, serve_once, serve_once_with_config, serve_with_config};
pub use signing::{
    SignedUrlSource, SignedWatermarkParams, bind_addr, sign_public_url, sign_public_url_with_method,
};

/// Writes a line to stderr using a raw file-descriptor/handle write, bypassing
/// Rust's `std::io::Stderr` type whose internal `ReentrantLock` can interfere
/// with `MutexGuard` drop ordering in Rust 2024 edition, breaking HTTP
/// keep-alive.
pub(crate) fn stderr_write(msg: &str) {
    use std::io::Write;

    let bytes = msg.as_bytes();
    let mut buf = Vec::with_capacity(bytes.len() + 1);
    buf.extend_from_slice(bytes);
    buf.push(b'\n');

    #[cfg(unix)]
    {
        use std::os::fd::FromRawFd;
        // SAFETY: fd 2 (stderr) is always valid for the lifetime of the process.
        let mut f = unsafe { std::fs::File::from_raw_fd(2) };
        let _ = f.write_all(&buf);
        // Do not drop `f` — that would close fd 2 (stderr).
        std::mem::forget(f);
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::FromRawHandle;

        unsafe extern "system" {
            fn GetStdHandle(nStdHandle: u32) -> *mut std::ffi::c_void;
        }

        const STD_ERROR_HANDLE: u32 = (-12_i32) as u32;
        // SAFETY: GetStdHandle(STD_ERROR_HANDLE) returns the stderr handle
        // which is always valid for the lifetime of the process.
        let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
        let mut f = unsafe { std::fs::File::from_raw_handle(handle) };
        let _ = f.write_all(&buf);
        // Do not drop `f` — that would close the stderr handle.
        std::mem::forget(f);
    }
}

#[cfg(test)]
#[allow(unused_imports)] // Some imports are only used by feature-gated tests (e.g. s3).
mod tests {
    use serial_test::serial;

    use super::config::DEFAULT_MAX_CONCURRENT_TRANSFORMS;
    use super::config::{
        DEFAULT_PUBLIC_MAX_AGE_SECONDS, DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS,
        parse_presets_from_env,
    };
    use super::http_parse::{
        DEFAULT_MAX_UPLOAD_BODY_BYTES, HttpRequest, find_header_terminator, read_request_body,
        read_request_headers, resolve_storage_path,
    };
    use super::multipart::parse_multipart_form_data;
    use super::remote::{PinnedResolver, prepare_remote_fetch_target};
    use super::response::auth_required_response;
    use super::response::{HttpResponse, bad_request_response};
    // Items re-exported via `pub use` in mod.rs — accessible as `super::X`.
    use super::{
        DEFAULT_BIND_ADDR, ServerConfig, SignedUrlSource, TransformOptionsPayload, bind_addr,
        serve_once_with_config, sign_public_url,
    };
    // Items from submodules — imported via direct submodule paths.
    use super::auth::{
        authorize_request_headers, authorize_signed_request, canonical_query_without_signature,
    };
    use super::handler::{
        DEFAULT_HYSTERESIS_MARGIN, HealthCache, TransformImageRequestPayload, TransformSlot,
        TransformSourcePayload, WatermarkSource, disk_free_bytes, parse_public_get_request,
        process_rss_bytes, transform_source_bytes,
    };
    use super::lifecycle::preset_watcher;
    use super::negotiate::{
        CacheHitStatus, ImageResponsePolicy, PublicSourceKind, build_image_etag,
        build_image_response_headers, negotiate_output_format,
    };
    use super::routing::{
        AccessLogEntry, classify_route, emit_access_log, extract_cache_status, extract_request_id,
        extract_watermark_flag, resolve_client_ip, route_request,
    };
    use super::{config, metrics::RouteMetric};
    use crate::{
        Artifact, ArtifactMetadata, Fit, MediaType, OptimizeMode, RawArtifact, TransformOptions,
        sniff_artifact,
    };
    use hmac::{Hmac, Mac};
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    use sha2::Sha256;
    use std::collections::{BTreeMap, HashMap};
    use std::env;
    use std::fs;
    use std::io::{Cursor, Read, Write};
    use std::net::IpAddr;
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    /// Test-only convenience wrapper that reads headers + body in one shot,
    /// preserving the original `read_request` semantics for existing tests.
    fn read_request<R: Read>(stream: &mut R) -> Result<HttpRequest, HttpResponse> {
        let partial = read_request_headers(stream, DEFAULT_MAX_UPLOAD_BODY_BYTES)?;
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

    fn response_body(response: &HttpResponse) -> &str {
        std::str::from_utf8(&response.body).expect("utf8 response body")
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
        let format = negotiate_output_format(
            Some("image/jpeg,image/png"),
            &artifact_with_alpha(true),
            &[],
        )
        .expect("negotiate output format")
        .expect("resolved output format");

        assert_eq!(format, MediaType::Png);
    }

    #[test]
    fn negotiate_output_format_prefers_avif_for_wildcard_accept() {
        let format = negotiate_output_format(Some("image/*"), &artifact_with_alpha(false), &[])
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
            &[],
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
            &[],
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
            &[],
        );

        assert!(!headers.iter().any(|(k, _)| *k == "Content-Security-Policy"));
    }

    #[test]
    fn backpressure_rejects_when_at_capacity() {
        let config = ServerConfig::new(std::env::temp_dir(), None);
        config
            .transforms_in_flight
            .store(DEFAULT_MAX_CONCURRENT_TRANSFORMS, Ordering::Relaxed);

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

        let response = transform_source_bytes(
            png_bytes,
            TransformOptions::default(),
            None,
            &request,
            ImageResponsePolicy::PrivateTransform,
            &config,
            WatermarkSource::None,
            None,
            None,
        );

        assert!(response.status.contains("503"));

        assert_eq!(
            config.transforms_in_flight.load(Ordering::Relaxed),
            DEFAULT_MAX_CONCURRENT_TRANSFORMS
        );
    }

    #[test]
    fn backpressure_rejects_with_custom_concurrency_limit() {
        let custom_limit = 2u64;
        let mut config = ServerConfig::new(std::env::temp_dir(), None);
        config.max_concurrent_transforms = custom_limit;
        config
            .transforms_in_flight
            .store(custom_limit, Ordering::Relaxed);

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

        let response = transform_source_bytes(
            png_bytes,
            TransformOptions::default(),
            None,
            &request,
            ImageResponsePolicy::PrivateTransform,
            &config,
            WatermarkSource::None,
            None,
            None,
        );

        assert!(response.status.contains("503"));
    }

    #[test]
    fn compute_cache_key_is_deterministic() {
        let opts = TransformOptions {
            width: Some(300),
            height: Some(200),
            format: Some(MediaType::Webp),
            ..TransformOptions::default()
        };
        let key1 = super::cache::compute_cache_key("source-abc", &opts, None, None);
        let key2 = super::cache::compute_cache_key("source-abc", &opts, None, None);
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
        let key1 = super::cache::compute_cache_key("same-source", &opts1, None, None);
        let key2 = super::cache::compute_cache_key("same-source", &opts2, None, None);
        assert_ne!(key1, key2);
    }

    #[test]
    fn compute_cache_key_includes_accept_when_present() {
        let opts = TransformOptions::default();
        let key_no_accept = super::cache::compute_cache_key("src", &opts, None, None);
        let key_with_accept =
            super::cache::compute_cache_key("src", &opts, Some("image/webp"), None);
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
            &[],
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
            &[],
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

        cache.put("src", "https://example.com/image.png", b"raw-source-bytes");
        let result = cache.get("src", "https://example.com/image.png");

        assert_eq!(result.as_deref(), Some(b"raw-source-bytes".as_ref()));
    }

    #[test]
    fn origin_cache_miss_for_unknown_url() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        assert!(
            cache
                .get("src", "https://unknown.example.com/missing.png")
                .is_none()
        );
    }

    #[test]
    fn origin_cache_expired_entry_is_none() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let mut cache = super::cache::OriginCache::new(dir.path());
        cache.ttl = Duration::from_secs(0);

        cache.put("src", "https://example.com/img.png", b"data");
        std::thread::sleep(Duration::from_millis(10));

        assert!(cache.get("src", "https://example.com/img.png").is_none());
    }

    #[test]
    fn origin_cache_uses_origin_subdirectory() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let cache = super::cache::OriginCache::new(dir.path());

        cache.put("src", "https://example.com/test.png", b"bytes");

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
            None,
            None,
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

        let config = ServerConfig::new(temp_dir("parse-query"), None);
        let response = parse_public_get_request(&query, PublicSourceKind::Path, &config)
            .expect_err("unknown query should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("is not supported"));
    }

    #[test]
    fn parse_public_get_request_resolves_preset() {
        let mut presets = HashMap::new();
        presets.insert(
            "thumbnail".to_string(),
            TransformOptionsPayload {
                width: Some(150),
                height: Some(150),
                fit: Some("cover".to_string()),
                ..TransformOptionsPayload::default()
            },
        );
        let config = ServerConfig::new(temp_dir("preset"), None).with_presets(presets);

        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("preset".to_string(), "thumbnail".to_string()),
        ]);
        let (_, options, _) =
            parse_public_get_request(&query, PublicSourceKind::Path, &config).unwrap();

        assert_eq!(options.width, Some(150));
        assert_eq!(options.height, Some(150));
        assert_eq!(options.fit, Some(Fit::Cover));
    }

    #[test]
    fn parse_public_get_request_preset_with_override() {
        let mut presets = HashMap::new();
        presets.insert(
            "thumbnail".to_string(),
            TransformOptionsPayload {
                width: Some(150),
                height: Some(150),
                fit: Some("cover".to_string()),
                format: Some("webp".to_string()),
                ..TransformOptionsPayload::default()
            },
        );
        let config = ServerConfig::new(temp_dir("preset-override"), None).with_presets(presets);

        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("preset".to_string(), "thumbnail".to_string()),
            ("width".to_string(), "200".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]);
        let (_, options, _) =
            parse_public_get_request(&query, PublicSourceKind::Path, &config).unwrap();

        assert_eq!(options.width, Some(200));
        assert_eq!(options.height, Some(150));
        assert_eq!(options.format, Some(MediaType::Jpeg));
    }

    #[test]
    fn parse_public_get_request_accepts_optimize_fields() {
        let config = ServerConfig::new(temp_dir("optimize-query"), None);
        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("format".to_string(), "jpeg".to_string()),
            ("optimize".to_string(), "lossy".to_string()),
            ("targetQuality".to_string(), "ssim:0.98".to_string()),
        ]);

        let (_, options, _) =
            parse_public_get_request(&query, PublicSourceKind::Path, &config).unwrap();

        assert_eq!(options.format, Some(MediaType::Jpeg));
        assert_eq!(options.optimize, OptimizeMode::Lossy);
        assert_eq!(
            options
                .target_quality
                .expect("target quality should be parsed")
                .to_string(),
            "ssim:0.98"
        );
    }

    #[test]
    fn parse_public_get_request_rejects_unknown_preset() {
        let config = ServerConfig::new(temp_dir("preset-unknown"), None);

        let query = BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("preset".to_string(), "nonexistent".to_string()),
        ]);
        let response = parse_public_get_request(&query, PublicSourceKind::Path, &config)
            .expect_err("unknown preset should fail");

        assert_eq!(response.status, "400 Bad Request");
        assert!(response_body(&response).contains("unknown preset"));
    }

    #[test]
    fn sign_public_url_includes_preset_in_signed_url() {
        let url = sign_public_url(
            "https://cdn.example.com",
            SignedUrlSource::Path {
                path: "/image.png".to_string(),
                version: None,
            },
            &crate::TransformOptions::default(),
            "public-dev",
            "secret-value",
            4_102_444_800,
            None,
            Some("thumbnail"),
        )
        .expect("sign public URL with preset");

        assert!(url.contains("preset=thumbnail"));
        assert!(url.contains("signature="));
    }

    #[test]
    #[serial]
    fn parse_presets_from_env_parses_json() {
        unsafe {
            env::set_var(
                "TRUSS_PRESETS",
                r#"{"thumb":{"width":100,"height":100,"fit":"cover"}}"#,
            );
            env::remove_var("TRUSS_PRESETS_FILE");
        }
        let (presets, file_path) = parse_presets_from_env().unwrap();
        unsafe {
            env::remove_var("TRUSS_PRESETS");
        }

        assert!(file_path.is_none());
        assert_eq!(presets.len(), 1);
        let thumb = presets.get("thumb").unwrap();
        assert_eq!(thumb.width, Some(100));
        assert_eq!(thumb.height, Some(100));
        assert_eq!(thumb.fit.as_deref(), Some("cover"));
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
        assert_eq!(response.content_type, Some("application/problem+json"));
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
        assert_eq!(response.content_type, Some("image/jpeg"));

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
        assert_eq!(response.content_type, Some("image/jpeg"));

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
        assert_eq!(response.content_type, Some("image/jpeg"));

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
        let (file_bytes, options, _watermark) =
            super::multipart::parse_upload_request(&request.body, &boundary)
                .expect("parse upload body");

        assert_eq!(file_bytes, png_bytes());
        assert_eq!(options.width, Some(8));
        assert_eq!(options.format, Some(MediaType::Jpeg));
    }

    #[test]
    fn metrics_endpoint_does_not_require_authentication() {
        let response = route_request(
            metrics_request(false),
            &ServerConfig::new(temp_dir("metrics-no-auth"), Some("secret".to_string())),
        );

        assert_eq!(response.status, "200 OK");
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
            response.content_type,
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        assert!(body.contains("truss_http_requests_total"));
        assert!(body.contains("truss_http_requests_by_route_total{route=\"/health\"}"));
        assert!(body.contains("truss_http_responses_total{status=\"200\"}"));
        // Histogram metrics
        assert!(body.contains("# TYPE truss_http_request_duration_seconds histogram"));
        assert!(
            body.contains(
                "truss_http_request_duration_seconds_bucket{route=\"/health\",le=\"+Inf\"}"
            )
        );
        assert!(body.contains("# TYPE truss_transform_duration_seconds histogram"));
        assert!(body.contains("# TYPE truss_storage_request_duration_seconds histogram"));
        // Transform error counter
        assert!(body.contains("# TYPE truss_transform_errors_total counter"));
        assert!(body.contains("truss_transform_errors_total{error_type=\"decode_failed\"}"));
    }

    #[test]
    fn metrics_endpoint_returns_401_when_token_required() {
        let mut config = ServerConfig::new(temp_dir("metrics-auth"), None);
        config.metrics_token = Some("my-secret-token".to_string());

        // No auth header → 401
        let response = route_request(metrics_request(false), &config);
        assert_eq!(response.status, "401 Unauthorized");
    }

    #[test]
    fn metrics_endpoint_accepts_valid_token() {
        let mut config = ServerConfig::new(temp_dir("metrics-auth-ok"), None);
        config.metrics_token = Some("secret".to_string());

        // Bearer secret matches
        let response = route_request(metrics_request(true), &config);
        assert_eq!(response.status, "200 OK");
    }

    #[test]
    fn metrics_endpoint_rejects_wrong_token() {
        let mut config = ServerConfig::new(temp_dir("metrics-auth-bad"), None);
        config.metrics_token = Some("correct-token".to_string());

        // Bearer secret ≠ correct-token
        let response = route_request(metrics_request(true), &config);
        assert_eq!(response.status, "401 Unauthorized");
    }

    #[test]
    fn metrics_endpoint_returns_404_when_disabled() {
        let mut config = ServerConfig::new(temp_dir("metrics-disabled"), None);
        config.disable_metrics = true;

        let response = route_request(metrics_request(false), &config);
        assert_eq!(response.status, "404 Not Found");
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

        let server = thread::spawn(move || serve_once_with_config(listener, config));

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
            response.content_type,
            Some("application/problem+json"),
            "error responses must use application/problem+json"
        );
        assert_eq!(bad_request.content_type, Some("application/problem+json"),);

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
            super::http_parse::max_body_for_headers(
                &headers,
                super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
            ),
            super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_json_uses_default_limit() {
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        assert_eq!(
            super::http_parse::max_body_for_headers(
                &headers,
                super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
            ),
            super::http_parse::MAX_REQUEST_BODY_BYTES
        );
    }

    #[test]
    fn max_body_for_no_content_type_uses_default_limit() {
        let headers: Vec<(String, String)> = vec![];
        assert_eq!(
            super::http_parse::max_body_for_headers(
                &headers,
                super::http_parse::DEFAULT_MAX_UPLOAD_BODY_BYTES
            ),
            super::http_parse::MAX_REQUEST_BODY_BYTES
        );
    }

    fn make_test_config() -> ServerConfig {
        ServerConfig::new(std::env::temp_dir(), None)
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn storage_backend_parse_filesystem_aliases() {
        assert_eq!(
            super::StorageBackend::parse("filesystem").unwrap(),
            super::StorageBackend::Filesystem
        );
        assert_eq!(
            super::StorageBackend::parse("fs").unwrap(),
            super::StorageBackend::Filesystem
        );
        assert_eq!(
            super::StorageBackend::parse("local").unwrap(),
            super::StorageBackend::Filesystem
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_backend_parse_s3() {
        assert_eq!(
            super::StorageBackend::parse("s3").unwrap(),
            super::StorageBackend::S3
        );
        assert_eq!(
            super::StorageBackend::parse("S3").unwrap(),
            super::StorageBackend::S3
        );
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn storage_backend_parse_gcs() {
        assert_eq!(
            super::StorageBackend::parse("gcs").unwrap(),
            super::StorageBackend::Gcs
        );
        assert_eq!(
            super::StorageBackend::parse("GCS").unwrap(),
            super::StorageBackend::Gcs
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn storage_backend_parse_rejects_unknown() {
        assert!(super::StorageBackend::parse("").is_err());
        #[cfg(not(feature = "azure"))]
        assert!(super::StorageBackend::parse("azure").is_err());
        #[cfg(feature = "azure")]
        assert!(super::StorageBackend::parse("azure").is_ok());
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
        cfg_s3.storage_backend = super::StorageBackend::S3;

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
        cfg_a.storage_backend = super::StorageBackend::S3;
        cfg_a.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "shared",
            Some("http://minio-a:9000"),
        )));

        let mut cfg_b = make_test_config();
        cfg_b.storage_backend = super::StorageBackend::S3;
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
        assert_eq!(cfg.storage_backend, super::StorageBackend::Filesystem);
        assert!(cfg.s3_context.is_none());
    }

    #[test]
    #[cfg(feature = "s3")]
    fn storage_payload_deserializes_storage_variant() {
        let json = r#"{"source":{"kind":"storage","key":"photos/hero.jpg"},"options":{}}"#;
        let payload: TransformImageRequestPayload = serde_json::from_str(json).unwrap();
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
        let payload: TransformImageRequestPayload = serde_json::from_str(json).unwrap();
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
        cfg_a.storage_backend = super::StorageBackend::S3;
        cfg_a.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "bucket-a", None,
        )));

        let mut cfg_b = make_test_config();
        cfg_b.storage_backend = super::StorageBackend::S3;
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
        cfg.storage_backend = super::StorageBackend::S3;
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
    //
    // These tests mutate process-global environment variables. A mutex
    // serializes them so that parallel test threads cannot interfere, and
    // each test saves/restores the variables it touches.
    // -----------------------------------------------------------------------

    /// Mutex that serializes every test touching process-global env vars
    /// (`ServerConfig::from_env()` reads many `TRUSS_*` variables).
    /// All env-mutating tests must acquire this — `#[serial]` alone is not
    /// sufficient because it only serializes within its own group.
    static FROM_ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// All environment variables that any `from_env` test may read or write.
    /// Every variable listed here is saved before the test and restored after,
    /// preventing cross-test pollution.
    const ENV_VARS: &[&str] = &[
        "TRUSS_STORAGE_ROOT",
        "TRUSS_STORAGE_BACKEND",
        #[cfg(feature = "s3")]
        "TRUSS_S3_BUCKET",
        #[cfg(feature = "gcs")]
        "TRUSS_GCS_BUCKET",
        #[cfg(feature = "azure")]
        "TRUSS_AZURE_CONTAINER",
        "TRUSS_STORAGE_TIMEOUT_SECS",
        "TRUSS_MAX_CONCURRENT_TRANSFORMS",
    ];

    /// Save current values, run `f`, then restore originals regardless of
    /// panics. Holds `FROM_ENV_MUTEX` for the duration.
    fn with_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        let _guard = FROM_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<String>)> = ENV_VARS
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        // Apply requested overrides
        for &(key, value) in vars {
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        // Restore originals
        for (key, original) in saved {
            match original {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        if let Err(payload) = result {
            std::panic::resume_unwind(payload);
        }
    }

    /// Convenience alias for S3-specific tests.
    #[cfg(feature = "s3")]
    fn with_s3_env<F: FnOnce()>(vars: &[(&str, Option<&str>)], f: F) {
        with_env(vars, f);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_rejects_invalid_storage_backend() {
        let storage = temp_dir("env-bad-backend");
        let storage_str = storage.to_str().unwrap().to_string();
        with_s3_env(
            &[
                ("TRUSS_STORAGE_ROOT", Some(&storage_str)),
                ("TRUSS_STORAGE_BACKEND", Some("nosuchbackend")),
                ("TRUSS_S3_BUCKET", None),
            ],
            || {
                let result = ServerConfig::from_env();
                assert!(result.is_err());
                let msg = result.unwrap_err().to_string();
                assert!(msg.contains("unknown storage backend"), "got: {msg}");
            },
        );
        let _ = std::fs::remove_dir_all(storage);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_rejects_s3_without_bucket() {
        let storage = temp_dir("env-no-bucket");
        let storage_str = storage.to_str().unwrap().to_string();
        with_s3_env(
            &[
                ("TRUSS_STORAGE_ROOT", Some(&storage_str)),
                ("TRUSS_STORAGE_BACKEND", Some("s3")),
                ("TRUSS_S3_BUCKET", None),
            ],
            || {
                let result = ServerConfig::from_env();
                assert!(result.is_err());
                let msg = result.unwrap_err().to_string();
                assert!(msg.contains("TRUSS_S3_BUCKET"), "got: {msg}");
            },
        );
        let _ = std::fs::remove_dir_all(storage);
    }

    #[test]
    #[cfg(feature = "s3")]
    fn from_env_accepts_s3_with_bucket() {
        let storage = temp_dir("env-s3-ok");
        let storage_str = storage.to_str().unwrap().to_string();
        with_s3_env(
            &[
                ("TRUSS_STORAGE_ROOT", Some(&storage_str)),
                ("TRUSS_STORAGE_BACKEND", Some("s3")),
                ("TRUSS_S3_BUCKET", Some("my-images")),
            ],
            || {
                let cfg =
                    ServerConfig::from_env().expect("from_env should succeed with s3 + bucket");
                assert_eq!(cfg.storage_backend, super::StorageBackend::S3);
                let ctx = cfg.s3_context.expect("s3_context should be Some");
                assert_eq!(ctx.default_bucket, "my-images");
            },
        );
        let _ = std::fs::remove_dir_all(storage);
    }

    // -----------------------------------------------------------------------
    // S3: health endpoint
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "s3")]
    fn health_ready_s3_returns_503_when_context_missing() {
        let storage = temp_dir("health-s3-no-ctx");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::S3;
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
                .any(|c| c["name"] == "storageBackend" && c["status"] == "fail"),
            "expected s3Client fail check in {body}",
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn health_ready_s3_includes_s3_client_check() {
        let storage = temp_dir("health-s3-ok");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::S3;
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

        // The s3Client check will report "fail" because there is no real S3
        // endpoint, but the important thing is that the check is present.
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().any(|c| c["name"] == "storageBackend"),
            "expected s3Client check in {body}",
        );
    }

    // -----------------------------------------------------------------------
    // S3: public by-path remap (leading slash trimmed, Storage variant used)
    // -----------------------------------------------------------------------

    /// Replicates the Path→Storage remap that `handle_public_get_request`
    /// performs when `storage_backend == S3`, so we can inspect the resulting
    /// key without issuing a real S3 request.
    #[cfg(feature = "s3")]
    fn remap_path_to_storage(path: &str, version: Option<&str>) -> TransformSourcePayload {
        let source = TransformSourcePayload::Path {
            path: path.to_string(),
            version: version.map(|v| v.to_string()),
        };
        match source {
            TransformSourcePayload::Path { path, version } => TransformSourcePayload::Storage {
                bucket: None,
                key: path.trim_start_matches('/').to_string(),
                version,
            },
            other => other,
        }
    }

    #[test]
    #[cfg(feature = "s3")]
    fn public_by_path_s3_remap_trims_leading_slash() {
        // Paths with a leading slash (the common case from signed URLs like
        // `path=/image.png`) must have the slash stripped so that the S3 key
        // is `image.png`, not `/image.png`.
        let source = remap_path_to_storage("/photos/hero.jpg", Some("v1"));
        match &source {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg", "leading / must be trimmed");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        // Without a leading slash the key must be unchanged.
        let source2 = remap_path_to_storage("photos/hero.jpg", Some("v1"));
        match &source2 {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        // Both must produce the same versioned hash (same effective key).
        let mut cfg = make_test_config();
        cfg.storage_backend = super::StorageBackend::S3;
        cfg.s3_context = Some(std::sync::Arc::new(super::s3::S3Context::for_test(
            "my-bucket",
            None,
        )));
        assert_eq!(
            source.versioned_source_hash(&cfg),
            source2.versioned_source_hash(&cfg),
            "leading-slash and no-leading-slash paths must hash identically after trim",
        );
    }

    #[test]
    #[cfg(feature = "s3")]
    fn public_by_path_s3_remap_produces_storage_variant() {
        // Verify the remap converts Path to Storage with bucket: None.
        let source = remap_path_to_storage("/image.png", None);
        match source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert!(bucket.is_none(), "bucket must be None (use default)");
                assert_eq!(key, "image.png");
                assert!(version.is_none());
            }
            _ => panic!("expected Storage variant"),
        }
    }

    // -----------------------------------------------------------------------
    // GCS: health endpoint
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "gcs")]
    fn health_ready_gcs_returns_503_when_context_missing() {
        let storage = temp_dir("health-gcs-no-ctx");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Gcs;
        config.gcs_context = None;

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
                .any(|c| c["name"] == "storageBackend" && c["status"] == "fail"),
            "expected gcsClient fail check in {body}",
        );
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn health_ready_gcs_includes_gcs_client_check() {
        let storage = temp_dir("health-gcs-ok");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Gcs;
        config.gcs_context = Some(std::sync::Arc::new(super::gcs::GcsContext::for_test(
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

        // The gcsClient check will report "fail" because there is no real GCS
        // endpoint, but the important thing is that the check is present.
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().any(|c| c["name"] == "storageBackend"),
            "expected gcsClient check in {body}",
        );
    }

    // -----------------------------------------------------------------------
    // GCS: public by-path remap (leading slash trimmed, Storage variant used)
    // -----------------------------------------------------------------------

    #[cfg(feature = "gcs")]
    fn remap_path_to_storage_gcs(path: &str, version: Option<&str>) -> TransformSourcePayload {
        let source = TransformSourcePayload::Path {
            path: path.to_string(),
            version: version.map(|v| v.to_string()),
        };
        match source {
            TransformSourcePayload::Path { path, version } => TransformSourcePayload::Storage {
                bucket: None,
                key: path.trim_start_matches('/').to_string(),
                version,
            },
            other => other,
        }
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn public_by_path_gcs_remap_trims_leading_slash() {
        let source = remap_path_to_storage_gcs("/photos/hero.jpg", Some("v1"));
        match &source {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg", "leading / must be trimmed");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        let source2 = remap_path_to_storage_gcs("photos/hero.jpg", Some("v1"));
        match &source2 {
            TransformSourcePayload::Storage { key, .. } => {
                assert_eq!(key, "photos/hero.jpg");
            }
            _ => panic!("expected Storage variant after remap"),
        }

        let mut cfg = make_test_config();
        cfg.storage_backend = super::StorageBackend::Gcs;
        cfg.gcs_context = Some(std::sync::Arc::new(super::gcs::GcsContext::for_test(
            "my-bucket",
            None,
        )));
        assert_eq!(
            source.versioned_source_hash(&cfg),
            source2.versioned_source_hash(&cfg),
            "leading-slash and no-leading-slash paths must hash identically after trim",
        );
    }

    #[test]
    #[cfg(feature = "gcs")]
    fn public_by_path_gcs_remap_produces_storage_variant() {
        let source = remap_path_to_storage_gcs("/image.png", None);
        match source {
            TransformSourcePayload::Storage {
                bucket,
                key,
                version,
            } => {
                assert!(bucket.is_none(), "bucket must be None (use default)");
                assert_eq!(key, "image.png");
                assert!(version.is_none());
            }
            _ => panic!("expected Storage variant"),
        }
    }

    // -----------------------------------------------------------------------
    // Azure: health endpoint
    // -----------------------------------------------------------------------

    #[test]
    #[cfg(feature = "azure")]
    fn health_ready_azure_returns_503_when_context_missing() {
        let storage = temp_dir("health-azure-no-ctx");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Azure;
        config.azure_context = None;

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
                .any(|c| c["name"] == "storageBackend" && c["status"] == "fail"),
            "expected azureClient fail check in {body}",
        );
    }

    #[test]
    #[cfg(feature = "azure")]
    fn health_ready_azure_includes_azure_client_check() {
        let storage = temp_dir("health-azure-ok");
        let mut config = ServerConfig::new(storage.clone(), None);
        config.storage_backend = super::StorageBackend::Azure;
        config.azure_context = Some(std::sync::Arc::new(super::azure::AzureContext::for_test(
            "test-bucket",
            "http://localhost:10000/devstoreaccount1",
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

        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().any(|c| c["name"] == "storageBackend"),
            "expected azureClient check in {body}",
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

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_default() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", None),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.storage_timeout_secs, 30);
            },
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_custom() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", Some("60")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.storage_timeout_secs, 60);
            },
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_min_boundary() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", Some("1")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.storage_timeout_secs, 1);
            },
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_max_boundary() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", Some("300")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.storage_timeout_secs, 300);
            },
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_empty_string_uses_default() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", Some("")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.storage_timeout_secs, 30);
            },
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_zero_rejected() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", Some("0")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let err = ServerConfig::from_env().unwrap_err();
                assert!(
                    err.to_string().contains("between 1 and 300"),
                    "error should mention valid range: {err}"
                );
            },
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_over_max_rejected() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", Some("301")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let err = ServerConfig::from_env().unwrap_err();
                assert!(
                    err.to_string().contains("between 1 and 300"),
                    "error should mention valid range: {err}"
                );
            },
        );
    }

    #[test]
    #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
    fn test_storage_timeout_non_numeric_rejected() {
        with_env(
            &[
                ("TRUSS_STORAGE_TIMEOUT_SECS", Some("abc")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let err = ServerConfig::from_env().unwrap_err();
                assert!(
                    err.to_string().contains("positive integer"),
                    "error should mention positive integer: {err}"
                );
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_default() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", None),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.max_concurrent_transforms, 64);
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_custom() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", Some("128")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.max_concurrent_transforms, 128);
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_min_boundary() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", Some("1")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.max_concurrent_transforms, 1);
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_max_boundary() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", Some("1024")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.max_concurrent_transforms, 1024);
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_empty_uses_default() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", Some("")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let config = ServerConfig::from_env().unwrap();
                assert_eq!(config.max_concurrent_transforms, 64);
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_zero_rejected() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", Some("0")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let err = ServerConfig::from_env().unwrap_err();
                assert!(
                    err.to_string().contains("between 1 and 1024"),
                    "error should mention valid range: {err}"
                );
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_over_max_rejected() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", Some("1025")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let err = ServerConfig::from_env().unwrap_err();
                assert!(
                    err.to_string().contains("between 1 and 1024"),
                    "error should mention valid range: {err}"
                );
            },
        );
    }

    #[test]
    fn test_max_concurrent_transforms_non_numeric_rejected() {
        with_env(
            &[
                ("TRUSS_MAX_CONCURRENT_TRANSFORMS", Some("abc")),
                ("TRUSS_STORAGE_BACKEND", None),
            ],
            || {
                let err = ServerConfig::from_env().unwrap_err();
                assert!(
                    err.to_string().contains("positive integer"),
                    "error should mention positive integer: {err}"
                );
            },
        );
    }

    #[test]
    #[serial]
    fn test_transform_deadline_default() {
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 30);
    }

    #[test]
    #[serial]
    fn test_transform_deadline_custom() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "60");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 60);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_min_boundary() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "1");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 1);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_max_boundary() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "300");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 300);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_empty_uses_default() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.transform_deadline_secs, 30);
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 300"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_over_max_rejected() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "301");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("between 1 and 300"),
            "error should mention valid range: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    fn test_transform_deadline_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_TRANSFORM_DEADLINE_SECS", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("positive integer"),
            "error should mention positive integer: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_TRANSFORM_DEADLINE_SECS");
        }
    }

    #[test]
    #[serial]
    #[cfg(feature = "azure")]
    fn test_azure_container_env_var_required() {
        unsafe {
            std::env::set_var("TRUSS_STORAGE_BACKEND", "azure");
            std::env::remove_var("TRUSS_AZURE_CONTAINER");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(
            err.to_string().contains("TRUSS_AZURE_CONTAINER"),
            "error should mention TRUSS_AZURE_CONTAINER: {err}"
        );
        unsafe {
            std::env::remove_var("TRUSS_STORAGE_BACKEND");
        }
    }

    #[test]
    fn server_config_debug_redacts_bearer_token_and_signed_url_secret() {
        let mut config = ServerConfig::new(
            temp_dir("debug-redact"),
            Some("super-secret-token-12345".to_string()),
        );
        config.signed_url_key_id = Some("visible-key-id".to_string());
        config.signed_url_secret = Some("super-secret-hmac-key".to_string());
        let debug = format!("{config:?}");
        assert!(
            !debug.contains("super-secret-token-12345"),
            "bearer_token leaked in Debug output: {debug}"
        );
        assert!(
            !debug.contains("super-secret-hmac-key"),
            "signed_url_secret leaked in Debug output: {debug}"
        );
        assert!(
            debug.contains("[REDACTED]"),
            "expected [REDACTED] in Debug output: {debug}"
        );
        assert!(
            debug.contains("visible-key-id"),
            "signed_url_key_id should be visible: {debug}"
        );
    }

    #[test]
    fn authorize_headers_accepts_correct_bearer_token() {
        let config = ServerConfig::new(temp_dir("auth-ok"), Some("correct-token".to_string()));
        let headers = vec![(
            "authorization".to_string(),
            "Bearer correct-token".to_string(),
        )];
        assert!(authorize_request_headers(&headers, &config).is_ok());
    }

    #[test]
    fn authorize_headers_rejects_wrong_bearer_token() {
        let config = ServerConfig::new(temp_dir("auth-wrong"), Some("correct-token".to_string()));
        let headers = vec![(
            "authorization".to_string(),
            "Bearer wrong-token".to_string(),
        )];
        let err = authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    #[test]
    fn authorize_headers_rejects_missing_header() {
        let config = ServerConfig::new(temp_dir("auth-missing"), Some("correct-token".to_string()));
        let headers: Vec<(String, String)> = vec![];
        let err = authorize_request_headers(&headers, &config).unwrap_err();
        assert_eq!(err.status, "401 Unauthorized");
    }

    // ── TransformSlot RAII guard ──────────────────────────────────────

    #[test]
    fn transform_slot_acquire_succeeds_under_limit() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let slot = TransformSlot::try_acquire(&counter, 2);
        assert!(slot.is_some());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transform_slot_acquire_returns_none_at_limit() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let _s1 = TransformSlot::try_acquire(&counter, 1).unwrap();
        let s2 = TransformSlot::try_acquire(&counter, 1);
        assert!(s2.is_none());
        // Counter must still be 1 (failed acquire must not leak).
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transform_slot_drop_decrements_counter() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        {
            let _slot = TransformSlot::try_acquire(&counter, 4).unwrap();
            assert_eq!(counter.load(Ordering::Relaxed), 1);
        }
        // After drop the counter must return to zero.
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn transform_slot_multiple_acquires_up_to_limit() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let limit = 3u64;
        let mut slots = Vec::new();
        for _ in 0..limit {
            slots.push(TransformSlot::try_acquire(&counter, limit).unwrap());
        }
        assert_eq!(counter.load(Ordering::Relaxed), limit);
        // One more must fail.
        assert!(TransformSlot::try_acquire(&counter, limit).is_none());
        assert_eq!(counter.load(Ordering::Relaxed), limit);
        // Drop all slots.
        slots.clear();
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    // ── Access log via emit_access_log ────────────────────────────────

    #[test]
    fn emit_access_log_produces_json_with_expected_fields() {
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        let captured = Arc::new(Mutex::new(String::new()));
        let captured_clone = Arc::clone(&captured);
        let handler: super::LogHandler =
            Arc::new(move |msg: &str| *captured_clone.lock().unwrap() = msg.to_owned());

        let mut config = ServerConfig::new(temp_dir("access-log"), None);
        config.log_handler = Some(handler);

        let start = Instant::now();
        emit_access_log(
            &config,
            &AccessLogEntry {
                request_id: "req-123",
                method: "GET",
                path: "/image.png",
                route: "transform",
                status: "200",
                start,
                cache_status: Some("hit"),
                watermark: false,
            },
        );

        let output = captured.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        assert_eq!(parsed["kind"], "access_log");
        assert_eq!(parsed["request_id"], "req-123");
        assert_eq!(parsed["method"], "GET");
        assert_eq!(parsed["path"], "/image.png");
        assert_eq!(parsed["route"], "transform");
        assert_eq!(parsed["status"], "200");
        assert_eq!(parsed["cache_status"], "hit");
        assert!(parsed["latency_ms"].is_u64());
    }

    #[test]
    fn emit_access_log_null_cache_status_when_none() {
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        let captured = Arc::new(Mutex::new(String::new()));
        let captured_clone = Arc::clone(&captured);
        let handler: super::LogHandler =
            Arc::new(move |msg: &str| *captured_clone.lock().unwrap() = msg.to_owned());

        let mut config = ServerConfig::new(temp_dir("access-log-none"), None);
        config.log_handler = Some(handler);

        emit_access_log(
            &config,
            &AccessLogEntry {
                request_id: "req-456",
                method: "POST",
                path: "/upload",
                route: "upload",
                status: "201",
                start: Instant::now(),
                cache_status: None,
                watermark: false,
            },
        );

        let output = captured.lock().unwrap().clone();
        let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
        assert!(parsed["cache_status"].is_null());
    }

    // ── X-Request-Id header ───────────────────────────────────────────

    #[test]
    fn x_request_id_is_extracted_from_incoming_headers() {
        let headers = vec![
            ("host".to_string(), "localhost".to_string()),
            ("x-request-id".to_string(), "custom-id-abc".to_string()),
        ];
        assert_eq!(
            extract_request_id(&headers),
            Some("custom-id-abc".to_string())
        );
    }

    #[test]
    fn x_request_id_not_extracted_when_empty() {
        let headers = vec![("x-request-id".to_string(), "".to_string())];
        assert!(extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_not_extracted_when_absent() {
        let headers = vec![("host".to_string(), "localhost".to_string())];
        assert!(extract_request_id(&headers).is_none());
    }

    // ── Cache status extraction ───────────────────────────────────────

    #[test]
    fn cache_status_hit_detected() {
        let headers: Vec<(String, String)> =
            vec![("Cache-Status".to_string(), "\"truss\"; hit".to_string())];
        assert_eq!(extract_cache_status(&headers), Some("hit"));
    }

    #[test]
    fn cache_status_miss_detected() {
        let headers: Vec<(String, String)> = vec![(
            "Cache-Status".to_string(),
            "\"truss\"; fwd=miss".to_string(),
        )];
        assert_eq!(extract_cache_status(&headers), Some("miss"));
    }

    #[test]
    fn cache_status_none_when_header_absent() {
        let headers: Vec<(String, String)> =
            vec![("Content-Type".to_string(), "image/png".to_string())];
        assert!(extract_cache_status(&headers).is_none());
    }

    #[test]
    fn signing_keys_populated_by_with_signed_url_credentials() {
        let config = ServerConfig::new(temp_dir("signing-keys-populated"), None)
            .with_signed_url_credentials("key-alpha", "secret-alpha");

        assert_eq!(
            config.signing_keys.get("key-alpha").map(String::as_str),
            Some("secret-alpha")
        );
    }

    #[test]
    fn authorize_signed_request_accepts_multiple_keys() {
        let mut extra = HashMap::new();
        extra.insert("key-beta".to_string(), "secret-beta".to_string());
        let config = ServerConfig::new(temp_dir("multi-key-accept"), None)
            .with_signed_url_credentials("key-alpha", "secret-alpha")
            .with_signing_keys(extra);

        // Sign with key-alpha
        let request_alpha = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=key-alpha&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-alpha",
        );
        let query_alpha =
            super::auth::parse_query_params(&request_alpha).expect("parse query alpha");
        authorize_signed_request(&request_alpha, &query_alpha, &config)
            .expect("key-alpha should be accepted");

        // Sign with key-beta
        let request_beta = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=key-beta&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-beta",
        );
        let query_beta = super::auth::parse_query_params(&request_beta).expect("parse query beta");
        authorize_signed_request(&request_beta, &query_beta, &config)
            .expect("key-beta should be accepted");
    }

    #[test]
    fn authorize_signed_request_rejects_unknown_key() {
        let config = ServerConfig::new(temp_dir("unknown-key-reject"), None)
            .with_signed_url_credentials("key-alpha", "secret-alpha");

        let request = signed_public_request(
            "/images/by-path?path=%2Fimage.png&keyId=key-unknown&expires=4102444800&format=jpeg",
            "assets.example.com",
            "secret-unknown",
        );
        let query = super::auth::parse_query_params(&request).expect("parse query");
        authorize_signed_request(&request, &query, &config)
            .expect_err("unknown key should be rejected");
    }

    // ── Security: X-Request-Id CRLF injection prevention ─────────────

    #[test]
    fn x_request_id_rejects_crlf_injection() {
        let headers = vec![(
            "x-request-id".to_string(),
            "evil\r\nX-Injected: true".to_string(),
        )];
        assert!(
            extract_request_id(&headers).is_none(),
            "CRLF in request ID must be rejected"
        );
    }

    #[test]
    fn x_request_id_rejects_lone_cr() {
        let headers = vec![("x-request-id".to_string(), "evil\rid".to_string())];
        assert!(extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_rejects_lone_lf() {
        let headers = vec![("x-request-id".to_string(), "evil\nid".to_string())];
        assert!(extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_rejects_nul_byte() {
        let headers = vec![("x-request-id".to_string(), "evil\0id".to_string())];
        assert!(extract_request_id(&headers).is_none());
    }

    #[test]
    fn x_request_id_accepts_normal_uuid() {
        let headers = vec![(
            "x-request-id".to_string(),
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        )];
        assert_eq!(
            extract_request_id(&headers),
            Some("550e8400-e29b-41d4-a716-446655440000".to_string())
        );
    }

    // ── Characterization: ServerConfig defaults ──────────────────────

    #[test]
    fn server_config_new_has_expected_defaults() {
        let root = temp_dir("cfg-defaults");
        let config = ServerConfig::new(root.clone(), None);
        assert_eq!(config.storage_root, root);
        assert!(config.bearer_token.is_none());
        assert!(config.signed_url_secret.is_none());
        assert!(config.signing_keys.is_empty());
        assert!(config.presets.read().unwrap().is_empty());
        assert_eq!(
            config.max_concurrent_transforms,
            DEFAULT_MAX_CONCURRENT_TRANSFORMS
        );
        assert_eq!(
            config.public_max_age_seconds,
            DEFAULT_PUBLIC_MAX_AGE_SECONDS
        );
        assert_eq!(
            config.public_stale_while_revalidate_seconds,
            DEFAULT_PUBLIC_STALE_WHILE_REVALIDATE_SECONDS
        );
        assert!(!config.allow_insecure_url_sources);
    }

    #[test]
    fn server_config_builder_with_signed_url_credentials_overwrites() {
        let root = temp_dir("cfg-builder");
        let config = ServerConfig::new(root, None)
            .with_signed_url_credentials("key1", "secret1")
            .with_signed_url_credentials("key2", "secret2");
        assert!(config.signing_keys.contains_key("key1"));
        assert!(config.signing_keys.contains_key("key2"));
    }

    // ── Characterization: route_request classification ───────────────

    #[test]
    fn route_request_returns_not_found_for_unknown_path() {
        let root = temp_dir("route-unknown");
        let config = ServerConfig::new(root, None);
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/nonexistent".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("host".to_string(), "localhost".to_string())],
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        assert_eq!(response.status, "404 Not Found");
    }

    #[test]
    fn route_request_health_returns_200() {
        let root = temp_dir("route-health");
        let config = ServerConfig::new(root, None);
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![("host".to_string(), "localhost".to_string())],
            body: Vec::new(),
        };
        let response = route_request(request, &config);
        assert_eq!(response.status, "200 OK");
    }

    // ── Characterization: TransformSlot thread safety ────────────────

    #[test]
    fn transform_slot_concurrent_acquire_respects_limit() {
        use std::sync::Arc;
        use std::sync::Barrier;
        use std::sync::atomic::AtomicU64;

        let counter = Arc::new(AtomicU64::new(0));
        let limit = 4u64;
        let num_threads = 16;
        let barrier = Arc::new(Barrier::new(num_threads));
        let acquired = Arc::new(AtomicU64::new(0));

        let handles: Vec<_> = (0..num_threads)
            .map(|_| {
                let counter = Arc::clone(&counter);
                let barrier = Arc::clone(&barrier);
                let acquired = Arc::clone(&acquired);
                thread::spawn(move || {
                    barrier.wait();
                    if let Some(_slot) = TransformSlot::try_acquire(&counter, limit) {
                        acquired.fetch_add(1, Ordering::Relaxed);
                        thread::sleep(Duration::from_millis(10));
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_default() {
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 40_000_000);
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_custom() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "10000000");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 10_000_000);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_min_boundary() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "1");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 1);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_max_boundary() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "100000000");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 100_000_000);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_empty_uses_default() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_input_pixels, 40_000_000);
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_INPUT_PIXELS"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_over_max_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "100000001");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_INPUT_PIXELS"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_input_pixels_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_INPUT_PIXELS", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_INPUT_PIXELS"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_INPUT_PIXELS");
        }
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_default() {
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_upload_bytes, 100 * 1024 * 1024);
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_custom() {
        unsafe {
            std::env::set_var("TRUSS_MAX_UPLOAD_BYTES", "5242880");
        }
        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.max_upload_bytes, 5 * 1024 * 1024);
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_zero_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_UPLOAD_BYTES", "0");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_UPLOAD_BYTES"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
    }

    #[test]
    #[serial]
    fn test_max_upload_bytes_non_numeric_rejected() {
        unsafe {
            std::env::set_var("TRUSS_MAX_UPLOAD_BYTES", "abc");
        }
        let err = ServerConfig::from_env().unwrap_err();
        assert!(err.to_string().contains("TRUSS_MAX_UPLOAD_BYTES"));
        unsafe {
            std::env::remove_var("TRUSS_MAX_UPLOAD_BYTES");
        }
    }

    #[test]
    fn max_body_for_multipart_uses_custom_upload_limit() {
        let headers = vec![(
            "content-type".to_string(),
            "multipart/form-data; boundary=abc".to_string(),
        )];
        let custom_limit = 5 * 1024 * 1024;
        assert_eq!(
            super::http_parse::max_body_for_headers(&headers, custom_limit),
            custom_limit
        );
    }

    #[test]
    fn health_includes_max_input_pixels() {
        let storage = temp_dir("health-pixels");
        let request = HttpRequest {
            method: "GET".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: Vec::new(),
            body: Vec::new(),
        };

        let config = ServerConfig::new(storage, None);
        let response = route_request(request, &config);

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        assert_eq!(body["maxInputPixels"], 40_000_000);
    }

    #[test]
    fn health_includes_transform_capacity_details() {
        let storage = temp_dir("health-capacity");
        let config = ServerConfig::new(storage, None);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        let checks = body["checks"].as_array().expect("checks array");
        let capacity = checks
            .iter()
            .find(|c| c["name"] == "transformCapacity")
            .expect("transformCapacity check");
        assert_eq!(capacity["current"], 0);
        assert_eq!(capacity["max"], 64);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_rss_bytes_returns_some() {
        let rss = process_rss_bytes();
        assert!(rss.is_some());
        assert!(rss.unwrap() > 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_free_bytes_returns_some_for_existing_dir() {
        let dir = temp_dir("disk-free");
        let free = disk_free_bytes(&dir);
        assert!(free.is_some());
        assert!(free.unwrap() > 0);
    }

    #[test]
    fn health_ready_returns_503_when_memory_exceeded() {
        let storage = temp_dir("health-mem");
        let mut config = ServerConfig::new(storage, None);
        // Set threshold to 1 byte — guaranteed to be exceeded.
        config.health_max_memory_bytes = Some(1);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        // On Linux, RSS > 1 byte → 503. On other platforms, memory check
        // is skipped so the response is 200.
        if cfg!(target_os = "linux") {
            assert_eq!(response.status, "503 Service Unavailable");
        }
    }

    #[test]
    fn health_includes_memory_usage_on_linux() {
        let storage = temp_dir("health-mem-report");
        let mut config = ServerConfig::new(storage, None);
        config.health_max_memory_bytes = Some(u64::MAX);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse health body");
        if cfg!(target_os = "linux") {
            let checks = body["checks"].as_array().expect("checks array");
            let mem = checks
                .iter()
                .find(|c| c["name"] == "memoryUsage")
                .expect("memoryUsage check");
            assert_eq!(mem["status"], "ok");
            assert!(mem["rssBytes"].as_u64().unwrap() > 0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_free_bytes_returns_none_for_nonexistent_path() {
        let free = disk_free_bytes(std::path::Path::new("/nonexistent/path/xyz"));
        assert!(free.is_none());
    }

    #[test]
    fn health_ready_503_body_contains_fail_status() {
        let storage = temp_dir("health-ready-body");
        std::fs::remove_dir_all(&storage).ok();
        let config = ServerConfig::new(storage, None);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        assert_eq!(response.status, "503 Service Unavailable");
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        let storage_check = checks
            .iter()
            .find(|c| c["name"] == "storageRoot")
            .expect("storageRoot check");
        assert_eq!(storage_check["status"], "fail");
    }

    #[test]
    fn health_ready_cache_disk_free_shown_when_cache_root_set() {
        let storage = temp_dir("health-ready-cache-disk");
        let cache = temp_dir("health-ready-cache-disk-cache");
        let mut config = ServerConfig::new(storage, None).with_cache_root(cache);
        config.health_cache_min_free_bytes = Some(1);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        let disk_check = checks
            .iter()
            .find(|c| c["name"] == "cacheDiskFree")
            .expect("cacheDiskFree check");
        assert_eq!(disk_check["status"], "ok");
        if cfg!(target_os = "linux") {
            assert!(disk_check["freeBytes"].as_u64().is_some());
        }
        assert_eq!(disk_check["thresholdBytes"], 1);
    }

    #[test]
    fn health_ready_no_cache_disk_free_without_cache_root() {
        let storage = temp_dir("health-ready-no-cache");
        let config = ServerConfig::new(storage, None);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks.iter().all(|c| c["name"] != "cacheDiskFree"),
            "cacheDiskFree should not appear without cache_root"
        );
    }

    #[test]
    fn health_ready_memory_check_includes_details() {
        let storage = temp_dir("health-ready-mem-detail");
        let mut config = ServerConfig::new(storage, None);
        config.health_max_memory_bytes = Some(u64::MAX);
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );
        let body: serde_json::Value = serde_json::from_slice(&response.body).expect("parse body");
        let checks = body["checks"].as_array().expect("checks array");
        let mem = checks.iter().find(|c| c["name"] == "memoryUsage");
        if cfg!(target_os = "linux") {
            let mem = mem.expect("memoryUsage check present on Linux");
            assert_eq!(mem["status"], "ok");
            assert_eq!(mem["thresholdBytes"], u64::MAX);
            assert!(mem["rssBytes"].as_u64().is_some());
        } else {
            assert!(mem.is_none(), "memoryUsage should be absent on non-Linux");
        }
    }

    // ── graceful shutdown: draining flag ─────────────────────────────

    #[test]
    fn health_ready_returns_503_when_draining() {
        let storage = temp_dir("health-ready-draining");
        let config = ServerConfig::new(storage, None);
        config.draining.store(true, Ordering::Relaxed);

        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        assert_eq!(response.status, "503 Service Unavailable");
        assert_eq!(response.content_type, Some("application/json"));
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready body");
        assert_eq!(body["status"], "fail");
        let checks = body["checks"].as_array().expect("checks array");
        assert!(
            checks
                .iter()
                .any(|c| c["name"] == "draining" && c["status"] == "fail")
        );
    }

    #[test]
    fn health_ready_returns_ok_when_not_draining() {
        let storage = temp_dir("health-ready-not-draining");
        let config = ServerConfig::new(storage, None);
        // draining is false by default.
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/ready".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        assert_eq!(response.status, "200 OK");
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).expect("parse ready body");
        assert_eq!(body["status"], "ok");
        // Should not have a draining check entry.
        let checks = body["checks"].as_array().expect("checks array");
        assert!(!checks.iter().any(|c| c["name"] == "draining"));
    }

    // ── Drain during normal request processing (m10) ─────────────

    #[test]
    fn health_live_returns_200_while_draining() {
        let storage = temp_dir("live-draining");
        let config = ServerConfig::new(storage, None);
        config.draining.store(true, Ordering::Relaxed);

        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/health/live".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        // Liveness should always return 200 even when draining — only
        // readiness returns 503.
        assert_eq!(response.status, "200 OK");
    }

    #[test]
    fn normal_request_processed_while_draining() {
        let storage = temp_dir("normal-draining");
        let config = ServerConfig::new(storage, None);
        config.draining.store(true, Ordering::Relaxed);

        // A non-health, non-image request should still be routed (e.g. 404
        // because the path doesn't match any route) — it should NOT get a
        // 503 just because the server is draining.
        let response = route_request(
            HttpRequest {
                method: "GET".to_string(),
                target: "/nonexistent".to_string(),
                version: "HTTP/1.1".to_string(),
                headers: Vec::new(),
                body: Vec::new(),
            },
            &config,
        );

        // The path doesn't match any route, so we get 404 — NOT 503.
        assert_eq!(response.status, "404 Not Found");
    }

    // ── preset hot-reload watcher ────────────────────────────────────

    #[test]
    fn preset_watcher_reloads_on_file_change() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = std::env::temp_dir().join(format!(
            "truss_test_watcher_{}",
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("presets.json");
        std::fs::write(&path, r#"{"thumb":{"width":100}}"#).unwrap();

        let presets = Arc::new(std::sync::RwLock::new({
            let mut m = std::collections::HashMap::new();
            m.insert(
                "thumb".to_string(),
                TransformOptionsPayload {
                    width: Some(100),
                    height: None,
                    fit: None,
                    position: None,
                    format: None,
                    quality: None,
                    optimize: None,
                    target_quality: None,
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
            m
        }));
        let draining = Arc::new(AtomicBool::new(false));
        let config = Arc::new(ServerConfig::new(dir.clone(), None));

        let presets_clone = Arc::clone(&presets);
        let draining_clone = Arc::clone(&draining);
        let config_clone = Arc::clone(&config);
        let path_clone = path.clone();

        let handle = std::thread::spawn(move || {
            preset_watcher(presets_clone, path_clone, draining_clone, config_clone);
        });

        // Wait a moment, then update the file with a new mtime.
        std::thread::sleep(std::time::Duration::from_millis(100));
        // Ensure a different mtime by sleeping briefly.
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(&path, r#"{"thumb":{"width":200},"banner":{"width":800}}"#).unwrap();

        // Wait for the watcher to pick up the change (poll interval is 5s).
        std::thread::sleep(std::time::Duration::from_secs(6));

        // Verify updated presets.
        {
            let p = presets.read().unwrap();
            assert_eq!(p.len(), 2, "expected 2 presets after reload");
            assert_eq!(p["thumb"].width, Some(200));
            assert_eq!(p["banner"].width, Some(800));
        }

        // Stop the watcher.
        draining.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preset_watcher_keeps_old_presets_on_invalid_file() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = std::env::temp_dir().join(format!(
            "truss_test_watcher_invalid_{}",
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("presets.json");
        std::fs::write(&path, r#"{"thumb":{"width":100}}"#).unwrap();

        let presets = Arc::new(std::sync::RwLock::new({
            let mut m = std::collections::HashMap::new();
            m.insert(
                "thumb".to_string(),
                TransformOptionsPayload {
                    width: Some(100),
                    height: None,
                    fit: None,
                    position: None,
                    format: None,
                    quality: None,
                    optimize: None,
                    target_quality: None,
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
            m
        }));
        let draining = Arc::new(AtomicBool::new(false));
        let config = Arc::new(ServerConfig::new(dir.clone(), None));

        let presets_clone = Arc::clone(&presets);
        let draining_clone = Arc::clone(&draining);
        let config_clone = Arc::clone(&config);
        let path_clone = path.clone();

        let handle = std::thread::spawn(move || {
            preset_watcher(presets_clone, path_clone, draining_clone, config_clone);
        });

        // Write invalid JSON after a brief delay.
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::thread::sleep(std::time::Duration::from_secs(1));
        std::fs::write(&path, "invalid json!!!").unwrap();

        // Wait for the watcher to process.
        std::thread::sleep(std::time::Duration::from_secs(6));

        // Original presets should still be in place.
        {
            let p = presets.read().unwrap();
            assert_eq!(p.len(), 1, "presets should not change on invalid file");
            assert_eq!(p["thumb"].width, Some(100));
        }

        draining.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- resolve_client_ip tests ---

    fn h(name: &str, value: &str) -> (String, String) {
        (name.to_string(), value.to_string())
    }

    #[test]
    fn resolve_client_ip_no_trusted_proxies_returns_peer() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let headers = vec![h("x-forwarded-for", "1.2.3.4")];
        assert_eq!(resolve_client_ip(peer, &headers, &[]), peer);
    }

    #[test]
    fn resolve_client_ip_peer_not_trusted_returns_peer() {
        let peer: IpAddr = "192.168.1.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Addr("10.0.0.1".parse().unwrap())];
        let headers = vec![h("x-forwarded-for", "1.2.3.4")];
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), peer);
    }

    #[test]
    fn resolve_client_ip_xff_extracts_rightmost_non_trusted() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Addr("10.0.0.1".parse().unwrap())];
        let headers = vec![h("x-forwarded-for", "1.1.1.1, 2.2.2.2")];
        // Rightmost non-trusted: 2.2.2.2
        let expected: IpAddr = "2.2.2.2".parse().unwrap();
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), expected);
    }

    #[test]
    fn resolve_client_ip_xff_skips_trusted_in_chain() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![
            config::TrustedProxy::Addr("10.0.0.1".parse().unwrap()),
            config::TrustedProxy::Addr("10.0.0.2".parse().unwrap()),
        ];
        // Chain: client → proxy1(10.0.0.2) → proxy2(10.0.0.1)
        let headers = vec![h("x-forwarded-for", "1.1.1.1, 10.0.0.2")];
        let expected: IpAddr = "1.1.1.1".parse().unwrap();
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), expected);
    }

    #[test]
    fn resolve_client_ip_xff_all_trusted_falls_back_to_peer() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Cidr("10.0.0.0".parse().unwrap(), 8)];
        let headers = vec![h("x-forwarded-for", "10.1.2.3, 10.4.5.6")];
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), peer);
    }

    #[test]
    fn resolve_client_ip_xri_fallback() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Addr("10.0.0.1".parse().unwrap())];
        let headers = vec![h("x-real-ip", "3.3.3.3")];
        let expected: IpAddr = "3.3.3.3".parse().unwrap();
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), expected);
    }

    #[test]
    fn resolve_client_ip_xff_preferred_over_xri() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Addr("10.0.0.1".parse().unwrap())];
        let headers = vec![h("x-forwarded-for", "1.1.1.1"), h("x-real-ip", "2.2.2.2")];
        let expected: IpAddr = "1.1.1.1".parse().unwrap();
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), expected);
    }

    #[test]
    fn resolve_client_ip_no_headers_falls_back_to_peer() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Addr("10.0.0.1".parse().unwrap())];
        assert_eq!(resolve_client_ip(peer, &[], &trusted), peer);
    }

    #[test]
    fn resolve_client_ip_xff_with_invalid_entries_skipped() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Addr("10.0.0.1".parse().unwrap())];
        let headers = vec![h("x-forwarded-for", "bogus, 1.1.1.1")];
        let expected: IpAddr = "1.1.1.1".parse().unwrap();
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), expected);
    }

    #[test]
    fn resolve_client_ip_cidr_trusted_proxy() {
        let peer: IpAddr = "172.16.5.10".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Cidr(
            "172.16.0.0".parse().unwrap(),
            12,
        )];
        let headers = vec![h("x-forwarded-for", "8.8.8.8")];
        let expected: IpAddr = "8.8.8.8".parse().unwrap();
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), expected);
    }

    #[test]
    fn resolve_client_ip_case_insensitive_headers() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = vec![config::TrustedProxy::Addr("10.0.0.1".parse().unwrap())];
        let headers = vec![h("X-Forwarded-For", "5.5.5.5")];
        let expected: IpAddr = "5.5.5.5".parse().unwrap();
        assert_eq!(resolve_client_ip(peer, &headers, &trusted), expected);
    }

    // --- extract_request_id tests ---

    #[test]
    fn extract_request_id_returns_value_when_present() {
        let headers = vec![h("x-request-id", "abc-123")];
        assert_eq!(extract_request_id(&headers), Some("abc-123".to_string()));
    }

    #[test]
    fn extract_request_id_returns_none_when_absent() {
        let headers = vec![h("content-type", "text/plain")];
        assert_eq!(extract_request_id(&headers), None);
    }

    #[test]
    fn extract_request_id_returns_none_for_empty_value() {
        let headers = vec![h("x-request-id", "")];
        assert_eq!(extract_request_id(&headers), None);
    }

    #[test]
    fn extract_request_id_rejects_cr() {
        let headers = vec![h("x-request-id", "abc\r123")];
        assert_eq!(extract_request_id(&headers), None);
    }

    #[test]
    fn extract_request_id_rejects_lf() {
        let headers = vec![h("x-request-id", "abc\n123")];
        assert_eq!(extract_request_id(&headers), None);
    }

    #[test]
    fn extract_request_id_rejects_nul() {
        let headers = vec![h("x-request-id", "abc\x00123")];
        assert_eq!(extract_request_id(&headers), None);
    }

    // --- extract_cache_status tests ---

    #[test]
    fn extract_cache_status_returns_none_when_absent() {
        let headers = vec![h("content-type", "text/plain")];
        assert_eq!(extract_cache_status(&headers), None);
    }

    #[test]
    fn extract_cache_status_returns_hit_when_present() {
        let headers = vec![h("Cache-Status", "hit;detail=memory")];
        assert_eq!(extract_cache_status(&headers), Some("hit"));
    }

    #[test]
    fn extract_cache_status_returns_miss_when_no_hit() {
        let headers = vec![h("Cache-Status", "miss")];
        assert_eq!(extract_cache_status(&headers), Some("miss"));
    }

    // --- extract_watermark_flag tests ---

    #[test]
    fn extract_watermark_flag_removes_header_and_returns_true() {
        let mut headers = vec![h("content-type", "text/plain"), h("X-Truss-Watermark", "1")];
        assert!(extract_watermark_flag(&mut headers));
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "content-type");
    }

    #[test]
    fn extract_watermark_flag_returns_false_when_absent() {
        let mut headers = vec![h("content-type", "text/plain")];
        assert!(!extract_watermark_flag(&mut headers));
        assert_eq!(headers.len(), 1);
    }

    // --- classify_route tests ---

    #[test]
    fn classify_route_health_endpoints() {
        let make_req = |method: &str, path: &str| HttpRequest {
            method: method.to_string(),
            target: path.to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };

        assert!(matches!(
            classify_route(&make_req("GET", "/health")),
            RouteMetric::Health
        ));
        assert!(matches!(
            classify_route(&make_req("HEAD", "/health")),
            RouteMetric::Health
        ));
        assert!(matches!(
            classify_route(&make_req("GET", "/health/live")),
            RouteMetric::HealthLive
        ));
        assert!(matches!(
            classify_route(&make_req("GET", "/health/ready")),
            RouteMetric::HealthReady
        ));
    }

    #[test]
    fn classify_route_image_endpoints() {
        let make_req = |method: &str, path: &str| HttpRequest {
            method: method.to_string(),
            target: path.to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };

        assert!(matches!(
            classify_route(&make_req("GET", "/images/by-path")),
            RouteMetric::PublicByPath
        ));
        assert!(matches!(
            classify_route(&make_req("GET", "/images/by-url")),
            RouteMetric::PublicByUrl
        ));
        assert!(matches!(
            classify_route(&make_req("POST", "/images:transform")),
            RouteMetric::Transform
        ));
        assert!(matches!(
            classify_route(&make_req("POST", "/images")),
            RouteMetric::Upload
        ));
    }

    #[test]
    fn classify_route_metrics_endpoint() {
        let req = HttpRequest {
            method: "GET".to_string(),
            target: "/metrics".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert!(matches!(classify_route(&req), RouteMetric::Metrics));
    }

    #[test]
    fn classify_route_unknown_path() {
        let req = HttpRequest {
            method: "GET".to_string(),
            target: "/nonexistent".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert!(matches!(classify_route(&req), RouteMetric::Unknown));
    }

    #[test]
    fn classify_route_wrong_method() {
        let req = HttpRequest {
            method: "POST".to_string(),
            target: "/health".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![],
            body: vec![],
        };
        assert!(matches!(classify_route(&req), RouteMetric::Unknown));
    }

    // ── HealthCache tests ────────────────────────────────────────────

    #[cfg(target_os = "linux")]
    #[test]
    fn health_cache_returns_cached_rss_within_ttl() {
        // Use a very long TTL so the cache never expires during the test.
        let cache = HealthCache::new(3600, DEFAULT_HYSTERESIS_MARGIN);
        let first = cache.rss();
        let second = cache.rss();
        // Both calls should return the same cached value.
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn health_cache_returns_cached_disk_free_within_ttl() {
        let dir = temp_dir("hc-disk");
        let cache = HealthCache::new(3600, DEFAULT_HYSTERESIS_MARGIN);
        let first = cache.disk_free(&dir);
        let second = cache.disk_free(&dir);
        assert_eq!(first, second);
        assert!(first.is_some());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn health_cache_refreshes_rss_after_ttl() {
        // TTL of 0 means every call should perform a fresh syscall.
        let cache = HealthCache::new(0, DEFAULT_HYSTERESIS_MARGIN);
        let first = cache.rss();
        let second = cache.rss();
        // Both should succeed (fresh reads).
        assert!(first.is_some());
        assert!(second.is_some());
    }

    #[test]
    fn health_cache_ttl_zero_always_refreshes_disk_free() {
        let dir = temp_dir("hc-disk-zero");
        let cache = HealthCache::new(0, DEFAULT_HYSTERESIS_MARGIN);
        // With TTL=0, the cache timestamp check should never short-circuit.
        let first = cache.disk_free(&dir);
        let second = cache.disk_free(&dir);
        // On Linux both are Some; on other platforms both are None.
        assert_eq!(first.is_some(), second.is_some());
    }

    #[test]
    fn health_cache_default_ttl_in_server_config() {
        let storage = temp_dir("hc-default-ttl");
        let config = ServerConfig::new(storage, None);
        // Default TTL is 5 seconds = 5_000_000_000 nanoseconds.
        assert_eq!(config.health_cache.ttl_nanos, 5_000_000_000);
    }
}
