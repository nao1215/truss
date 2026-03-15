//! Remote URL fetching with layered SSRF protection.
//!
//! # Design decisions
//!
//! **DNS pinning:** We resolve the remote URL once and then pin the HTTP connection
//! to the validated IP addresses via [`PinnedResolver`]. This prevents DNS rebinding
//! attacks where the first resolution returns a public IP (passes validation) but a
//! second resolution during connection returns a private/metadata IP.
//!
//! **IP deny-list:** We check resolved IPs against an explicit deny-list that is
//! more conservative than `is_private()` alone — it also blocks shared address space
//! (100.64.0.0/10), TEST-NET ranges, and reserved IPv4 space (240.0.0.0/4). Cloud
//! metadata endpoints (169.254.169.254, metadata.google.internal, fd00:ec2::254) are
//! always blocked regardless of `allow_insecure_url_sources`.
//!
//! **Origin cache:** Remote fetches are cached on disk to avoid redundant HTTP
//! round-trips. The security policy is re-evaluated before each cache lookup so
//! tightening restrictions invalidates previously cached responses.

use super::ServerConfig;
use super::cache::OriginCache;
use super::handler::TransformSourcePayload;
use super::http_parse::resolve_storage_path;
use super::metrics::{ORIGIN_CACHE_HITS_TOTAL, ORIGIN_CACHE_MISSES_TOTAL};
use super::response::map_source_io_error;
use super::response::{
    HttpResponse, bad_gateway_response, bad_request_response, forbidden_response,
    payload_too_large_response, too_many_redirects_response,
};
use super::stderr_write;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use ureq::http;
use url::Url;

pub(super) const MAX_SOURCE_BYTES: u64 = 100 * 1024 * 1024;
pub(super) const MAX_WATERMARK_BYTES: u64 = 10 * 1024 * 1024;
pub(super) const MAX_REMOTE_REDIRECTS: usize = 5;
#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub(super) const STORAGE_DOWNLOAD_TIMEOUT_SECS: u64 = 30;

