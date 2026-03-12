use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::{MediaType, TransformError};

// ── Existing counters ────────────────────────────────────────────────

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
pub(super) static HTTP_RESPONSES_304_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static HTTP_RESPONSES_OTHER_TOTAL: AtomicU64 = AtomicU64::new(0);

pub(super) static CACHE_HITS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static CACHE_MISSES_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static ORIGIN_CACHE_HITS_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static ORIGIN_CACHE_MISSES_TOTAL: AtomicU64 = AtomicU64::new(0);
pub(super) static WATERMARK_TRANSFORMS_TOTAL: AtomicU64 = AtomicU64::new(0);

pub(super) static START_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub(super) fn uptime_seconds() -> u64 {
    START_TIME.get_or_init(Instant::now).elapsed().as_secs()
}

// ── RouteMetric ──────────────────────────────────────────────────────

const ROUTE_COUNT: usize = 9;

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

    const fn index(self) -> usize {
        match self {
            Self::Health => 0,
            Self::HealthLive => 1,
            Self::HealthReady => 2,
            Self::PublicByPath => 3,
            Self::PublicByUrl => 4,
            Self::Transform => 5,
            Self::Upload => 6,
            Self::Metrics => 7,
            Self::Unknown => 8,
        }
    }

    const ALL: [Self; ROUTE_COUNT] = [
        Self::Health,
        Self::HealthLive,
        Self::HealthReady,
        Self::PublicByPath,
        Self::PublicByUrl,
        Self::Transform,
        Self::Upload,
        Self::Metrics,
        Self::Unknown,
    ];
}

// ── AtomicHistogram ──────────────────────────────────────────────────

const BUCKET_COUNT: usize = 12;
const BUCKET_BOUNDS: [f64; BUCKET_COUNT] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

pub(super) struct AtomicHistogram {
    buckets: [AtomicU64; BUCKET_COUNT],
    count: AtomicU64,
    sum_bits: AtomicU64,
}

impl AtomicHistogram {
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU64 = AtomicU64::new(0);

    const fn new() -> Self {
        Self {
            buckets: [Self::ZERO; BUCKET_COUNT],
            count: AtomicU64::new(0),
            sum_bits: AtomicU64::new(0),
        }
    }

