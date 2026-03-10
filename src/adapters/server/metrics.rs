use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub(super) static HTTP_REQUESTS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_HEALTH_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_HEALTH_LIVE_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_HEALTH_READY_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_PUBLIC_BY_PATH_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_PUBLIC_BY_URL_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_TRANSFORM_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_UPLOAD_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_METRICS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_REQUESTS_UNKNOWN_TOTAL: AtomicU64 = AtomicU64::new(0);

pub(super) static HTTP_RESPONSES_200_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_400_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_401_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_403_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_404_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_406_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_413_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_415_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_500_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_501_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_502_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_503_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_508_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_OTHER_TOTAL: AtomicU64 = AtomicU64::new(0);

pub(super) static CACHE_HITS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static CACHE_MISSES_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static ORIGIN_CACHE_HITS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static ORIGIN_CACHE_MISSES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Default maximum number of concurrent image transforms allowed.
/// Configurable at runtime via `TRUSS_MAX_CONCURRENT_TRANSFORMS`.
pub(super) const DEFAULT_MAX_CONCURRENT_TRANSFORMS: u64 = 64;

/// Process start time used to compute uptime in the `/health` diagnostic endpoint.
pub(super) static START_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Returns the process uptime in seconds since the first call to this function.
pub(super) fn uptime_seconds() -> u64 {
    START_TIME.get_or_init(Instant::now).elapsed().as_secs()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteMetric {
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
    pub(super) const fn as_label(self) -> &'static str {
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

pub(super) fn record_http_metrics(route: RouteMetric, status: &str) {
    HTTP_REQUESTS_TOTAL.fetch_add(1, Ordering::Relaxed);
    route_counter(route).fetch_add(1, Ordering::Relaxed);
    status_counter(status).fetch_add(1, Ordering::Relaxed);
}

pub(super) fn render_metrics_text(max_concurrent: u64, transforms_in_flight: &AtomicU64) -> String {
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
        transforms_in_flight.load(Ordering::Relaxed)
    ));

    body.push_str(
        "# HELP truss_transforms_max_concurrent Maximum allowed concurrent transforms.\n",
    );
    body.push_str("# TYPE truss_transforms_max_concurrent gauge\n");
    body.push_str(&format!(
        "truss_transforms_max_concurrent {max_concurrent}\n"
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

pub(super) fn route_counter(route: RouteMetric) -> &'static AtomicU64 {
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

pub(super) fn status_counter(status: &str) -> &'static AtomicU64 {
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

pub(super) fn status_counter_value(status: &str) -> u64 {
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

pub(super) fn status_code(status: &str) -> Option<&str> {
    status.split_whitespace().next()
}