pub(super) fn resolve_source_bytes(
    source: TransformSourcePayload,
    config: &ServerConfig,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, HttpResponse> {
    match source {
        TransformSourcePayload::Path { path, .. } => {
            let path = resolve_storage_path(&config.storage_root, &path)?;
            let metadata = std::fs::metadata(&path).map_err(map_source_io_error)?;
            if metadata.len() > config.max_source_bytes {
                return Err(payload_too_large_response("source file is too large"));
            }

            std::fs::read(&path).map_err(map_source_io_error)
        }
        TransformSourcePayload::Url { url, .. } => read_remote_source_bytes(&url, config, deadline),
        #[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
        TransformSourcePayload::Storage { bucket, key, .. } => {
            resolve_storage_source_bytes(bucket.as_deref(), &key, config)
        }
    }
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
fn resolve_storage_source_bytes(
    bucket: Option<&str>,
    key: &str,
    config: &ServerConfig,
) -> Result<Vec<u8>, HttpResponse> {
    match config.storage_backend {
        #[cfg(feature = "s3")]
        super::StorageBackend::S3 => {
            let s3_ctx = config
                .s3_context
                .as_ref()
                .ok_or_else(|| bad_request_response("S3 storage backend is not configured"))?;
            let effective_bucket = bucket.unwrap_or(&s3_ctx.default_bucket);
            super::s3::read_s3_source_bytes(
                effective_bucket,
                key,
                s3_ctx,
                config.storage_timeout_secs,
            )
        }
        #[cfg(feature = "gcs")]
        super::StorageBackend::Gcs => {
            let gcs_ctx = config
                .gcs_context
                .as_ref()
                .ok_or_else(|| bad_request_response("GCS storage backend is not configured"))?;
            let effective_bucket = bucket.unwrap_or(&gcs_ctx.default_bucket);
            super::gcs::read_gcs_source_bytes(
                effective_bucket,
                key,
                gcs_ctx,
                config.storage_timeout_secs,
            )
        }
        #[cfg(feature = "azure")]
        super::StorageBackend::Azure => {
            let azure_ctx = config
                .azure_context
                .as_ref()
                .ok_or_else(|| bad_request_response("Azure storage backend is not configured"))?;
            let effective_bucket = bucket.unwrap_or(&azure_ctx.default_container);
            super::azure::read_azure_source_bytes(
                effective_bucket,
                key,
                azure_ctx,
                config.storage_timeout_secs,
            )
        }
        super::StorageBackend::Filesystem => Err(bad_request_response(
            "storage backend is set to filesystem but received a storage source",
        )),
    }
}

pub(super) fn read_remote_source_bytes(
    url: &str,
    config: &ServerConfig,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, HttpResponse> {
    fetch_remote_bytes(
        url,
        config,
        deadline,
        &RemoteFetchPolicy {
            max_bytes: config.max_source_bytes,
            cache_namespace: "src",
            resource_label: "remote URL",
        },
    )
}

pub(super) fn read_remote_watermark_bytes(
    url: &str,
    config: &ServerConfig,
    deadline: Option<Instant>,
) -> Result<Vec<u8>, HttpResponse> {
    fetch_remote_bytes(
        url,
        config,
        deadline,
        &RemoteFetchPolicy {
            max_bytes: config.max_watermark_bytes,
            cache_namespace: "wm",
            resource_label: "watermark image",
        },
    )
}

struct RemoteFetchPolicy {
    max_bytes: u64,
    cache_namespace: &'static str,
    resource_label: &'static str,
}

fn fetch_remote_bytes(
    url: &str,
    config: &ServerConfig,
    deadline: Option<Instant>,
    policy: &RemoteFetchPolicy,
) -> Result<Vec<u8>, HttpResponse> {
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
        && let Some(bytes) = cache.get(policy.cache_namespace, url)
    {
        ORIGIN_CACHE_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
        if bytes.len() as u64 > policy.max_bytes {
            return Err(payload_too_large_response(&format!(
                "cached {} exceeds {} bytes",
                policy.resource_label, policy.max_bytes
            )));
        }
        return Ok(bytes);
    }

    if origin_cache.is_some() {
        ORIGIN_CACHE_MISSES_TOTAL.fetch_add(1, Ordering::Relaxed);
    }

    let max_redirects = config.max_remote_redirects;
    let mut current_url = url.to_string();

    for redirect_index in 0..=max_redirects {
        let target = prepare_remote_fetch_target(&current_url, config)?;
        let agent = build_remote_agent(&target, deadline);

        match agent.get(target.url.as_str()).call() {
            Ok(response) => {
                let status = response.status().as_u16();
                if is_redirect_status(status) {
                    current_url =
                        next_redirect_url(&target.url, &response, redirect_index, max_redirects)?;
                } else if (200..=299).contains(&status) {
                    let bytes =
                        read_remote_response_body(target.url.as_str(), response, policy.max_bytes)?;
                    if let Some(cache) = origin_cache {
                        cache.put(policy.cache_namespace, url, &bytes);
                    }
                    return Ok(bytes);
                } else {
                    let msg = format!(
                        "failed to fetch {}: upstream HTTP {status}",
                        policy.resource_label
                    );
                    stderr_write(&format!("truss: {msg} for {url}"));
                    return Err(bad_gateway_response(&msg));
                }
            }
            Err(error) => {
                let msg = format!("failed to fetch {}: {error}", policy.resource_label);
                stderr_write(&format!("truss: {msg}"));
                return Err(bad_gateway_response(&msg));
            }
        }
    }

    Err(too_many_redirects_response(&format!(
        "{} URL exceeded the redirect limit",
        policy.resource_label
    )))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteFetchTarget {
    pub(super) url: Url,
    pub(super) netloc: String,
    pub(super) addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PinnedResolver {
    pub(super) expected_netloc: String,
    pub(super) addrs: Vec<SocketAddr>,
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

pub(super) fn prepare_remote_fetch_target(
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

pub(super) fn build_remote_agent(
    target: &RemoteFetchTarget,
    deadline: Option<Instant>,
) -> ureq::Agent {
    let remaining = deadline.map(|d| d.saturating_duration_since(Instant::now()));
    let body_timeout = match remaining {
        Some(r) if r.is_zero() => Duration::from_millis(1),
        Some(r) => r.min(Duration::from_secs(30)),
        None => Duration::from_secs(30),
    };
    let connect_timeout = match remaining {
        Some(r) if r.is_zero() => Duration::from_millis(1),
        Some(r) => r.min(Duration::from_secs(10)),
        None => Duration::from_secs(10),
    };
    let config = ureq::config::Config::builder()
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_connect(Some(connect_timeout))
        .timeout_recv_body(Some(body_timeout))
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

pub(super) fn next_redirect_url(
    current_url: &Url,
    response: &http::Response<ureq::Body>,
    redirect_index: usize,
    max_redirects: usize,
) -> Result<String, HttpResponse> {
    if redirect_index == max_redirects {
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

pub(super) fn parse_remote_url(value: &str) -> Result<Url, HttpResponse> {
    Url::parse(value)
        .map_err(|error| bad_request_response(&format!("remote URL is invalid: {error}")))
}

pub(super) fn validate_remote_url(
    url: &Url,
    config: &ServerConfig,
) -> Result<Vec<SocketAddr>, HttpResponse> {
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

    // Always block cloud metadata endpoints, even when insecure sources are
    // allowed.  This matches the unconditional check in
    // `validate_backend_endpoint_url` and the guarantee stated in the module
    // doc-comment.
    if is_cloud_metadata_host(url) {
        return Err(forbidden_response(
            "remote URL points to a cloud metadata service",
        ));
    }

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

pub(super) fn read_remote_response_body(
    url: &str,
    response: http::Response<ureq::Body>,
    max_bytes: u64,
) -> Result<Vec<u8>, HttpResponse> {
    validate_remote_content_encoding(&response)?;

    if response
        .headers()
        .get("Content-Length")
        .and_then(|v: &http::HeaderValue| v.to_str().ok())
        .and_then(|value: &str| value.parse::<u64>().ok())
        .is_some_and(|len| len > max_bytes)
    {
        return Err(payload_too_large_response(&format!(
            "remote response exceeds {max_bytes} bytes"
        )));
    }

    let mut reader = response.into_body().into_reader().take(max_bytes + 1);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|error| {
        bad_gateway_response(&format!("failed to read remote URL `{url}`: {error}"))
    })?;

    if bytes.len() as u64 > max_bytes {
        return Err(payload_too_large_response(&format!(
            "remote response exceeds {max_bytes} bytes"
        )));
    }

    Ok(bytes)
}

pub(super) fn validate_remote_content_encoding(
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

pub(super) fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Validates a storage-backend endpoint URL (e.g. `TRUSS_GCS_ENDPOINT`,
/// `AWS_ENDPOINT_URL`) to prevent SSRF attacks.
///
/// Cloud metadata hostnames are always blocked regardless of `allow_insecure`.
/// When `allow_insecure` is false, the hostname is resolved via DNS and every
/// resulting IP is checked against the private/loopback deny-list.
#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub(super) fn validate_backend_endpoint_url(
    url: &str,
    env_var_name: &str,
    allow_insecure: bool,
) -> Result<(), std::io::Error> {
    use std::io;
    use std::net::ToSocketAddrs;

    let parsed: url::Url = url.parse().map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{env_var_name} is not a valid URL: {e}"),
        )
    })?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{env_var_name} must use http or https scheme, got `{other}`"),
            ));
        }
    }

    // Always block cloud metadata services, even in insecure mode.
    if is_cloud_metadata_host(&parsed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{env_var_name} must not point to a cloud metadata service"),
        ));
    }

    if let Some(host) = parsed.host_str() {
        if !allow_insecure {
            let port = parsed.port_or_known_default().unwrap_or(80);
            let addr_str = format!("{host}:{port}");
            let addrs: Vec<_> = addr_str
                .to_socket_addrs()
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("{env_var_name} could not be resolved: {e}"),
                    )
                })?
                .collect();
            if addrs.iter().any(|a| is_disallowed_remote_ip(a.ip())) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{env_var_name} must not point to a private or loopback address"),
                ));
            }
        }
    }

    Ok(())
}