    pub(super) fn observe(&self, duration: Duration) {
        let value = duration.as_secs_f64();
        self.count.fetch_add(1, Ordering::Relaxed);

        // CAS loop to add f64 value to sum stored as u64 bits.
        loop {
            let old_bits = self.sum_bits.load(Ordering::Relaxed);
            let new_bits = (f64::from_bits(old_bits) + value).to_bits();
            if self
                .sum_bits
                .compare_exchange_weak(old_bits, new_bits, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            std::hint::spin_loop();
        }

        // Cumulative histogram: increment all buckets where value <= bound.
        for (i, &bound) in BUCKET_BOUNDS.iter().enumerate() {
            if value <= bound {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn snapshot(&self) -> HistogramSnapshot {
        let mut bucket_values = [0u64; BUCKET_COUNT];
        for (i, b) in self.buckets.iter().enumerate() {
            bucket_values[i] = b.load(Ordering::Relaxed);
        }
        HistogramSnapshot {
            buckets: bucket_values,
            count: self.count.load(Ordering::Relaxed),
            sum: f64::from_bits(self.sum_bits.load(Ordering::Relaxed)),
        }
    }
}

struct HistogramSnapshot {
    buckets: [u64; BUCKET_COUNT],
    count: u64,
    sum: f64,
}

impl HistogramSnapshot {
    fn render(&self, name: &str, label_key: &str, label_value: &str, buf: &mut String) {
        for (i, &bound) in BUCKET_BOUNDS.iter().enumerate() {
            let _ = writeln!(
                buf,
                "{name}_bucket{{{label_key}=\"{label_value}\",le=\"{bound}\"}} {}",
                self.buckets[i]
            );
        }
        let _ = writeln!(
            buf,
            "{name}_bucket{{{label_key}=\"{label_value}\",le=\"+Inf\"}} {}",
            self.count
        );
        let _ = writeln!(
            buf,
            "{name}_sum{{{label_key}=\"{label_value}\"}} {}",
            self.sum
        );
        let _ = writeln!(
            buf,
            "{name}_count{{{label_key}=\"{label_value}\"}} {}",
            self.count
        );
    }
}

// ── Transform error type ─────────────────────────────────────────────

const TRANSFORM_ERROR_COUNT: usize = 8;
const TRANSFORM_ERROR_LABELS: [&str; TRANSFORM_ERROR_COUNT] = [
    "invalid_input",
    "invalid_options",
    "unsupported_input_format",
    "unsupported_output_format",
    "decode_failed",
    "encode_failed",
    "capability_missing",
    "limit_exceeded",
];

fn transform_error_index(error: &TransformError) -> usize {
    match error {
        TransformError::InvalidInput(_) => 0,
        TransformError::InvalidOptions(_) => 1,
        TransformError::UnsupportedInputMediaType(_) => 2,
        TransformError::UnsupportedOutputMediaType(_) => 3,
        TransformError::DecodeFailed(_) => 4,
        TransformError::EncodeFailed(_) => 5,
        TransformError::CapabilityMissing(_) => 6,
        TransformError::LimitExceeded(_) => 7,
    }
}

// ── MediaType index ──────────────────────────────────────────────────

const MEDIA_TYPE_COUNT: usize = 7;
const MEDIA_TYPE_LABELS: [&str; MEDIA_TYPE_COUNT] =
    ["jpeg", "png", "webp", "avif", "svg", "bmp", "tiff"];

fn media_type_index(mt: MediaType) -> usize {
    match mt {
        MediaType::Jpeg => 0,
        MediaType::Png => 1,
        MediaType::Webp => 2,
        MediaType::Avif => 3,
        MediaType::Svg => 4,
        MediaType::Bmp => 5,
        MediaType::Tiff => 6,
    }
}

// ── Storage backend index ────────────────────────────────────────────

const STORAGE_BACKEND_COUNT: usize = 4;
const STORAGE_BACKEND_LABELS: [&str; STORAGE_BACKEND_COUNT] = ["filesystem", "s3", "gcs", "azure"];

pub(super) fn storage_backend_index_from_config(backend: &super::StorageBackendLabel) -> usize {
    match backend {
        super::StorageBackendLabel::Filesystem => 0,
        super::StorageBackendLabel::S3 => 1,
        super::StorageBackendLabel::Gcs => 2,
        super::StorageBackendLabel::Azure => 3,
    }
}

// ── Static histogram/counter instances ───────────────────────────────

#[allow(clippy::declare_interior_mutable_const)]
const HISTOGRAM_INIT: AtomicHistogram = AtomicHistogram::new();

static HTTP_REQUEST_DURATION: [AtomicHistogram; ROUTE_COUNT] = [HISTOGRAM_INIT; ROUTE_COUNT];
static TRANSFORM_DURATION: [AtomicHistogram; MEDIA_TYPE_COUNT] = [HISTOGRAM_INIT; MEDIA_TYPE_COUNT];
static STORAGE_DURATION: [AtomicHistogram; STORAGE_BACKEND_COUNT] =
    [HISTOGRAM_INIT; STORAGE_BACKEND_COUNT];

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_COUNTER: AtomicU64 = AtomicU64::new(0);
static TRANSFORM_ERRORS: [AtomicU64; TRANSFORM_ERROR_COUNT] = [ZERO_COUNTER; TRANSFORM_ERROR_COUNT];

// ── Public recording functions ───────────────────────────────────────

pub(super) fn record_http_metrics(route: RouteMetric, status: &str) {
    HTTP_REQUESTS_TOTAL.fetch_add(1, Ordering::Relaxed);
    route_counter(route).fetch_add(1, Ordering::Relaxed);
    status_counter(status).fetch_add(1, Ordering::Relaxed);
}

pub(super) fn record_http_request_duration(route: RouteMetric, start: Instant) {
    HTTP_REQUEST_DURATION[route.index()].observe(start.elapsed());
}

pub(super) fn record_transform_duration(output_format: MediaType, start: Instant) {
    TRANSFORM_DURATION[media_type_index(output_format)].observe(start.elapsed());
}

pub(super) fn record_transform_error(error: &TransformError) {
    TRANSFORM_ERRORS[transform_error_index(error)].fetch_add(1, Ordering::Relaxed);
}

pub(super) fn record_storage_duration(backend_index: usize, start: Instant) {
    STORAGE_DURATION[backend_index].observe(start.elapsed());
}

pub(super) fn record_watermark_transform() {
    WATERMARK_TRANSFORMS_TOTAL.fetch_add(1, Ordering::Relaxed);
}

// ── Render ────────────────────────────────────────────────────────────

pub(super) fn render_metrics_text(max_concurrent: u64, transforms_in_flight: &AtomicU64) -> String {
    let mut body = String::with_capacity(8192);

    body.push_str(
        "# HELP truss_process_up Whether the server adapter considers the process alive.\n",
    );
    body.push_str("# TYPE truss_process_up gauge\n");
    body.push_str("truss_process_up 1\n");

    body.push_str(
        "# HELP truss_transforms_in_flight Number of image transforms currently executing.\n",
    );
    body.push_str("# TYPE truss_transforms_in_flight gauge\n");
    let _ = writeln!(
        body,
        "truss_transforms_in_flight {}",
        transforms_in_flight.load(Ordering::Relaxed)
    );

    body.push_str(
        "# HELP truss_transforms_max_concurrent Maximum allowed concurrent transforms.\n",
    );
    body.push_str("# TYPE truss_transforms_max_concurrent gauge\n");
    let _ = writeln!(body, "truss_transforms_max_concurrent {max_concurrent}");

    body.push_str(
        "# HELP truss_http_requests_total Total parsed HTTP requests handled by the server adapter.\n",
    );
    body.push_str("# TYPE truss_http_requests_total counter\n");
    let _ = writeln!(
        body,
        "truss_http_requests_total {}",
        HTTP_REQUESTS_TOTAL.load(Ordering::Relaxed)
    );

    body.push_str(
        "# HELP truss_http_requests_by_route_total Total parsed HTTP requests handled by route.\n",
    );
    body.push_str("# TYPE truss_http_requests_by_route_total counter\n");
    for route in RouteMetric::ALL {
        let _ = writeln!(
            body,
            "truss_http_requests_by_route_total{{route=\"{}\"}} {}",
            route.as_label(),
            route_counter(route).load(Ordering::Relaxed)
        );
    }

    body.push_str("# HELP truss_cache_hits_total Total transform cache hits.\n");
    body.push_str("# TYPE truss_cache_hits_total counter\n");
    let _ = writeln!(
        body,
        "truss_cache_hits_total {}",
        CACHE_HITS_TOTAL.load(Ordering::Relaxed)
    );

    body.push_str("# HELP truss_cache_misses_total Total transform cache misses.\n");
    body.push_str("# TYPE truss_cache_misses_total counter\n");
    let _ = writeln!(
        body,
        "truss_cache_misses_total {}",
        CACHE_MISSES_TOTAL.load(Ordering::Relaxed)
    );

    body.push_str("# HELP truss_origin_cache_hits_total Total origin response cache hits.\n");
    body.push_str("# TYPE truss_origin_cache_hits_total counter\n");
    let _ = writeln!(
        body,
        "truss_origin_cache_hits_total {}",
        ORIGIN_CACHE_HITS_TOTAL.load(Ordering::Relaxed)
    );

    body.push_str("# HELP truss_origin_cache_misses_total Total origin response cache misses.\n");
    body.push_str("# TYPE truss_origin_cache_misses_total counter\n");
    let _ = writeln!(
        body,
        "truss_origin_cache_misses_total {}",
        ORIGIN_CACHE_MISSES_TOTAL.load(Ordering::Relaxed)
    );

    body.push_str(
        "# HELP truss_watermark_transforms_total Total transforms that included a watermark.\n",
    );
    body.push_str("# TYPE truss_watermark_transforms_total counter\n");
    let _ = writeln!(
        body,
        "truss_watermark_transforms_total {}",
        WATERMARK_TRANSFORMS_TOTAL.load(Ordering::Relaxed)
    );

    body.push_str(
        "# HELP truss_http_responses_total Total HTTP responses emitted by status code.\n",
    );
    body.push_str("# TYPE truss_http_responses_total counter\n");
    for status in [
        "200", "304", "400", "401", "403", "404", "406", "413", "415", "500", "501", "502", "503",
        "508", "other",
    ] {
        let _ = writeln!(
            body,
            "truss_http_responses_total{{status=\"{status}\"}} {}",
            status_counter_value(status)
        );
    }

    // ── Histograms ───────────────────────────────────────────────────

    body.push_str("# HELP truss_http_request_duration_seconds HTTP request duration in seconds.\n");
    body.push_str("# TYPE truss_http_request_duration_seconds histogram\n");
    for route in RouteMetric::ALL {
        HTTP_REQUEST_DURATION[route.index()].snapshot().render(
            "truss_http_request_duration_seconds",
            "route",
            route.as_label(),
            &mut body,
        );
    }

    body.push_str("# HELP truss_transform_duration_seconds Image transform duration in seconds.\n");
    body.push_str("# TYPE truss_transform_duration_seconds histogram\n");
    for (i, &label) in MEDIA_TYPE_LABELS.iter().enumerate() {
        TRANSFORM_DURATION[i].snapshot().render(
            "truss_transform_duration_seconds",
            "format",
            label,
            &mut body,
        );
    }

    body.push_str(
        "# HELP truss_storage_request_duration_seconds Storage backend request duration in seconds.\n",
    );
    body.push_str("# TYPE truss_storage_request_duration_seconds histogram\n");
    for (i, &label) in STORAGE_BACKEND_LABELS.iter().enumerate() {
        STORAGE_DURATION[i].snapshot().render(
            "truss_storage_request_duration_seconds",
            "backend",
            label,
            &mut body,
        );
    }

    // ── Transform error counter ──────────────────────────────────────

    body.push_str("# HELP truss_transform_errors_total Total transform errors by error type.\n");
    body.push_str("# TYPE truss_transform_errors_total counter\n");
    for (i, &label) in TRANSFORM_ERROR_LABELS.iter().enumerate() {
        let _ = writeln!(
            body,
            "truss_transform_errors_total{{error_type=\"{label}\"}} {}",
            TRANSFORM_ERRORS[i].load(Ordering::Relaxed)
        );
    }

    body
}

// ── Existing helpers ─────────────────────────────────────────────────

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
        Some("304") => &HTTP_RESPONSES_304_TOTAL,
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
        "304" => HTTP_RESPONSES_304_TOTAL.load(Ordering::Relaxed),
        "508" => HTTP_RESPONSES_508_TOTAL.load(Ordering::Relaxed),
        _ => HTTP_RESPONSES_OTHER_TOTAL.load(Ordering::Relaxed),
    }
}

pub(super) fn status_code(status: &str) -> Option<&str> {
    status.split_whitespace().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_observe_populates_cumulative_buckets() {
        let h = AtomicHistogram::new();
        // 50ms should fall into le=0.05 and all higher buckets
        h.observe(Duration::from_millis(50));
        let snap = h.snapshot();

        assert_eq!(snap.count, 1);
        assert!(snap.sum > 0.049 && snap.sum < 0.06);

        // le=0.005..0.025 should be 0 (50ms > those bounds)
        assert_eq!(snap.buckets[0], 0); // le=0.005
        assert_eq!(snap.buckets[1], 0); // le=0.01
        assert_eq!(snap.buckets[2], 0); // le=0.025
        // le=0.05 and above should be 1
        assert_eq!(snap.buckets[3], 1); // le=0.05
        assert_eq!(snap.buckets[4], 1); // le=0.1
        assert_eq!(snap.buckets[11], 1); // le=30.0
    }

    #[test]
    fn histogram_multiple_observations_accumulate() {
        let h = AtomicHistogram::new();
        h.observe(Duration::from_millis(1)); // 0.001s -> le=0.005+
        h.observe(Duration::from_millis(100)); // 0.1s -> le=0.1+
        h.observe(Duration::from_secs(5)); // 5.0s -> le=5.0+
        let snap = h.snapshot();

        assert_eq!(snap.count, 3);
        assert!(snap.sum > 5.1 && snap.sum < 5.2);

        // le=0.005: 1 (only 1ms observation)
        assert_eq!(snap.buckets[0], 1);
        // le=0.1: 2 (1ms + 100ms)
        assert_eq!(snap.buckets[4], 2);
        // le=5.0: 3 (all three)
        assert_eq!(snap.buckets[9], 3);
        // le=10.0: 3
        assert_eq!(snap.buckets[10], 3);
    }

    #[test]
    fn histogram_render_produces_valid_prometheus_format() {
        let h = AtomicHistogram::new();
        h.observe(Duration::from_millis(100));
        let snap = h.snapshot();

        let mut buf = String::new();
        snap.render("test_metric", "route", "/health", &mut buf);

        assert!(buf.contains("test_metric_bucket{route=\"/health\",le=\"0.005\"} 0"));
        assert!(buf.contains("test_metric_bucket{route=\"/health\",le=\"0.1\"} 1"));
        assert!(buf.contains("test_metric_bucket{route=\"/health\",le=\"+Inf\"} 1"));
        assert!(buf.contains("test_metric_sum{route=\"/health\"}"));
        assert!(buf.contains("test_metric_count{route=\"/health\"} 1"));
    }

    #[test]
    fn histogram_zero_state_renders_all_zeros() {
        let h = AtomicHistogram::new();
        let snap = h.snapshot();

        let mut buf = String::new();
        snap.render("empty", "k", "v", &mut buf);

        assert!(buf.contains("empty_bucket{k=\"v\",le=\"+Inf\"} 0"));
        assert!(buf.contains("empty_sum{k=\"v\"} 0"));
        assert!(buf.contains("empty_count{k=\"v\"} 0"));
    }

    #[test]
    fn transform_error_index_covers_all_variants() {
        // Ensures the match is exhaustive and each variant gets a unique index.
        let errors = [
            TransformError::InvalidInput(String::new()),
            TransformError::InvalidOptions(String::new()),
            TransformError::UnsupportedInputMediaType(String::new()),
            TransformError::UnsupportedOutputMediaType(MediaType::Png),
            TransformError::DecodeFailed(String::new()),
            TransformError::EncodeFailed(String::new()),
            TransformError::CapabilityMissing(String::new()),
            TransformError::LimitExceeded(String::new()),
        ];
        let mut seen = std::collections::HashSet::new();
        for (i, error) in errors.iter().enumerate() {
            let idx = transform_error_index(error);
            assert_eq!(idx, i, "index mismatch for variant {i}");
            assert!(seen.insert(idx), "duplicate index {idx}");
        }
    }

    #[test]
    fn record_transform_error_increments_correct_counter() {
        let before = TRANSFORM_ERRORS[4].load(Ordering::Relaxed);
        record_transform_error(&TransformError::DecodeFailed("test".into()));
        let after = TRANSFORM_ERRORS[4].load(Ordering::Relaxed);
        assert_eq!(after - before, 1);
    }

    #[test]
    fn render_metrics_text_contains_all_metric_families() {
        let inflight = AtomicU64::new(0);
        let output = render_metrics_text(64, &inflight);

        let expected_types = [
            "# TYPE truss_process_up gauge",
            "# TYPE truss_transforms_in_flight gauge",
            "# TYPE truss_transforms_max_concurrent gauge",
            "# TYPE truss_http_requests_total counter",
            "# TYPE truss_http_requests_by_route_total counter",
            "# TYPE truss_cache_hits_total counter",
            "# TYPE truss_cache_misses_total counter",
            "# TYPE truss_origin_cache_hits_total counter",
            "# TYPE truss_origin_cache_misses_total counter",
            "# TYPE truss_http_responses_total counter",
            "# TYPE truss_http_request_duration_seconds histogram",
            "# TYPE truss_transform_duration_seconds histogram",
            "# TYPE truss_storage_request_duration_seconds histogram",
            "# TYPE truss_transform_errors_total counter",
        ];
        for expected in expected_types {
            assert!(output.contains(expected), "missing metric type: {expected}");
        }
    }
}
