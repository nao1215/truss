# Prometheus Metrics

truss exposes a `/metrics` endpoint in [Prometheus text exposition format](https://prometheus.io/docs/instrumenting/exposition_formats/#text-based-format) (version 0.0.4).

## Endpoint

| Path       | Method | Authentication |
|------------|--------|----------------|
| `/metrics` | GET    | **None**       |

The endpoint does not require Bearer-token authentication so that Prometheus scrapers can collect metrics without additional configuration.

> **Security note:** The metrics endpoint exposes operational information such as request counts, error rates, and latency distributions. In production, restrict access to `/metrics` at the network level (e.g., Kubernetes NetworkPolicy, firewall rules, or reverse-proxy path restrictions) rather than exposing it to the public internet.

## Scrape Configuration

```yaml
scrape_configs:
  - job_name: truss
    scrape_interval: 15s
    static_configs:
      - targets: ["localhost:8080"]
```

## Metrics Reference

### Gauges

| Metric | Description |
|--------|-------------|
| `truss_process_up` | Always `1` when the server is running. |
| `truss_transforms_in_flight` | Number of image transforms currently executing. |
| `truss_transforms_max_concurrent` | Configured maximum concurrent transforms (`TRUSS_MAX_CONCURRENT_TRANSFORMS`). |

### Counters

| Metric | Labels | Description |
|--------|--------|-------------|
| `truss_http_requests_total` | | Total HTTP requests handled. |
| `truss_http_requests_by_route_total` | `route` | Requests broken down by route (e.g., `/health`, `/images:transform`). |
| `truss_http_responses_total` | `status` | Responses broken down by HTTP status code. |
| `truss_cache_hits_total` | | Transform cache hits. |
| `truss_cache_misses_total` | | Transform cache misses. |
| `truss_origin_cache_hits_total` | | Origin (remote URL) cache hits. |
| `truss_origin_cache_misses_total` | | Origin cache misses. |
| `truss_transform_errors_total` | `error_type` | Transform errors by category. |

#### `error_type` values

| Value | Description |
|-------|-------------|
| `invalid_input` | Structurally invalid input artifact. |
| `invalid_options` | Contradictory or unsupported transform options. |
| `unsupported_input_format` | Input media type cannot be processed. |
| `unsupported_output_format` | Requested output format cannot be produced. |
| `decode_failed` | Input decoding failed. |
| `encode_failed` | Output encoding failed. |
| `capability_missing` | Runtime lacks a required capability. |
| `limit_exceeded` | Image exceeds a processing limit (e.g., pixel count). |

### Histograms

All histograms use the following bucket boundaries (seconds):

```
0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0
```

| Metric | Labels | Description |
|--------|--------|-------------|
| `truss_http_request_duration_seconds` | `route` | End-to-end HTTP request duration. |
| `truss_transform_duration_seconds` | `format` | Image transform duration (successful transforms only). |
| `truss_storage_request_duration_seconds` | `backend` | Storage backend fetch duration. |

#### `route` values

`/health`, `/health/live`, `/health/ready`, `/images/by-path`, `/images/by-url`, `/images:transform`, `/images`, `/metrics`, `<unknown>`

#### `format` values

`jpeg`, `png`, `webp`, `avif`, `svg`, `bmp`

#### `backend` values

`filesystem`, `s3`, `gcs`, `azure`

## Example PromQL Queries

```promql
# Request rate by route (last 5 minutes)
rate(truss_http_requests_by_route_total[5m])

# p99 request latency
histogram_quantile(0.99, rate(truss_http_request_duration_seconds_bucket[5m]))

# Transform error rate
rate(truss_transform_errors_total[5m])

# Cache hit ratio
rate(truss_cache_hits_total[5m]) / (rate(truss_cache_hits_total[5m]) + rate(truss_cache_misses_total[5m]))

# Storage latency p95 by backend
histogram_quantile(0.95, rate(truss_storage_request_duration_seconds_bucket[5m]))
```