/// Returns `true` when the URL targets a well-known cloud metadata service.
///
/// Checked hostnames:
/// - `169.254.169.254` (AWS / Azure / most clouds)
/// - `metadata.google.internal` (GCP)
/// - `[fd00:ec2::254]` (AWS IMDSv2 IPv6)
fn is_cloud_metadata_host(url: &Url) -> bool {
    // Explicit checks cover AWS/Azure (169.254.169.254), GCP
    // (metadata.google.internal), and AWS IMDSv2 IPv6.  Other providers
    // (DigitalOcean, Oracle, etc.) also use 169.254.169.254 and are
    // therefore caught here.  Alibaba's 100.100.100.200 falls in the
    // CGNAT range (100.64.0.0/10) and is rejected by `is_disallowed_ipv4`.
    match url.host_str() {
        Some("169.254.169.254") | Some("metadata.google.internal") => true,
        _ => {
            url.host()
                == Some(url::Host::Ipv6(std::net::Ipv6Addr::new(
                    0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254,
                )))
        }
    }
}

pub(super) fn is_disallowed_remote_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_disallowed_ipv4(ip),
        IpAddr::V6(ip) => is_disallowed_ipv6(ip),
    }
}

pub(super) fn is_disallowed_ipv4(ip: Ipv4Addr) -> bool {
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

pub(super) fn is_disallowed_ipv6(ip: Ipv6Addr) -> bool {
    // Check native IPv6 properties first.
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }

    // Check IPv4-mapped addresses (e.g. ::ffff:127.0.0.1) against IPv4 rules
    // to prevent SSRF bypass via mapped addresses.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_disallowed_ipv4(v4);
    }

    let segments = ip.segments();

    // Check deprecated IPv4-compatible addresses (e.g. ::127.0.0.1).
    // Some network stacks still route these despite deprecation (RFC 4291 §2.5.5.1).
    // Only applies when the upper 96 bits are zero and it's not IPv4-mapped.
    if segments[..6] == [0, 0, 0, 0, 0, 0] {
        let v4 = Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        );
        return is_disallowed_ipv4(v4);
    }

    // Block 6to4 addresses (2002::/16) which embed an IPv4 address in bits 16-48.
    // An attacker can encode private IPs like 127.0.0.1 as 2002:7f00:0001::.
    if segments[0] == 0x2002 {
        let embedded = Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        );
        return is_disallowed_ipv4(embedded);
    }

    // Block Teredo addresses (2001:0000::/32) which can tunnel to arbitrary IPv4.
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        return true;
    }

    ip.is_unique_local()
        || ip.is_unicast_link_local()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

