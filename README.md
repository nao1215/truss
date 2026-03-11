# truss

[![Build](https://github.com/nao1215/truss/actions/workflows/rust.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/rust.yml)
[![CLI Integration](https://github.com/nao1215/truss/actions/workflows/integration-cli.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/integration-cli.yml)
[![API Integration](https://github.com/nao1215/truss/actions/workflows/integration-api.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/integration-api.yml)
[![Crates.io](https://img.shields.io/crates/v/truss-image)](https://crates.io/crates/truss-image)
[![Crates.io Downloads](https://img.shields.io/crates/d/truss-image)](https://crates.io/crates/truss-image)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org/)

![logo](./doc/img/logo-small.png)

Resize, convert, blur, and watermark images from the CLI, an HTTP server, or the browser -- written in Rust with signed-URL authentication and SSRF protection built in.

[Try the WASM demo in your browser](https://nao1215.github.io/truss/) -- no install, no upload, runs 100 % client-side.

![WASM demo screenshot](./doc/img/wasm-sample.png)

## Why truss?

- **One binary, three interfaces** -- the same Rust core powers the CLI, an HTTP image-transform server, and a WASM browser demo.
- **Security by default** -- signed URLs, SSRF protections, and SVG sanitization are built in.
- **Broad format support** -- JPEG, PNG, WebP, AVIF, BMP, and SVG; retains EXIF, ICC, and XMP metadata where possible.
- **Cross-platform** -- Linux, macOS, Windows.
- **Tested contracts** -- CLI behavior is locked by [ShellSpec](https://github.com/shellspec/shellspec), HTTP API by [runn](https://github.com/k1LoW/runn).

## Comparison

Feature comparison with [imgproxy](https://github.com/imgproxy/imgproxy) and [imagor](https://github.com/cshum/imagor) as of March 2026.

| Feature | truss | imgproxy | imagor |
|---------|:-----:|:--------:|:------:|
| Language | Rust | Go | Go |
| Runtime dependencies | None | libvips (C) | libvips (C) |
| CLI | Yes | No | No |
| WASM browser demo | Yes | No | No |
| Signed URLs | Yes | Yes | Yes |
| JPEG / PNG / WebP / AVIF | Yes | Yes | Yes |
| SVG sanitization | Yes | Yes | No |
| S3 / GCS | Yes | Yes | Yes |
| Azure Blob Storage | Yes | Yes | No |
| Watermark | Yes | Yes | Yes |
| Prometheus metrics | Yes | Yes | No |
| License | MIT | MIT | Apache 2.0 |

## Architecture

```mermaid
flowchart TB
    CLI["CLI<br/>(truss convert)"] --> Core
    Server["HTTP Server<br/>(truss serve)"] --> Core
    WASM["WASM<br/>(browser)"] --> Core

    subgraph Core["Shared Rust core"]
        direction LR
        Sniff["Detect format"] --> Transform["Resize / blur / watermark"]
        Transform --> Encode["Encode output"]
    end

    Server --> Storage

    subgraph Storage["Storage backends"]
        FS["Local filesystem"]
        S3["S3"]
        GCS["GCS"]
        Azure["Azure Blob"]
    end
```

CLI reads local files or fetches remote URLs directly. The HTTP server resolves images from storage backends or client uploads. The WASM build processes files selected in the browser.

## Installation

### Prebuilt binaries

Download a prebuilt binary from the [GitHub Releases](https://github.com/nao1215/truss/releases) page. Archives and SHA256 checksums are published for each release.

| Target | Archive |
|--------|---------|
| Linux x86_64 | `truss-v*-x86_64-unknown-linux-gnu.tar.gz` |
| Linux aarch64 | `truss-v*-aarch64-unknown-linux-gnu.tar.gz` |
| macOS x86_64 | `truss-v*-x86_64-apple-darwin.tar.gz` |
| macOS aarch64 (Apple Silicon) | `truss-v*-aarch64-apple-darwin.tar.gz` |
| Windows x86_64 | `truss-v*-x86_64-pc-windows-msvc.zip` |

### From source

```sh
cargo install truss-image
```

To enable S3 storage backend support, add `--features s3`:

```sh
cargo install truss-image --features s3
```

To enable Google Cloud Storage (GCS) backend support, add `--features gcs`:

```sh
cargo install truss-image --features gcs
```

To enable Azure Blob Storage backend support, add `--features azure`:

```sh
cargo install truss-image --features azure
```

To enable all storage backends:

```sh
cargo install truss-image --features "s3,gcs,azure"
```

This installs the `truss` command.

## Quick Start

### CLI

Run `truss --help` to see the full set of options.

```sh
# Convert format
truss photo.png -o photo.jpg

# Resize + convert
truss photo.png -o thumb.webp --width 800 --format webp --quality 75

# Convert from a remote URL
truss --url https://example.com/img.png -o out.avif --format avif

# Sanitize SVG (remove scripts and external references)
truss diagram.svg -o safe.svg

# Rasterize SVG
truss diagram.svg -o diagram.png --width 1024

# Inspect metadata
truss inspect photo.jpg
```

#### Filter: Before / After

| | Original | Gaussian Blur (`--blur 5.0`) | Watermark (`--watermark`) |
|---|---|---|---|
| | ![original](./doc/img/sample-bee.jpg) | ![blurred](./doc/img/sample-bee-blurred.jpg) | ![watermarked](./doc/img/sample-bee-watermarked.jpg) |

```sh
# Blur
truss photo.jpg -o blurred.jpg --blur 5.0

# Watermark
truss photo.jpg -o watermarked.jpg \
  --watermark logo.png --watermark-position bottom-right \
  --watermark-opacity 50 --watermark-margin 10
```



### HTTP Server -- one curl to transform

```sh
# Start the server
docker run -p 8080:8080 \
  -e TRUSS_BIND_ADDR=0.0.0.0:8080 \
  -e TRUSS_BEARER_TOKEN=changeme \
  -v ./images:/data:ro \
  -e TRUSS_STORAGE_ROOT=/data \
  ghcr.io/nao1215/truss:latest

# Resize a local image to 400 px wide WebP in one request
curl -X POST http://localhost:8080/images:transform \
  -H "Authorization: Bearer changeme" \
  -F "file=@photo.jpg" \
  -F 'options={"format":"webp","width":400}' \
  -o thumb.webp

# Signed public URL (no Bearer token needed)
truss sign --base-url http://localhost:8080 \
  --path photos/hero.jpg --key-id mykey --secret s3cret \
  --expires 1700000000 --width 800 --format webp
# => http://localhost:8080/images/by-path?path=photos/hero.jpg&width=800&format=webp&keyId=mykey&expires=1700000000&signature=...
```

## Commands

| Command | Description |
|---------|-------------|
| `convert` | Convert and transform an image file |
| `inspect` | Show metadata (format, dimensions, alpha) of an image |
| `serve` | Start the HTTP image-transform server |
| `sign` | Generate a signed public URL for the server |
| `completions` | Generate shell completion scripts |
| `version` | Print version information |
| `help` | Show help for a command (e.g. `truss help convert`) |

The `convert` subcommand can be omitted: `truss photo.png -o photo.jpg` is equivalent to `truss convert photo.png -o photo.jpg`. Similarly, server flags at the top level imply `serve`: `truss --bind 0.0.0.0:8080` is equivalent to `truss serve --bind 0.0.0.0:8080`.

## Supported Formats

| Input \ Output | JPEG | PNG | WebP | AVIF | BMP | SVG |
|-------------|:----:|:---:|:----:|:----:|:---:|:---:|
| JPEG        | Yes  | Yes | Yes  | Yes  | Yes | -   |
| PNG         | Yes  | Yes | Yes  | Yes  | Yes | -   |
| WebP        | Yes  | Yes | Yes  | Yes  | Yes | -   |
| AVIF        | Yes  | Yes | Yes  | Yes  | Yes | -   |
| BMP         | Yes  | Yes | Yes  | Yes  | Yes | -   |
| SVG         | Yes  | Yes | Yes  | Yes  | Yes | Yes |

SVG to SVG performs sanitization only, removing scripts and external references.

## HTTP Server

By default, the server listens on `127.0.0.1:8080`. Configuration can be supplied through environment variables or CLI flags.

```sh
truss serve --bind 0.0.0.0:8080 --storage-root /var/images
```

Key environment variables:

| Variable | Description |
|------|------|
| `TRUSS_BIND_ADDR` | Bind address (default: `127.0.0.1:8080`) |
| `TRUSS_STORAGE_ROOT` | Root directory for local image sources |
| `TRUSS_BEARER_TOKEN` | Bearer token for private endpoints |
| `TRUSS_PUBLIC_BASE_URL` | External base URL for signed-URL authority (for reverse proxy / CDN setups) |
| `TRUSS_SIGNED_URL_KEY_ID` | Key ID for signed public URLs |
| `TRUSS_SIGNED_URL_SECRET` | Shared secret for signed public URLs |
| `TRUSS_ALLOW_INSECURE_URL_SOURCES` | Allow private-network/loopback URL sources (`true`/`1`; dev/test only) |
| `TRUSS_CACHE_ROOT` | Directory for the transform cache; caching is disabled when unset |
| `TRUSS_PUBLIC_MAX_AGE` | `Cache-Control: max-age` for public GET responses in seconds (default: `3600`) |
| `TRUSS_PUBLIC_STALE_WHILE_REVALIDATE` | `Cache-Control: stale-while-revalidate` for public GET responses in seconds (default: `60`) |
| `TRUSS_DISABLE_ACCEPT_NEGOTIATION` | Disable Accept-based content negotiation (`true`/`1`; recommended behind CDNs that don't forward Accept) |
| `TRUSS_STORAGE_BACKEND` | Storage backend for public `GET /images/by-path`: `filesystem` (default), `s3`, `gcs`, or `azure`. Only one backend can be active at a time. When set to `s3`, `gcs`, or `azure`, the `path` query parameter is used as the object key. Private endpoints can still use `kind: storage` regardless of this setting. |
| `TRUSS_S3_BUCKET` | Default S3 bucket name (required when backend is `s3`) |
| `TRUSS_S3_FORCE_PATH_STYLE` | Use path-style S3 addressing (`true`/`1`; required for MinIO, LocalStack, etc.) |
| `AWS_REGION` | AWS region for the S3 client (e.g. `us-east-1`) |
| `AWS_ACCESS_KEY_ID` | AWS access key for S3 authentication |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key for S3 authentication |
| `AWS_ENDPOINT_URL` | Custom S3-compatible endpoint URL (e.g. `http://minio:9000` for MinIO) |
| `TRUSS_GCS_BUCKET` | Default GCS bucket name (required when backend is `gcs`) |
| `TRUSS_GCS_ENDPOINT` | Custom GCS endpoint URL (e.g. `http://fake-gcs:4443` for fake-gcs-server) |
| `GOOGLE_APPLICATION_CREDENTIALS` | Path to GCS service account JSON key file |
| `GOOGLE_APPLICATION_CREDENTIALS_JSON` | Inline GCS service account JSON (alternative to file path) |
| `TRUSS_AZURE_CONTAINER` | Default Azure Blob Storage container name (required when backend is `azure`) |
| `TRUSS_AZURE_ENDPOINT` | Custom Azure Blob endpoint URL (e.g. `http://azurite:10000/devstoreaccount1` for Azurite) |
| `AZURE_STORAGE_ACCOUNT_NAME` | Azure storage account name (used to derive the default endpoint when `TRUSS_AZURE_ENDPOINT` is not set; must be 3-24 lowercase alphanumeric characters) |
| `TRUSS_MAX_CONCURRENT_TRANSFORMS` | Maximum concurrent image transforms; requests exceeding this limit receive 503 (default: `64`, range: 1–1024). Start conservative (e.g. #CPUs × 2) and increase until CPU/memory saturation or 503s appear. |
| `TRUSS_TRANSFORM_DEADLINE_SECS` | Per-transform wall-clock deadline in seconds (default: `30`, range: 1–300). Set slightly above your p95 transform latency but below any upstream/client timeout to avoid cascading failures. |
| `TRUSS_STORAGE_TIMEOUT_SECS` | Download timeout in seconds for object storage backends (default: `30`, range: 1–300) |

### Structured Access Logs

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

Azure authentication: By default, truss uses anonymous access, which works for public containers and Azurite local development. For private containers, append a SAS token to `TRUSS_AZURE_ENDPOINT`. On Azure-hosted compute (App Service, AKS, VMs), managed identity is used automatically when no explicit credentials are provided.

### Prometheus Metrics

The server exposes a `/metrics` endpoint in Prometheus text exposition format. The endpoint does not require authentication, so Prometheus scrapers can collect metrics without additional configuration.

For the full metrics reference, bucket boundaries, and example PromQL queries, see [doc/prometheus.md](doc/prometheus.md).

API reference:

- OpenAPI YAML: [doc/openapi.yaml](doc/openapi.yaml)
- Swagger UI on GitHub Pages: https://nao1215.github.io/truss/swagger/

## Docker

```sh
docker compose up
```

This starts the server from `compose.yml`. The default configuration mounts `./images` as the storage root.

To build and run it directly:

```sh
docker build -t truss .
docker run -p 8080:8080 \
  -e TRUSS_BIND_ADDR=0.0.0.0:8080 \
  -e TRUSS_BEARER_TOKEN=changeme \
  -v ./images:/data:ro \
  -e TRUSS_STORAGE_ROOT=/data \
  truss
```

Prebuilt container images are published to GHCR:

```sh
docker pull ghcr.io/nao1215/truss:latest
```

The first GHCR package publish is private by default. To allow anonymous pulls from ECS, change the package visibility to `Public` in GitHub Packages settings once after the first publish.

## WASM Demo

A browser demo is available on GitHub Pages:

https://nao1215.github.io/truss/

The demo is a static browser application built from the WASM target. Images are processed locally and never leave the browser.

To build the demo locally, use [`scripts/build-wasm-demo.sh`](scripts/build-wasm-demo.sh):

```sh
rustup target add wasm32-unknown-unknown
# The wasm-bindgen-cli version must match the wasm-bindgen dependency in Cargo.toml.
cargo install wasm-bindgen-cli --version 0.2.114
./scripts/build-wasm-demo.sh
```

The build output is written to `web/dist/`.

## Shell Completions

```sh
# Bash
truss completions bash > ~/.local/share/bash-completion/completions/truss

# Zsh (add ~/.zfunc to your fpath)
truss completions zsh > ~/.zfunc/_truss

# Fish
truss completions fish > ~/.config/fish/completions/truss.fish

# PowerShell
truss completions powershell > truss.ps1
```

## CDN / Reverse-Proxy Integration

truss is an image transformation origin, not a CDN itself. In production, place a CDN such as CloudFront (or a reverse proxy like nginx / Envoy) in front of truss so that transformed images are cached at the edge.

```mermaid
flowchart LR
    Viewer -->|HTTPS request| CloudFront
    CloudFront -->|cache hit| Viewer
    CloudFront -->|cache miss| ALB["ALB / nginx / Envoy"]
    ALB --> truss
    truss -->|read source| Storage["Local storage<br/>or remote URL origin"]
```

- CloudFront is the cache layer. It serves cached responses directly on cache hits.
- truss is the origin API. Image transformation runs on truss, not on CloudFront.
- An ALB or reverse proxy is recommended between CloudFront and truss because truss does not handle TLS termination or large-scale traffic on its own.
- The truss on-disk cache (`TRUSS_CACHE_ROOT`) is a single-node auxiliary cache that reduces redundant transforms on the origin; it is not a replacement for the CDN cache.

### Public vs. Private Endpoints

Only the public GET endpoints should be exposed through CloudFront:

| Endpoint | Visibility | CloudFront |
|----------|-----------|------------|
| `GET /images/by-path` | Public (signed URL) | Origin for CDN |
| `GET /images/by-url` | Public (signed URL) | Origin for CDN |
| `POST /images:transform` | Private (Bearer token) | Do not expose |
| `POST /images` | Private (Bearer token) | Do not expose |

### CDN Cache Key Configuration

CDN cache keys must vary by the signed-URL authentication inputs and any transform query parameters used by the public GET endpoints (`GET /images/by-path`, `GET /images/by-url`). Configure your CDN / CloudFront Cache Policy to include the following query string parameters in the cache key (or use a policy that forwards all query strings):

- Authentication: `keyId`, `expires`, `signature`
- Source: `path` or `url`, `version`
- Transform: `width`, `height`, `fit`, `position`, `format`, `quality`, `background`, `rotate`, `autoOrient`, `stripMetadata`, `preserveExif`, `blur`

This ensures that a cached response for one signed URL is not served to requests with different or expired signatures, and different transform options produce separate cache entries.

### `TRUSS_PUBLIC_BASE_URL`

When truss runs behind CloudFront, set `TRUSS_PUBLIC_BASE_URL` to the public CloudFront domain (e.g. `https://images.example.com`). Signed-URL verification compares the request authority against this value; a mismatch will cause signature validation to fail.

```sh
TRUSS_PUBLIC_BASE_URL=https://images.example.com truss serve
```

## Requirements

| Item | Requirement |
|------|------|
| Rust | stable toolchain (edition 2024) |
| OS | Linux, macOS, Windows |

## Benchmark

Measured with `doc/img/logo.png` (1536 x 1024 PNG, 1.6 MB) on AMD Ryzen 7 5800U. Each operation was run 10 times; the table shows min / avg / max wall-clock time.

### Conversion speed

| Operation | Avg | Min | Max |
|---|---|---|---|
| PNG → JPEG | 60 ms | 58 ms | 73 ms |
| PNG → WebP | 46 ms | 45 ms | 50 ms |
| PNG → AVIF | 6 956 ms | 6 427 ms | 8 092 ms |
| PNG → BMP | 40 ms | 38 ms | 42 ms |
| Resize 800w + JPEG | 69 ms | 67 ms | 75 ms |
| Resize 400w + WebP | 46 ms | 44 ms | 51 ms |
| Resize 200w + AVIF | 190 ms | 185 ms | 205 ms |
| Resize 500x500 cover + JPEG | 64 ms | 63 ms | 66 ms |
| JPEG quality 50 | 54 ms | 53 ms | 61 ms |
| Inspect metadata | 5 ms | 5 ms | 6 ms |

### Output file size

| Output | Size |
|---|---|
| PNG → JPEG | 124 KB |
| PNG → WebP | 1.2 MB |
| PNG → AVIF | 32 KB |
| PNG → BMP | 6.1 MB |
| Resize 800w → JPEG | 44 KB |
| Resize 400w → WebP | 108 KB |
| Resize 200w → AVIF | 4.0 KB |

## Roadmap

See the [public roadmap](https://github.com/nao1215/truss/issues?q=is%3Aissue+label%3Aroadmap) for planned features and milestones.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

- Look for [`good first issue`](https://github.com/nao1215/truss/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) to get started.
- Report bugs and request features via [Issues](https://github.com/nao1215/truss/issues).
- If the project is useful, starring the repository helps.
- Support via [GitHub Sponsors](https://github.com/sponsors/nao1215) is also welcome.
- Sharing the project on social media or in blog posts is appreciated.

## License

Released under the [MIT License](LICENSE).
