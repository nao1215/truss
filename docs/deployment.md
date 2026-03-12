# Deployment Guide

This page covers Docker setup, prebuilt binaries, cloud storage backends, and production deployment considerations for truss.

## Prebuilt Binaries

Download a prebuilt binary from the [GitHub Releases](https://github.com/nao1215/truss/releases) page. Archives and SHA256 checksums are published for each release.

| Target | Archive |
|--------|---------|
| Linux x86_64 | `truss-v*-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `truss-v*-aarch64-unknown-linux-gnu.tar.gz` |
| macOS x86_64 | `truss-v*-x86_64-apple-darwin.tar.gz` |
| macOS aarch64 (Apple Silicon) | `truss-v*-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `truss-v*-x86_64-pc-windows-msvc.zip` |

Example (Linux x86_64):

```sh
tar xzf truss-v*.tar.gz
sudo mv truss /usr/local/bin/
```

## Docker

### Docker Compose

```sh
docker compose up
```

This starts the server from `compose.yml`. The default configuration mounts `./images` as the storage root.

### Building and Running Directly

```sh
docker build -t truss .
docker run -p 8080:8080 \
  -e TRUSS_BIND_ADDR=0.0.0.0:8080 \
  -e TRUSS_BEARER_TOKEN=changeme \
  -v ./images:/data:ro \
  -e TRUSS_STORAGE_ROOT=/data \
  truss
```

### Prebuilt Container Images

Prebuilt container images are published to GHCR:

```sh
docker pull ghcr.io/nao1215/truss:latest
```

## Storage Backends

truss supports multiple storage backends. The backend is selected via `TRUSS_STORAGE_BACKEND`. Only one backend can be active at a time. See the [Configuration Reference](configuration.md) for all storage-related environment variables.

### Installing with Storage Backend Support

```sh
# S3
cargo install truss-image --features s3

# Google Cloud Storage
cargo install truss-image --features gcs

# Azure Blob Storage
cargo install truss-image --features azure

# All storage backends
cargo install truss-image --features "s3,gcs,azure"
```

### S3

Set `TRUSS_STORAGE_BACKEND=s3` and configure:

| Variable | Description |
|------|------|
| `TRUSS_S3_BUCKET` | Default S3 bucket name (required) |
| `TRUSS_S3_FORCE_PATH_STYLE` | Use path-style addressing (`true`/`1`; required for MinIO, LocalStack, etc.) |
| `AWS_REGION` | AWS region (e.g. `us-east-1`) |
| `AWS_ACCESS_KEY_ID` | AWS access key |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key |
| `AWS_ENDPOINT_URL` | Custom S3-compatible endpoint (e.g. `http://minio:9000`) |

### GCS

Set `TRUSS_STORAGE_BACKEND=gcs` and configure:

| Variable | Description |
|------|------|
| `TRUSS_GCS_BUCKET` | Default GCS bucket name (required) |
| `TRUSS_GCS_ENDPOINT` | Custom endpoint (e.g. `http://fake-gcs:4443`) |
| `GOOGLE_APPLICATION_CREDENTIALS` | Path to service account JSON key file |
| `GOOGLE_APPLICATION_CREDENTIALS_JSON` | Inline service account JSON (alternative to file path) |

### Azure Blob Storage

Set `TRUSS_STORAGE_BACKEND=azure` and configure:

| Variable | Description |
|------|------|
| `TRUSS_AZURE_CONTAINER` | Default container name (required) |
| `TRUSS_AZURE_ENDPOINT` | Custom endpoint (e.g. `http://azurite:10000/devstoreaccount1`) |
| `AZURE_STORAGE_ACCOUNT_NAME` | Storage account name (3-24 lowercase alphanumeric) |

By default, truss uses anonymous access, which works for public containers and Azurite local development. For private containers, append a SAS token to `TRUSS_AZURE_ENDPOINT`. On Azure-hosted compute (App Service, AKS, VMs), managed identity is used automatically when no explicit credentials are provided.

## CDN / Reverse-Proxy Integration

In production, place a CDN such as CloudFront (or a reverse proxy like nginx / Envoy) in front of truss so that transformed images are cached at the edge. See the [API Reference](api-reference.md#cdn--reverse-proxy-integration) for detailed CDN configuration guidance, including cache key setup and public vs. private endpoint visibility.

## Graceful Shutdown

truss supports graceful shutdown for zero-downtime deployments. The `TRUSS_SHUTDOWN_DRAIN_SECS` variable controls the drain period (default: 10 seconds). During this period, `/health/ready` returns 503 immediately so that load balancers stop sending new traffic, while in-flight requests complete.

On Kubernetes, set `terminationGracePeriodSeconds` >= drain + 20 (e.g. `35` for the default 10 s drain).

## Health Checks

| Endpoint | Purpose |
|----------|---------|
| `GET /health/live` | Liveness probe (always returns 200) |
| `GET /health/ready` | Readiness probe (returns 503 when draining, disk full, or memory limit exceeded) |

Configure health thresholds with:
- `TRUSS_HEALTH_CACHE_MIN_FREE_BYTES` -- minimum free bytes on cache disk
- `TRUSS_HEALTH_MAX_MEMORY_BYTES` -- maximum process RSS (Linux only)