#[cfg(test)]
mod redirect_tests {
    use super::*;

    #[test]
    fn is_redirect_status_recognizes_all_redirect_codes() {
        assert!(is_redirect_status(301));
        assert!(is_redirect_status(302));
        assert!(is_redirect_status(303));
        assert!(is_redirect_status(307));
        assert!(is_redirect_status(308));
    }

    #[test]
    fn is_redirect_status_rejects_non_redirect_codes() {
        assert!(!is_redirect_status(200));
        assert!(!is_redirect_status(201));
        assert!(!is_redirect_status(304));
        assert!(!is_redirect_status(400));
        assert!(!is_redirect_status(404));
        assert!(!is_redirect_status(500));
    }

    #[test]
    fn parse_remote_url_accepts_valid_urls() {
        assert!(parse_remote_url("http://example.com/image.png").is_ok());
        assert!(parse_remote_url("https://cdn.example.com/path/to/img.jpg").is_ok());
    }

    #[test]
    fn parse_remote_url_rejects_invalid_urls() {
        assert!(parse_remote_url("not a url").is_err());
        assert!(parse_remote_url("").is_err());
    }

    #[test]
    fn validate_remote_url_rejects_non_http_scheme() {
        let url = Url::parse("ftp://example.com/image.png").unwrap();
        let config = ServerConfig::new(std::env::temp_dir(), None);
        assert!(validate_remote_url(&url, &config).is_err());
    }

    #[test]
    fn validate_remote_url_rejects_userinfo() {
        let url = Url::parse("http://user:pass@example.com/image.png").unwrap();
        let config = ServerConfig::new(std::env::temp_dir(), None);
        assert!(validate_remote_url(&url, &config).is_err());
    }

    #[test]
    fn validate_remote_url_rejects_non_standard_port_when_strict() {
        let url = Url::parse("http://example.com:8080/image.png").unwrap();
        let mut config = ServerConfig::new(std::env::temp_dir(), None);
        config.allow_insecure_url_sources = false;
        assert!(validate_remote_url(&url, &config).is_err());
    }

