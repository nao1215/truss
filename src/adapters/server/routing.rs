/// Route dispatch, connection handling, and access logging.
use std::io;
use std::net::{IpAddr, TcpStream};
use std::time::Instant;

use serde_json::json;
use uuid::Uuid;

use super::config::ServerConfig;
use super::handler::{
    handle_health, handle_health_live, handle_health_ready, handle_metrics_request,
    handle_public_path_request, handle_public_url_request, handle_transform_request,
    handle_upload_request,
};
use super::http_parse;
use super::lifecycle::{SOCKET_READ_TIMEOUT, SOCKET_WRITE_TIMEOUT};
use super::metrics::{RouteMetric, record_http_metrics, record_http_request_duration, status_code};
use super::response::{
    HttpResponse, NOT_FOUND_BODY, too_many_requests_response, write_response,
    write_response_compressed,
};

use subtle::ConstantTimeEq;

pub(super) struct AccessLogEntry<'a> {
    pub(super) request_id: &'a str,
    pub(super) method: &'a str,
    pub(super) path: &'a str,
    pub(super) route: &'a str,
    pub(super) status: &'a str,
    pub(super) start: Instant,
    pub(super) cache_status: Option<&'a str>,
    pub(super) watermark: bool,
}

/// Extracts the `X-Request-Id` header value from request headers.
/// Returns `None` if the header is absent, empty, or contains
/// characters unsafe for HTTP headers (CR, LF, NUL).
pub(super) fn extract_request_id(headers: &[(String, String)]) -> Option<String> {
    headers.iter().find_map(|(name, value)| {
        if name != "x-request-id" || value.is_empty() {
            return None;
        }
        if value
            .bytes()
            .any(|b| b == b'\r' || b == b'\n' || b == b'\0')
        {
            return None;
        }
        Some(value.clone())
    })
}

/// Classifies the `Cache-Status` response header as `"hit"` or `"miss"`.
/// Returns `None` when the header is absent.
pub(super) fn extract_cache_status(headers: &[(String, String)]) -> Option<&'static str> {
    headers
        .iter()
        .find_map(|(name, value)| (name == "Cache-Status").then_some(value.as_str()))
        .map(|v| if v.contains("hit") { "hit" } else { "miss" })
}

/// Extracts and removes the internal `X-Truss-Watermark` header, returning whether it was set.
pub(super) fn extract_watermark_flag(headers: &mut Vec<(String, String)>) -> bool {
    let pos = headers
        .iter()
        .position(|(name, _)| name == "X-Truss-Watermark");
    if let Some(idx) = pos {
        headers.swap_remove(idx);
        true
    } else {
        false
    }
}

/// Resolves the real client IP when the server runs behind trusted reverse
/// proxies.
///
/// When `peer_ip` belongs to a trusted proxy the function inspects
/// `X-Forwarded-For` (right-to-left, skipping trusted entries) and then
/// `X-Real-IP`.  If neither header yields a usable address the original
/// `peer_ip` is returned.
pub(super) fn resolve_client_ip(
    peer_ip: IpAddr,
    headers: &[(String, String)],
    trusted_proxies: &[super::config::TrustedProxy],
) -> IpAddr {
    use super::config::is_trusted_proxy;

    if trusted_proxies.is_empty() || !is_trusted_proxy(trusted_proxies, peer_ip) {
        return peer_ip;
    }

    // Try X-Forwarded-For first: walk from rightmost to leftmost, skipping
    // addresses that are themselves trusted proxies.  The rightmost
    // non-trusted address is the most reliable client IP because each proxy
    // appends the upstream address it received the connection from.
    // Per RFC 7230 §3.2.2, multiple headers with the same name are
    // semantically equivalent to a single comma-joined header.
    let xff_values: Vec<&str> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("x-forwarded-for"))
        .map(|(_, v)| v.as_str())
        .collect();
    if !xff_values.is_empty() {
        let joined = xff_values.join(",");
        for segment in joined.rsplit(',') {
            if let Ok(ip) = segment.trim().parse::<IpAddr>()
                && !is_trusted_proxy(trusted_proxies, ip)
            {
                return ip;
            }
        }
    }

    // Fallback: X-Real-IP (single IP set by some proxies like nginx).
    if let Some(xri) = headers
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case("x-real-ip"))
        .map(|(_, v)| v.as_str())
        && let Ok(ip) = xri.trim().parse::<IpAddr>()
        && !is_trusted_proxy(trusted_proxies, ip)
    {
        return ip;
    }

    // All forwarded addresses are trusted (or headers are absent/invalid) —
    // fall back to the TCP peer address.
    peer_ip
}

