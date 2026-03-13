# Configuration Reference

truss is configured through environment variables and CLI flags. This page documents every available setting.

## Core Settings

| Variable | Description |
|------|------|
| `TRUSS_BIND_ADDR` | Bind address (default: `127.0.0.1:8080`) |
| `TRUSS_STORAGE_ROOT` | Root directory for local image sources |
| `TRUSS_BEARER_TOKEN` | Bearer token for private endpoints |
| `TRUSS_STORAGE_BACKEND` | `filesystem` (default), `s3`, `gcs`, or `azure` |
| `TRUSS_MAX_CONCURRENT_TRANSFORMS` | Max concurrent transforms; excess requests receive 503 (default: `64`, range: 1-1024) |
| `TRUSS_TRANSFORM_DEADLINE_SECS` | Per-transform deadline in seconds (default: `30`, range: 1-300) |
| `TRUSS_MAX_INPUT_PIXELS` | Max input image pixels before decode; excess images receive 422 (default: `40000000`, range: 1-100000000) |
| `TRUSS_MAX_UPLOAD_BYTES` | Max upload body size in bytes; excess requests receive 413 (default: `104857600` = 100 MB, range: 1-10737418240) |
| `TRUSS_STORAGE_TIMEOUT_SECS` | Download timeout for object storage backends in seconds (default: `30`, range: 1-300) |
| `TRUSS_KEEP_ALIVE_MAX_REQUESTS` | Max requests per keep-alive connection before the server closes it (default: `100`, range: 1-100000) |
| `TRUSS_HEALTH_CACHE_MIN_FREE_BYTES` | Minimum free bytes on cache disk; `/health/ready` returns 503 when breached (disabled by default) |
| `TRUSS_HEALTH_MAX_MEMORY_BYTES` | Maximum process RSS in bytes; `/health/ready` returns 503 when breached (disabled by default, Linux only) |
| `TRUSS_HEALTH_HYSTERESIS_MARGIN` | Recovery margin for readiness probe hysteresis (default: `0.05`, range: 0.01-0.50). After a threshold is breached, the value must recover past threshold ± margin before the check returns to ok |
| `TRUSS_SHUTDOWN_DRAIN_SECS` | Drain period in seconds during graceful shutdown; `/health/ready` returns 503 immediately (default: `10`, range: 0-300). Total shutdown time is drain + 15 s worker drain. On Kubernetes, set `terminationGracePeriodSeconds` >= drain + 20 (e.g. `35` for the default 10 s drain) |
| `TRUSS_RESPONSE_HEADERS` | JSON object of custom headers added to all image responses including private transforms (e.g. `{"CDN-Cache-Control":"max-age=86400"}`). Framing / hop-by-hop headers (`Content-Length`, `Transfer-Encoding`, `Content-Encoding`, `Content-Type`, `Connection`, etc.) are rejected at startup. Header names must be valid RFC 7230 tokens; values must contain only visible ASCII, SP, or HTAB (CRLF is rejected) |
| `TRUSS_DISABLE_COMPRESSION` | Disable gzip compression for non-image responses (`true`/`1`/`yes`/`on`, case-insensitive). When compression is enabled (default), `Vary: Accept-Encoding` is added to compressible responses |
| `TRUSS_COMPRESSION_LEVEL` | Gzip compression level (default: `1`, range: 0-9). `1` is fastest, `6` is a good trade-off, `9` is best compression |
| `TRUSS_MAX_SOURCE_BYTES` | Max source image size in bytes from filesystem or remote URL (default: `104857600` = 100 MB, range: 1-10737418240) |
| `TRUSS_MAX_WATERMARK_BYTES` | Max watermark image size in bytes from remote URL (default: `10485760` = 10 MB, range: 1-1073741824) |
| `TRUSS_MAX_REMOTE_REDIRECTS` | Max HTTP redirects to follow when fetching a remote URL (default: `5`, range: 0-20) |
| `TRUSS_LOG_LEVEL` | Log verbosity level (default: `info`, options: `error`, `warn`, `info`, `debug`). On Unix, send `SIGUSR1` to cycle the level at runtime without restarting (`info -> debug -> error -> warn -> info`) |

`TRUSS_STORAGE_BACKEND` selects the source for public `GET /images/by-path`. When set to `s3`, `gcs`, or `azure`, the `path` query parameter is used as the object key. Only one backend can be active at a time. Private endpoints can still use `kind: storage` regardless of this setting.

## Signed URLs and Caching