    #[test]
    fn validate_remote_url_allows_standard_port_when_strict() {
        let url = Url::parse("http://example.com:80/image.png").unwrap();
        let mut config = ServerConfig::new(std::env::temp_dir(), None);
        config.allow_insecure_url_sources = false;
        // Port 80 should be allowed even in strict mode.
        // The call may fail due to IP check, but should NOT fail due to port.
        let result = validate_remote_url(&url, &config);
        if let Err(ref resp) = result {
            let body = String::from_utf8_lossy(&resp.body);
            assert!(
                !body.contains("port is not allowed"),
                "port 80 should be allowed in strict mode"
            );
        }
    }

    #[test]
    fn validate_remote_url_blocks_metadata_even_when_insecure() {
        let mut config = ServerConfig::new(std::env::temp_dir(), None);
        config.allow_insecure_url_sources = true;

        // AWS metadata endpoint
        let url = Url::parse("http://169.254.169.254/latest/meta-data").unwrap();
        let err = validate_remote_url(&url, &config).unwrap_err();
        assert!(
            String::from_utf8_lossy(&err.body).contains("cloud metadata"),
            "should block AWS metadata"
        );

        // GCP metadata endpoint
        let url = Url::parse("http://metadata.google.internal/computeMetadata").unwrap();
        let err = validate_remote_url(&url, &config).unwrap_err();
        assert!(
            String::from_utf8_lossy(&err.body).contains("cloud metadata"),
            "should block GCP metadata"
        );

        // AWS IMDSv2 IPv6 endpoint
        let url = Url::parse("http://[fd00:ec2::254]/latest/meta-data").unwrap();
        let err = validate_remote_url(&url, &config).unwrap_err();
        assert!(
            String::from_utf8_lossy(&err.body).contains("cloud metadata"),
            "should block AWS IMDSv2 IPv6 metadata"
        );
    }

    /// Helper to construct an `http::Response<ureq::Body>` for tests that only
    /// inspect headers. The body is an empty byte vector.
    fn build_response(builder: http::response::Builder) -> http::Response<ureq::Body> {
        let (parts, _) = builder.body(()).unwrap().into_parts();
        let body = ureq::Body::builder().data(vec![]);
        http::Response::from_parts(parts, body)
    }

    #[test]
    fn validate_content_encoding_accepts_known_encodings() {
        let response =
            build_response(ureq::http::Response::builder().header("Content-Encoding", "gzip"));
        assert!(validate_remote_content_encoding(&response).is_ok());

        let response =
            build_response(ureq::http::Response::builder().header("Content-Encoding", "br"));
        assert!(validate_remote_content_encoding(&response).is_ok());

        let response =
            build_response(ureq::http::Response::builder().header("Content-Encoding", "identity"));
        assert!(validate_remote_content_encoding(&response).is_ok());
    }

    #[test]
    fn validate_content_encoding_rejects_unknown_encoding() {
        let response =
            build_response(ureq::http::Response::builder().header("Content-Encoding", "deflate"));
        assert!(validate_remote_content_encoding(&response).is_err());
    }

    #[test]
    fn validate_content_encoding_accepts_no_header() {
        let response = build_response(ureq::http::Response::builder());
        assert!(validate_remote_content_encoding(&response).is_ok());
    }

    // ── IP deny-list tests ──────────────────────────────────────────────