pub(super) fn emit_access_log(config: &ServerConfig, entry: &AccessLogEntry<'_>) {
    config.log(
        &json!({
            "kind": "access_log",
            "request_id": entry.request_id,
            "method": entry.method,
            "path": entry.path,
            "route": entry.route,
            "status": entry.status,
            "latency_ms": entry.start.elapsed().as_millis() as u64,
            "cache_status": entry.cache_status,
            "watermark": entry.watermark,
        })
        .to_string(),
    );
}

pub(super) fn handle_stream(mut stream: TcpStream, config: &ServerConfig) -> io::Result<()> {
    // Prevent slow or stalled clients from blocking the accept loop indefinitely.
    if let Err(err) = stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT)) {
        config.log_warn(&format!("failed to set socket read timeout: {err}"));
    }
    if let Err(err) = stream.set_write_timeout(Some(SOCKET_WRITE_TIMEOUT)) {
        config.log_warn(&format!("failed to set socket write timeout: {err}"));
    }

    // Extract the peer IP once for rate limiting. If peer_addr fails
    // (e.g. the socket was already closed), skip rate limiting for this
    // connection rather than rejecting it.
    let peer_ip = stream.peer_addr().ok().map(|addr| addr.ip());

    let mut requests_served: u64 = 0;

    loop {
        let partial = match http_parse::read_request_headers(&mut stream, config.max_upload_bytes) {
            Ok(partial) => partial,
            Err(response) => {
                if requests_served > 0 {
                    return Ok(());
                }
                let _ = write_response(&mut stream, response, true);
                return Ok(());
            }
        };

        // Start timing after headers are read so latency reflects server
        // processing time, not client send / socket-wait time.
        let start = Instant::now();

        let request_id =
            extract_request_id(&partial.headers).unwrap_or_else(|| Uuid::new_v4().to_string());

        let is_head = partial.method == "HEAD";

        // --- Per-IP rate limiting ---
        // When behind a trusted reverse proxy, resolve the real client IP
        // from X-Forwarded-For / X-Real-IP so each end-user gets an
        // independent rate-limit bucket.
        let client_ip = peer_ip.map(|ip| {
            if config.trusted_proxies.is_empty() {
                ip
            } else {
                resolve_client_ip(ip, &partial.headers, &config.trusted_proxies)
            }
        });
        if let (Some(limiter), Some(ip)) = (&config.rate_limiter, client_ip)
            && !limiter.check(ip)
        {
            let mut response = too_many_requests_response("rate limit exceeded — try again later");
            response
                .headers
                .push(("X-Request-Id".to_string(), request_id.clone()));
            response.strip_body_if_head(is_head);
            record_http_metrics(RouteMetric::Unknown, response.status);
            let sc = status_code(response.status).unwrap_or("unknown");
            let method_log = partial.method.clone();
            let path_log = partial.path().to_string();
            let _ = write_response(&mut stream, response, true);
            record_http_request_duration(RouteMetric::Unknown, start);
            emit_access_log(
                config,
                &AccessLogEntry {
                    request_id: &request_id,
                    method: &method_log,
                    path: &path_log,
                    route: &path_log,
                    status: sc,
                    start,
                    cache_status: None,
                    watermark: false,
                },
            );
            return Ok(());
        }

        let client_wants_close = partial
            .headers
            .iter()
            .any(|(name, value)| name == "connection" && value.eq_ignore_ascii_case("close"));

        let accepts_gzip = config.enable_compression
            && http_parse::header_value(&partial.headers, "accept-encoding")
                .is_some_and(|v| http_parse::accepts_encoding(v, "gzip"));

        let requires_auth = matches!(
            (partial.method.as_str(), partial.path()),
            ("POST", "/images:transform") | ("POST", "/images")
        );
        if requires_auth
            && let Err(mut response) =
                super::auth::authorize_request_headers(&partial.headers, config)
        {
            response
                .headers
                .push(("X-Request-Id".to_string(), request_id.clone()));
            record_http_metrics(RouteMetric::Unknown, response.status);
            let sc = status_code(response.status).unwrap_or("unknown");
            let method_log = partial.method.clone();
            let path_log = partial.path().to_string();
            let _ = write_response_compressed(
                &mut stream,
                response,
                true,
                accepts_gzip,
                config.compression_level,
            );
            record_http_request_duration(RouteMetric::Unknown, start);
            emit_access_log(
                config,
                &AccessLogEntry {
                    request_id: &request_id,
                    method: &method_log,
                    path: &path_log,
                    route: &path_log,
                    status: sc,
                    start,
                    cache_status: None,
                    watermark: false,
                },
            );
            return Ok(());
        }

        // Early-reject /metrics requests before draining the body so that
        // unauthenticated or disabled-metrics requests do not force a body read.
        if matches!(
            (partial.method.as_str(), partial.path()),
            ("GET" | "HEAD", "/metrics")
        ) {
            let early_response = if config.disable_metrics {
                Some(HttpResponse::problem(
                    "404 Not Found",
                    NOT_FOUND_BODY.as_bytes().to_vec(),
                ))
            } else if let Some(expected) = &config.metrics_token {
                let provided = http_parse::header_value(&partial.headers, "authorization")
                    .and_then(|value| {
                        let (scheme, token) = value.split_once(|c: char| c.is_whitespace())?;
                        scheme.eq_ignore_ascii_case("Bearer").then(|| token.trim())
                    });
                match provided {
                    Some(token) if token.as_bytes().ct_eq(expected.as_bytes()).into() => None,
                    _ => Some(super::response::auth_required_response(
                        "metrics endpoint requires authentication",
                    )),
                }
            } else {
                None
            };

            if let Some(mut response) = early_response {
                response
                    .headers
                    .push(("X-Request-Id".to_string(), request_id.clone()));
                response.strip_body_if_head(is_head);
                record_http_metrics(RouteMetric::Metrics, response.status);
                let sc = status_code(response.status).unwrap_or("unknown");
                let method_log = partial.method.clone();
                let path_log = partial.path().to_string();
                let _ = write_response_compressed(
                    &mut stream,
                    response,
                    true,
                    accepts_gzip,
                    config.compression_level,
                );
                record_http_request_duration(RouteMetric::Metrics, start);
                emit_access_log(
                    config,
                    &AccessLogEntry {
                        request_id: &request_id,
                        method: &method_log,
                        path: &path_log,
                        route: "/metrics",
                        status: sc,
                        start,
                        cache_status: None,
                        watermark: false,
                    },
                );
                return Ok(());
            }
        }

        // Clone method/path before `read_request_body` consumes `partial`.
        let method = partial.method.clone();
        let path = partial.path().to_string();

        let request = match http_parse::read_request_body(&mut stream, partial) {
            Ok(request) => request,
            Err(mut response) => {
                response
                    .headers
                    .push(("X-Request-Id".to_string(), request_id.clone()));
                record_http_metrics(RouteMetric::Unknown, response.status);
                let sc = status_code(response.status).unwrap_or("unknown");
                let _ = write_response_compressed(
                    &mut stream,
                    response,
                    true,
                    accepts_gzip,
                    config.compression_level,
                );
                record_http_request_duration(RouteMetric::Unknown, start);
                emit_access_log(
                    config,
                    &AccessLogEntry {
                        request_id: &request_id,
                        method: &method,
                        path: &path,
                        route: &path,
                        status: sc,
                        start,
                        cache_status: None,
                        watermark: false,
                    },
                );
                return Ok(());
            }
        };
        let route = classify_route(&request);
        let mut response = route_request(request, config);
        record_http_metrics(route, response.status);

        response
            .headers
            .push(("X-Request-Id".to_string(), request_id.clone()));

        let cache_status = extract_cache_status(&response.headers);
        let had_watermark = extract_watermark_flag(&mut response.headers);

        let sc = status_code(response.status).unwrap_or("unknown");

        response.strip_body_if_head(is_head);

        requests_served += 1;
        let close_after = client_wants_close || requests_served >= config.keep_alive_max_requests;

        write_response_compressed(
            &mut stream,
            response,
            close_after,
            accepts_gzip,
            config.compression_level,
        )?;
        record_http_request_duration(route, start);

        emit_access_log(
            config,
            &AccessLogEntry {
                request_id: &request_id,
                method: &method,
                path: &path,
                route: route.as_label(),
                status: sc,
                start,
                cache_status,
                watermark: had_watermark,
            },
        );

        if close_after {
            return Ok(());
        }
    }
}

pub(super) fn route_request(
    request: http_parse::HttpRequest,
    config: &ServerConfig,
) -> HttpResponse {
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

pub(super) fn classify_route(request: &http_parse::HttpRequest) -> RouteMetric {
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
