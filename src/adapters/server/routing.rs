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
use super::lifecycle::{HEADER_READ_DEADLINE, SOCKET_READ_TIMEOUT, SOCKET_WRITE_TIMEOUT};
use super::metrics::{RouteMetric, record_http_metrics, record_http_request_duration, status_code};
use super::response::{
    HttpResponse, NOT_FOUND_BODY, ResponseWriteOptions, problem_response,
    too_many_requests_response, write_response,
};
use crate::core::error_class::ErrorClass;

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

/// Longest client-supplied request id echoed back. A UUID is 36 characters and a
/// W3C `traceparent` is 55, so this is generous for every identifier a caller
/// would realistically forward, and it bounds what one header can add to every
/// response and every access log line for a request.
pub(super) const MAX_REQUEST_ID_LEN: usize = 128;

/// Returns the client-supplied request id when it is safe to echo verbatim.
///
/// The value is reflected into a response header, so rejecting only CR, LF, and
/// NUL is not enough: control characters and DEL are not `field-vchar` and
/// `obs-text` is deprecated under RFC 9110 section 5.5, so a proxy in front of
/// truss is entitled to reject whatever gets through. The rule here is
/// deliberately narrower than that grammar — printable ASCII only — and anything
/// else falls back to a generated id.
pub(super) fn extract_request_id(headers: &[(String, String)]) -> Option<String> {
    headers.iter().find_map(|(name, value)| {
        if name != "x-request-id" || value.is_empty() || value.len() > MAX_REQUEST_ID_LEN {
            return None;
        }
        if !value.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
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

pub(super) fn handle_stream(
    mut stream: TcpStream,
    accepted_at: Instant,
    config: &ServerConfig,
) -> io::Result<()> {
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
    // What a client wrote past the end of one request belongs to the next one it sent, since
    // a client controls neither how its bytes are packetised nor where a read stops.
    let mut carried_over: Vec<u8> = Vec::new();

    loop {
        // The socket read timeout is an inactivity timeout and resets on every byte, so it
        // cannot bound the header phase on its own. The deadline does, and the socket is
        // given the same budget so a connection that sends nothing at all is not held for
        // the full SOCKET_READ_TIMEOUT either.
        if let Err(err) = stream.set_read_timeout(Some(HEADER_READ_DEADLINE)) {
            config.log_warn(&format!("failed to set header read timeout: {err}"));
        }
        let header_deadline = Instant::now() + HEADER_READ_DEADLINE;
        let partial = match http_parse::read_request_headers(
            &mut stream,
            config.max_upload_bytes,
            Some(header_deadline),
            std::mem::take(&mut carried_over),
        ) {
            Ok(partial) => partial,
            Err(error) => {
                if requests_served > 0 {
                    return Ok(());
                }
                let is_head = error.method.as_deref() == Some("HEAD");
                let _ = write_response(
                    &mut stream,
                    error.response,
                    ResponseWriteOptions::closing(is_head),
                );
                return Ok(());
            }
        };

        // The first request on a connection is charged from the moment the connection was
        // accepted, because everything between then and here is the server: the wait for a
        // free worker, which on a saturated server is most of what the client experiences,
        // and the header read, which the deadline bounds. Charging from here instead logged
        // an eleven-second liveness probe as zero milliseconds and reported the same in
        // `truss_http_request_duration_seconds`, which `docs/prometheus.md` calls end to
        // end. Later requests on a keep-alive connection waited for no worker, and the idle
        // time before them is not part of any request, so they are charged from here.
        let start = if requests_served == 0 {
            accepted_at
        } else {
            Instant::now()
        };

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
        // The path is already parsed here, which is what lets the limit apply to the routes
        // whose cost justifies it and leave the health probes and the metrics scrape out.
        // `/health/live` answered 429 is a failed liveness probe, so shedding it turned a
        // burst of client traffic into a restart, and a scrape shed at the moment the
        // limiter starts working is the moment the evidence of it working stops arriving.
        if let (Some(limiter), Some(ip)) = (&config.rate_limiter, client_ip)
            && is_rate_limited(partial.path())
            && !limiter.check(ip)
        {
            let mut response = too_many_requests_response("rate limit exceeded — try again later");
            response.attach_request_id(&request_id);
            // The route the request named, so an operator can see which route the 429s
            // belong to rather than finding them all under the unknown one.
            let route = classify_route_from_path(partial.path());
            record_http_metrics(route, response.status);
            let sc = status_code(response.status).unwrap_or("unknown");
            let method_log = partial.method.clone();
            let path_log = partial.path().to_string();
            let _ = write_response(
                &mut stream,
                response,
                ResponseWriteOptions::closing(is_head),
            );
            record_http_request_duration(route, start);
            emit_access_log(
                config,
                &AccessLogEntry {
                    request_id: &request_id,
                    method: &method_log,
                    path: &path_log,
                    route: route.as_label(),
                    status: sc,
                    start,
                    cache_status: None,
                    watermark: false,
                },
            );
            return Ok(());
        }

        // A body is read under the longer inactivity timeout: a legitimate upload can be
        // up to `max_upload_bytes` over a slow link, and the routes that accept one reject
        // an unauthenticated request from the headers alone, before reaching this point.
        if let Err(err) = stream.set_read_timeout(Some(SOCKET_READ_TIMEOUT)) {
            config.log_warn(&format!("failed to restore socket read timeout: {err}"));
        }

        let wants_close = client_wants_close(&partial.version, &partial.headers);

        let accepts_gzip = config.enable_compression
            && http_parse::header_value(&partial.headers, "accept-encoding")
                .is_some_and(|v| http_parse::accepts_encoding(v, "gzip"));

        let requires_auth = matches!(
            (partial.method.as_str(), partial.path()),
            ("POST", "/images:transform" | "/images")
        );
        if requires_auth
            && let Err(mut response) =
                super::auth::authorize_request_headers(&partial.headers, config)
        {
            response.attach_request_id(&request_id);
            record_http_metrics(RouteMetric::Unknown, response.status);
            let sc = status_code(response.status).unwrap_or("unknown");
            let method_log = partial.method.clone();
            let path_log = partial.path().to_string();
            let _ = write_response(
                &mut stream,
                response,
                ResponseWriteOptions {
                    close: true,
                    is_head,
                    accepts_gzip,
                    compression_level: config.compression_level,
                },
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
                    .and_then(super::auth::extract_bearer_token);
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
                response.attach_request_id(&request_id);
                record_http_metrics(RouteMetric::Metrics, response.status);
                let sc = status_code(response.status).unwrap_or("unknown");
                let method_log = partial.method.clone();
                let path_log = partial.path().to_string();
                let _ = write_response(
                    &mut stream,
                    response,
                    ResponseWriteOptions {
                        close: true,
                        is_head,
                        accepts_gzip,
                        compression_level: config.compression_level,
                    },
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

        // Early-reject /health requests when a health token is configured.
        if matches!(
            (partial.method.as_str(), partial.path()),
            ("GET" | "HEAD", "/health")
        ) && let Some(expected) = &config.health_token
        {
            let provided = http_parse::header_value(&partial.headers, "authorization")
                .and_then(super::auth::extract_bearer_token);
            let early_response = match provided {
                Some(token) if token.as_bytes().ct_eq(expected.as_bytes()).into() => None,
                _ => Some(super::response::auth_required_response(
                    "health endpoint requires authentication",
                )),
            };

            if let Some(mut response) = early_response {
                response.attach_request_id(&request_id);
                record_http_metrics(RouteMetric::Health, response.status);
                let sc = status_code(response.status).unwrap_or("unknown");
                let method_log = partial.method.clone();
                let path_log = partial.path().to_string();
                let _ = write_response(
                    &mut stream,
                    response,
                    ResponseWriteOptions {
                        close: true,
                        is_head,
                        accepts_gzip,
                        compression_level: config.compression_level,
                    },
                );
                record_http_request_duration(RouteMetric::Health, start);
                emit_access_log(
                    config,
                    &AccessLogEntry {
                        request_id: &request_id,
                        method: &method_log,
                        path: &path_log,
                        route: "/health",
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

        let (request, leftover) = match http_parse::read_request_body(&mut stream, partial) {
            Ok(pair) => pair,
            Err(mut response) => {
                response.attach_request_id(&request_id);
                record_http_metrics(RouteMetric::Unknown, response.status);
                let sc = status_code(response.status).unwrap_or("unknown");
                let _ = write_response(
                    &mut stream,
                    response,
                    ResponseWriteOptions {
                        close: true,
                        is_head,
                        accepts_gzip,
                        compression_level: config.compression_level,
                    },
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

        response.attach_request_id(&request_id);

        let cache_status = extract_cache_status(&response.headers);
        let had_watermark = extract_watermark_flag(&mut response.headers);

        let sc = status_code(response.status).unwrap_or("unknown");

        requests_served += 1;
        let close_after = wants_close || requests_served >= config.keep_alive_max_requests;

        write_response(
            &mut stream,
            response,
            ResponseWriteOptions {
                close: close_after,
                is_head,
                accepts_gzip,
                compression_level: config.compression_level,
            },
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
        carried_over = leftover;
    }
}

/// Decides whether the connection closes after this request.
///
/// Persistence is the default only from HTTP/1.1 onwards. An HTTP/1.0 client that sends no
/// `Connection` header considers the exchange finished when it has read the answer, and
/// keeping its socket open parks a worker thread on it until the header deadline expires.
/// `Connection` is a comma-separated list, so `close` counts wherever it appears in one.
fn client_wants_close(version: &str, headers: &[(String, String)]) -> bool {
    let connection = http_parse::header_value(headers, "connection");
    if connection.is_some_and(|value| http_parse::header_list_contains(value, "close")) {
        return true;
    }
    if version.eq_ignore_ascii_case("HTTP/1.1") {
        return false;
    }
    !connection.is_some_and(|value| http_parse::header_list_contains(value, "keep-alive"))
}

/// One row of the routing table.
///
/// The path, the methods it serves, the label it is counted under, and whether the rate
/// limiter is asked about it all sit together, because every one of them used to be decided
/// somewhere else and the three answers disagreed: a wrong method on a real route was a 404
/// counted as an unknown route, and the limiter, which runs before any of this, shed the
/// health probes along with the transforms.
struct Route {
    path: &'static str,
    /// The methods this route serves, in the order `Allow` lists them.
    methods: &'static [&'static str],
    metric: RouteMetric,
    /// Whether the rate limiter is consulted for this route.
    ///
    /// The transform routes are what a limit is for: each one can cost a decode, a resize
    /// and an encode. The health endpoints answer from memory or from a cached measurement
    /// and are what decides whether the process keeps running, so shedding them turns a
    /// burst of client traffic into a restart. `/metrics` is how the limiter is observed,
    /// and it has `TRUSS_METRICS_TOKEN` for access control, which is the right tool for a
    /// scrape.
    rate_limited: bool,
    handler: fn(http_parse::HttpRequest, &ServerConfig) -> HttpResponse,
}

const GET_HEAD: &[&str] = &["GET", "HEAD"];
const POST: &[&str] = &["POST"];

const ROUTES: &[Route] = &[
    Route {
        path: "/health",
        methods: GET_HEAD,
        metric: RouteMetric::Health,
        rate_limited: false,
        handler: |_request, config| handle_health(config),
    },
    Route {
        path: "/health/live",
        methods: GET_HEAD,
        metric: RouteMetric::HealthLive,
        rate_limited: false,
        handler: |_request, _config| handle_health_live(),
    },
    Route {
        path: "/health/ready",
        methods: GET_HEAD,
        metric: RouteMetric::HealthReady,
        rate_limited: false,
        handler: |_request, config| handle_health_ready(config),
    },
    Route {
        path: "/metrics",
        methods: GET_HEAD,
        metric: RouteMetric::Metrics,
        rate_limited: false,
        handler: handle_metrics_request,
    },
    Route {
        path: "/images/by-path",
        methods: GET_HEAD,
        metric: RouteMetric::PublicByPath,
        rate_limited: true,
        handler: handle_public_path_request,
    },
    Route {
        path: "/images/by-url",
        methods: GET_HEAD,
        metric: RouteMetric::PublicByUrl,
        rate_limited: true,
        handler: handle_public_url_request,
    },
    Route {
        path: "/images:transform",
        methods: POST,
        metric: RouteMetric::Transform,
        rate_limited: true,
        handler: handle_transform_request,
    },
    Route {
        path: "/images",
        methods: POST,
        metric: RouteMetric::Upload,
        rate_limited: true,
        handler: handle_upload_request,
    },
];

impl Route {
    fn lookup(path: &str) -> Option<&'static Route> {
        ROUTES.iter().find(|route| route.path == path)
    }

    fn serves(&self, method: &str) -> bool {
        self.methods.contains(&method)
    }

    /// The `Allow` header RFC 9110 section 15.5.6 requires on a 405, naming the methods this
    /// route serves. `OPTIONS` is on every route, because every route answers it.
    fn allow_header(&self) -> String {
        let mut allow = self.methods.join(", ");
        allow.push_str(", OPTIONS");
        allow
    }
}

/// Whether the rate limiter is asked about this request.
///
/// A path truss does not serve is inside the limit: that is what a scanner produces and it
/// costs nothing to shed. A wrong method on a real route follows that route's answer, since
/// the cost of refusing it is the route's own.
fn is_rate_limited(path: &str) -> bool {
    Route::lookup(path).is_none_or(|route| route.rate_limited)
}

pub(super) fn route_request(
    request: http_parse::HttpRequest,
    config: &ServerConfig,
) -> HttpResponse {
    let Some(route) = Route::lookup(request.path()) else {
        return HttpResponse::problem("404 Not Found", NOT_FOUND_BODY.as_bytes().to_vec());
    };
    // An `OPTIONS` probe is what a browser, a CDN and an uptime monitor send before they
    // fetch. truss serves no CORS headers, so `Allow` and nothing else is the whole of what
    // it has to report, which is what RFC 9110 section 9.3.7 describes for such a server.
    if request.method == "OPTIONS" {
        return HttpResponse::empty(
            "204 No Content",
            vec![("Allow".to_string(), route.allow_header())],
        );
    }
    if !route.serves(&request.method) {
        let mut response = problem_response(
            ErrorClass::MethodNotAllowed,
            &format!("{} does not serve {}", route.path, request.method),
        );
        response
            .headers
            .push(("Allow".to_string(), route.allow_header()));
        return response;
    }
    (route.handler)(request, config)
}

/// The label a request is counted under, which is the route it named whether or not the
/// method was one the route serves.
pub(super) fn classify_route(request: &http_parse::HttpRequest) -> RouteMetric {
    classify_route_from_path(request.path())
}

fn classify_route_from_path(path: &str) -> RouteMetric {
    Route::lookup(path).map_or(RouteMetric::Unknown, |route| route.metric)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which routes the rate limiter is asked about, written out so a route added later has
    /// to choose a side.
    ///
    /// The transform routes are what a limit is for. The health endpoints and the metrics
    /// scrape are outside it: a liveness probe answered 429 is a failed probe, and an
    /// orchestrator that fails enough of them restarts the process, which turns a burst of
    /// client traffic into a restart. An unknown path stays inside, since that is what a
    /// scanner produces.
    #[test]
    fn the_rate_limiter_is_asked_about_the_transform_routes_and_not_the_probes() {
        let cases: &[(&str, bool)] = &[
            ("/images/by-path", true),
            ("/images/by-url", true),
            ("/images:transform", true),
            ("/images", true),
            ("/health", false),
            ("/health/live", false),
            ("/health/ready", false),
            ("/metrics", false),
            ("/nonexistent", true),
            ("/", true),
        ];
        for &(path, expected) in cases {
            assert_eq!(is_rate_limited(path), expected, "{path}");
        }

        for route in ROUTES {
            assert_eq!(
                is_rate_limited(route.path),
                route.rate_limited,
                "{} is not asked the table's own answer",
                route.path
            );
        }
    }

    /// Every route names the methods it serves, and `Allow` reports them plus `OPTIONS`,
    /// which every route answers.
    #[test]
    fn every_route_serves_its_methods_and_allows_options() {
        for route in ROUTES {
            assert!(!route.methods.is_empty(), "{} serves nothing", route.path);
            for method in route.methods {
                assert!(route.serves(method), "{} {}", method, route.path);
            }
            assert!(
                !route.serves("BREW"),
                "{} serves a method it does not name",
                route.path
            );
            let allow = route.allow_header();
            assert!(allow.ends_with(", OPTIONS"), "{allow}");
            for method in route.methods {
                assert!(allow.contains(method), "{allow} omits {method}");
            }
        }
    }

    /// A path appears once, so a lookup cannot depend on the order of the table.
    #[test]
    fn the_routing_table_names_each_path_once() {
        let mut seen: Vec<&str> = Vec::new();
        for route in ROUTES {
            assert!(
                !seen.contains(&route.path),
                "{} is listed twice",
                route.path
            );
            seen.push(route.path);
        }
    }

    /// A connection is kept open only when the client's protocol version says persistence is
    /// the default and the client did not ask to close. HTTP/1.0 has no persistent
    /// connections unless the client opts in, and `Connection` is a comma-separated list, so
    /// `close` counts wherever it appears in it.
    #[test]
    fn the_close_decision_reads_the_version_and_the_whole_connection_list() {
        let cases: &[(&str, Option<&str>, bool)] = &[
            ("HTTP/1.1", None, false),
            ("HTTP/1.1", Some("keep-alive"), false),
            ("HTTP/1.1", Some("close"), true),
            ("HTTP/1.1", Some("Close"), true),
            ("HTTP/1.1", Some("close, TE"), true),
            ("HTTP/1.1", Some("keep-alive, close"), true),
            ("HTTP/1.1", Some("TE"), false),
            ("HTTP/1.0", None, true),
            ("HTTP/1.0", Some("keep-alive"), false),
            ("HTTP/1.0", Some("keep-alive, TE"), false),
            ("HTTP/1.0", Some("close"), true),
            ("HTTP/0.9", None, true),
            ("BANANA", None, true),
        ];

        for &(version, connection, expected) in cases {
            let headers: Vec<(String, String)> = connection
                .map(|value| vec![("connection".to_string(), value.to_string())])
                .unwrap_or_default();
            assert_eq!(
                client_wants_close(version, &headers),
                expected,
                "{version} with Connection: {connection:?}"
            );
        }
    }
}