    #[test]
    fn disallowed_ipv4_blocks_private_ranges() {
        assert!(is_disallowed_ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_disallowed_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_disallowed_ipv4(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn disallowed_ipv4_blocks_loopback() {
        assert!(is_disallowed_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
    }

    #[test]
    fn disallowed_ipv4_blocks_link_local() {
        assert!(is_disallowed_ipv4(Ipv4Addr::new(169, 254, 169, 254)));
    }

    #[test]
    fn disallowed_ipv4_blocks_shared_address_space() {
        // 100.64.0.0/10 (CGNAT)
        assert!(is_disallowed_ipv4(Ipv4Addr::new(100, 64, 0, 1)));
        assert!(is_disallowed_ipv4(Ipv4Addr::new(100, 127, 255, 254)));
    }

    #[test]
    fn disallowed_ipv4_blocks_reserved_range() {
        // 240.0.0.0/4
        assert!(is_disallowed_ipv4(Ipv4Addr::new(240, 0, 0, 1)));
        assert!(is_disallowed_ipv4(Ipv4Addr::new(255, 255, 255, 254)));
    }

    #[test]
    fn disallowed_ipv4_allows_public_addresses() {
        assert!(!is_disallowed_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_disallowed_ipv4(Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_disallowed_ipv4(Ipv4Addr::new(93, 184, 216, 34)));
    }

    #[test]
    fn disallowed_ipv6_blocks_loopback() {
        assert!(is_disallowed_ipv6(Ipv6Addr::LOCALHOST));
    }

    #[test]
    fn disallowed_ipv6_blocks_ipv4_mapped_loopback() {
        // ::ffff:127.0.0.1
        let mapped = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001);
        assert!(is_disallowed_ipv6(mapped));
    }

    #[test]
    fn disallowed_ipv6_blocks_unique_local() {
        let ula = Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1);
        assert!(is_disallowed_ipv6(ula));
    }

    #[test]
    fn disallowed_ipv6_blocks_documentation_prefix() {
        // 2001:db8::/32
        let doc = Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1);
        assert!(is_disallowed_ipv6(doc));
    }

    #[test]
    fn disallowed_ipv6_blocks_ipv4_compatible_loopback() {
        // ::127.0.0.1 (deprecated IPv4-compatible address)
        let compat = Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0x7f00, 0x0001);
        assert!(is_disallowed_ipv6(compat));
    }

    #[test]
    fn disallowed_ipv6_blocks_ipv4_compatible_private() {
        // ::10.0.0.1 (deprecated IPv4-compatible address embedding private IP)
        let compat = Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0x0a00, 0x0001);
        assert!(is_disallowed_ipv6(compat));
    }

    #[test]
    fn disallowed_ipv6_blocks_6to4_loopback() {
        // 2002:7f00:0001:: encodes 127.0.0.1 via 6to4
        let addr = Ipv6Addr::new(0x2002, 0x7f00, 0x0001, 0, 0, 0, 0, 0);
        assert!(is_disallowed_ipv6(addr));
    }

    #[test]
    fn disallowed_ipv6_blocks_6to4_private() {
        // 2002:c0a8:0001:: encodes 192.168.0.1 via 6to4
        let addr = Ipv6Addr::new(0x2002, 0xc0a8, 0x0001, 0, 0, 0, 0, 0);
        assert!(is_disallowed_ipv6(addr));
    }

    #[test]
    fn disallowed_ipv6_allows_6to4_public() {
        // 2002:0801:0101:: encodes 8.1.1.1 (public) via 6to4
        let addr = Ipv6Addr::new(0x2002, 0x0801, 0x0101, 0, 0, 0, 0, 0);
        assert!(!is_disallowed_ipv6(addr));
    }

    #[test]
    fn disallowed_ipv6_blocks_teredo() {
        // 2001:0000::/32 is the Teredo prefix
        let teredo = Ipv6Addr::new(0x2001, 0x0000, 0x1234, 0, 0, 0, 0, 1);
        assert!(is_disallowed_ipv6(teredo));
    }

    // ── max_remote_redirects config enforcement ─────────────────────────

    #[test]
    fn max_remote_redirects_default_is_five() {
        let config = ServerConfig::new(std::env::temp_dir(), None);
        assert_eq!(config.max_remote_redirects, MAX_REMOTE_REDIRECTS);
        assert_eq!(config.max_remote_redirects, 5);
    }

    #[test]
    fn next_redirect_url_at_limit_returns_error() {
        // When redirect_index == max_redirects, should return an error.
        let current_url = Url::parse("http://example.com/a").unwrap();
        let response = build_response(
            ureq::http::Response::builder()
                .status(302)
                .header("Location", "/b"),
        );

        let result = next_redirect_url(&current_url, &response, 3, 3);
        assert!(
            result.is_err(),
            "should error when redirect_index == max_redirects"
        );
    }