| Variable | Description |
|------|------|
| `TRUSS_PUBLIC_BASE_URL` | External base URL for signed-URL authority (for reverse proxy / CDN setups) |
| `TRUSS_SIGNING_KEYS` | JSON object mapping key IDs to secrets for signed URLs (e.g. `{"k1":"secret1","k2":"secret2"}`). Supports key rotation by accepting multiple keys simultaneously. |
| `TRUSS_SIGNED_URL_KEY_ID` | Key ID for signed public URLs (legacy; merged into `TRUSS_SIGNING_KEYS` at startup) |
| `TRUSS_SIGNED_URL_SECRET` | Shared secret for signed public URLs (legacy; merged into `TRUSS_SIGNING_KEYS` at startup) |
| `TRUSS_CACHE_ROOT` | Directory for the transform cache; caching is disabled when unset |
| `TRUSS_PUBLIC_MAX_AGE` | `Cache-Control: max-age` for public GET responses in seconds (default: `3600`) |
| `TRUSS_PUBLIC_STALE_WHILE_REVALIDATE` | `Cache-Control: stale-while-revalidate` for public GET responses in seconds (default: `60`) |
| `TRUSS_DISABLE_ACCEPT_NEGOTIATION` | Disable Accept-based content negotiation (`true`/`1`; recommended behind CDNs that don't forward Accept) |
| `TRUSS_ALLOW_INSECURE_URL_SOURCES` | Allow private-network/loopback URL sources (`true`/`1`; dev/test only) |
| `TRUSS_PRESETS_FILE` | Path to a JSON file defining named transform presets. The file is watched for changes every 5 seconds; valid updates are applied without restart, invalid files are ignored (previous presets are kept) |
| `TRUSS_PRESETS` | Inline JSON defining named transform presets (ignored when `TRUSS_PRESETS_FILE` is set) |

Preset objects accept the same fields as the HTTP `ImageTransformOptions` schema, including `optimize` and `targetQuality`.

## S3

| Variable | Description |
|------|------|
| `TRUSS_S3_BUCKET` | Default S3 bucket name (required when backend is `s3`) |
| `TRUSS_S3_FORCE_PATH_STYLE` | Use path-style S3 addressing (`true`/`1`; required for MinIO, LocalStack, etc.) |
| `AWS_REGION` | AWS region for the S3 client (e.g. `us-east-1`) |
| `AWS_ACCESS_KEY_ID` | AWS access key for S3 authentication |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key for S3 authentication |
| `AWS_ENDPOINT_URL` | Custom S3-compatible endpoint URL (e.g. `http://minio:9000` for MinIO) |

## GCS

| Variable | Description |
|------|------|
| `TRUSS_GCS_BUCKET` | Default GCS bucket name (required when backend is `gcs`) |
| `TRUSS_GCS_ENDPOINT` | Custom GCS endpoint URL (e.g. `http://fake-gcs:4443` for fake-gcs-server) |
| `GOOGLE_APPLICATION_CREDENTIALS` | Path to GCS service account JSON key file |
| `GOOGLE_APPLICATION_CREDENTIALS_JSON` | Inline GCS service account JSON (alternative to file path) |

## Azure Blob Storage

| Variable | Description |
|------|------|
| `TRUSS_AZURE_CONTAINER` | Default container name (required when backend is `azure`) |
| `TRUSS_AZURE_ENDPOINT` | Custom endpoint URL (e.g. `http://azurite:10000/devstoreaccount1` for Azurite) |
| `AZURE_STORAGE_ACCOUNT_NAME` | Storage account name (3-24 lowercase alphanumeric; used to derive the default endpoint when `TRUSS_AZURE_ENDPOINT` is not set) |

By default, truss uses anonymous access, which works for public containers and Azurite local development. For private containers, append a SAS token to `TRUSS_AZURE_ENDPOINT`. On Azure-hosted compute (App Service, AKS, VMs), managed identity is used automatically when no explicit credentials are provided.

## Prometheus Metrics

The server exposes a `/metrics` endpoint in Prometheus text exposition format. By default, the endpoint does not require authentication.

| Variable | Description |
|------|------|
| `TRUSS_METRICS_TOKEN` | Bearer token for `/metrics`; when set, requests must include `Authorization: Bearer <token>` |
| `TRUSS_DISABLE_METRICS` | Disable the `/metrics` endpoint entirely (`true`/`1`; returns 404) |
| `TRUSS_HEALTH_TOKEN` | Bearer token for `/health`; when set, requests must include `Authorization: Bearer <token>`. `/health/live` and `/health/ready` remain unauthenticated |

For the full metrics reference, bucket boundaries, and example PromQL queries, see [../doc/prometheus.md](../doc/prometheus.md).

## Structured Access Logs

Every request emits a JSON access log line through the server's log handler (stderr by default). Each entry includes a unique request ID for end-to-end correlation.

```json
{"kind":"access_log","request_id":"a1b2c3d4-...","method":"GET","path":"/images/by-path","route":"/images/by-path","status":"200","latency_ms":42,"cache_status":"hit"}
```

| Field | Description |
|-------|-------------|
| `kind` | Always `"access_log"` -- distinguishes access logs from diagnostic messages |
| `request_id` | UUID v4 generated per request, or the incoming `X-Request-Id` header value when present |
| `method` | HTTP method (`GET`, `POST`, `HEAD`) |
| `path` | Request path without query string |
| `route` | Matched route label (e.g. `/images/by-path`, `/images:transform`) |
| `status` | HTTP status code as a string |
| `latency_ms` | Total request processing time in milliseconds |
| `cache_status` | `"hit"`, `"miss"`, or `null` (for non-transform endpoints) |

The server echoes the request ID back in the `X-Request-Id` response header, making it easy to correlate client-side logs with server-side entries. To propagate your own trace context, send an `X-Request-Id` header with your request and the server will reuse it.