    #[test]
    fn next_redirect_url_within_limit_follows_redirect() {
        let current_url = Url::parse("http://example.com/a").unwrap();
        let response = build_response(
            ureq::http::Response::builder()
                .status(302)
                .header("Location", "/b"),
        );

        let result = next_redirect_url(&current_url, &response, 0, 5);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "http://example.com/b");
    }

    #[test]
    fn next_redirect_url_resolves_relative_location() {
        let current_url = Url::parse("http://example.com/dir/a").unwrap();
        let response = build_response(
            ureq::http::Response::builder()
                .status(301)
                .header("Location", "b"),
        );

        let result = next_redirect_url(&current_url, &response, 0, 5).unwrap();
        assert_eq!(result, "http://example.com/dir/b");
    }

    #[test]
    fn next_redirect_url_resolves_absolute_location() {
        let current_url = Url::parse("http://example.com/a").unwrap();
        let response = build_response(
            ureq::http::Response::builder()
                .status(307)
                .header("Location", "https://cdn.example.com/image.png"),
        );

        let result = next_redirect_url(&current_url, &response, 0, 5).unwrap();
        assert_eq!(result, "https://cdn.example.com/image.png");
    }

    #[test]
    fn next_redirect_url_missing_location_returns_error() {
        let current_url = Url::parse("http://example.com/a").unwrap();
        let response = build_response(ureq::http::Response::builder().status(302));

        let result = next_redirect_url(&current_url, &response, 0, 5);
        assert!(
            result.is_err(),
            "missing Location header should produce an error"
        );
    }

    #[test]
    fn next_redirect_url_last_allowed_redirect_succeeds() {
        // redirect_index=4 with max=5 should succeed (the limit is exclusive).
        let current_url = Url::parse("http://example.com/a").unwrap();
        let response = build_response(
            ureq::http::Response::builder()
                .status(302)
                .header("Location", "/final"),
        );

        let result = next_redirect_url(&current_url, &response, 4, 5);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "http://example.com/final");
    }
}

#[cfg(test)]
#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
mod tests {
    use super::*;

    #[test]
    fn test_validate_backend_endpoint_url_accepts_http() {
        // allow_insecure=true so localhost passes
        assert!(validate_backend_endpoint_url("http://localhost:4443", "TEST_VAR", true).is_ok());
    }

    #[test]
    fn test_validate_backend_endpoint_url_accepts_https() {
        assert!(
            validate_backend_endpoint_url("https://storage.googleapis.com", "TEST_VAR", true)
                .is_ok()
        );
    }

    #[test]
    fn test_validate_backend_endpoint_url_rejects_non_http_scheme() {
        assert!(validate_backend_endpoint_url("ftp://example.com", "TEST_VAR", true).is_err());
        assert!(validate_backend_endpoint_url("file:///etc/passwd", "TEST_VAR", true).is_err());
    }

    #[test]
    fn test_validate_backend_endpoint_url_rejects_metadata_always() {
        // Even with allow_insecure=true, metadata services must be blocked.
        assert!(
            validate_backend_endpoint_url(
                "http://169.254.169.254/latest/meta-data",
                "TEST_VAR",
                true,
            )
            .is_err()
        );
        assert!(
            validate_backend_endpoint_url(
                "http://metadata.google.internal/computeMetadata",
                "TEST_VAR",
                true,
            )
            .is_err()
        );
        assert!(
            validate_backend_endpoint_url(
                "http://[fd00:ec2::254]/latest/meta-data",
                "TEST_VAR",
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn test_validate_backend_endpoint_url_rejects_invalid_url() {
        assert!(validate_backend_endpoint_url("not a url", "TEST_VAR", true).is_err());
    }

    #[test]
    fn test_validate_backend_endpoint_url_rejects_loopback_strict() {
        // allow_insecure=false should block 127.0.0.1
        assert!(validate_backend_endpoint_url("http://127.0.0.1:6379", "TEST_VAR", false).is_err());
    }

    #[test]
    fn test_validate_backend_endpoint_url_allows_loopback_insecure() {
        // allow_insecure=true should allow 127.0.0.1
        assert!(validate_backend_endpoint_url("http://127.0.0.1:4443", "TEST_VAR", true).is_ok());
    }

    #[test]
    fn test_validate_backend_endpoint_url_error_contains_var_name() {
        let err =
            validate_backend_endpoint_url("ftp://example.com", "MY_CUSTOM_VAR", true).unwrap_err();
        assert!(
            err.to_string().contains("MY_CUSTOM_VAR"),
            "error should mention the env var name: {err}"
        );
    }
}
