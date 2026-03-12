# truss

[![Build](https://github.com/nao1215/truss/actions/workflows/rust.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/rust.yml)
[![CLI Integration](https://github.com/nao1215/truss/actions/workflows/integration-cli.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/integration-cli.yml)
[![API Integration](https://github.com/nao1215/truss/actions/workflows/integration.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/integration.yml)
[![Crates.io](https://img.shields.io/crates/v/truss-image)](https://crates.io/crates/truss-image)
[![Crates.io Downloads](https://img.shields.io/crates/d/truss-image)](https://crates.io/crates/truss-image)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org/)

![logo](./doc/img/logo-small.png)

Resize, crop, convert, blur, sharpen, and watermark images from the CLI, an HTTP server, or the browser -- written in Rust with signed-URL authentication and SSRF protection built in.

[Try the WASM demo in your browser](https://nao1215.github.io/truss/) -- no install, no upload, runs 100 % client-side.

![WASM demo screenshot](./doc/img/wasm-sample.png)

## Why truss?

- One binary, three interfaces -- the same Rust core powers the CLI, an HTTP image-transform server, and a WASM browser demo.
- Security by default -- signed URLs, SSRF protections, and SVG sanitization are built in.
- Broad format support -- JPEG, PNG, WebP, AVIF, BMP, and SVG; retains EXIF, ICC, and XMP metadata where possible.
- Cross-platform -- Linux, macOS, Windows.
- Tested contracts -- CLI behavior is locked by [ShellSpec](https://github.com/shellspec/shellspec), HTTP API by [runn](https://github.com/k1LoW/runn).

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
| JPEG XL (JXL) | No | Input only | Yes |
| TIFF | Yes | Yes | Yes |
| GIF animation processing | No (out of scope) | Yes | Yes |
| SVG sanitization | Yes | Yes | No |
| Smart crop | No | Yes | Yes |
| Sharpen filter | Yes | Yes | Yes |
| Crop / Trim / Padding | Yes | Yes | Yes |
| S3  | Yes | Yes | Yes |
| GCS | Yes | Yes | Yes |
| Azure Blob Storage | Yes | Yes | No |
| Watermark | Yes | Yes | Yes |
| Prometheus metrics | Yes | Yes | Yes |
| License | MIT | MIT | Apache 2.0 |

## Architecture

```mermaid
flowchart TB
    CLI["CLI<br/>(truss convert)"] --> Core
    Server["HTTP Server<br/>(truss serve)"] --> Core
    WASM["WASM<br/>(browser)"] --> Core

    subgraph Core["Shared Rust core"]
        direction LR
        Sniff["Detect format"] --> Transform["Crop / resize / blur / sharpen / watermark"]
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

### Prebuilt binaries

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

## Quick Start

### CLI

The `convert` subcommand can be omitted: `truss photo.png -o photo.jpg` is equivalent to `truss convert photo.png -o photo.jpg`. Run `truss --help` to see the full set of options.

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

#### Examples: Blur & Watermark

| | Original | Gaussian Blur (`--blur 5.0`) | Watermark (`--watermark`) |
|---|---|---|---|
| | ![original](./doc/img/sample-bee.jpg) | ![blurred](./doc/img/sample-bee-blurred.jpg) | ![watermarked](./doc/img/sample-bee-watermarked.jpg) |

```sh
# Blur
truss photo.jpg -o blurred.jpg --blur 5.0

# Sharpen
truss photo.jpg -o sharpened.jpg --sharpen 2.0

# Watermark
truss photo.jpg -o watermarked.jpg \
  --watermark logo.png --watermark-position bottom-right \
  --watermark-opacity 50 --watermark-margin 10
```

### HTTP Server -- one curl to transform

```sh
# Start the server (binary)
truss serve --bind 0.0.0.0:8080 --storage-root ./images --bearer-token changeme

# Resize a local image to 400 px wide WebP in one request
curl -X POST http://localhost:8080/images:transform \
  -H "Authorization: Bearer changeme" \
  -F "file=@photo.jpg" \
  -F 'options={"format":"webp","width":400}' \
  -o thumb.webp

# Signed public URL (no Bearer token needed)
truss sign --base-url http://localhost:8080 \
  --path photos/hero.jpg --key-id mykey --secret s3cret \
  --expires 1900000000 --width 800 --format webp  # Unix timestamp (2030-03-17)
# => http://localhost:8080/images/by-path?path=photos/hero.jpg&width=800&format=webp&keyId=mykey&expires=1900000000&signature=...
```

See the [Docker](#docker) section for running with Docker instead.

## Commands

| Command | Description |
|---------|-------------|
| `convert` | Convert and transform an image file (can be omitted; see above) |
| `inspect` | Show metadata (format, dimensions, alpha) of an image |
| `serve` | Start the HTTP image-transform server (implied when server flags are used at the top level) |
| `sign` | Generate a signed public URL for the server |
| `completions` | Generate shell completion scripts |
| `version` | Print version information |
| `help` | Show help for a command (e.g. `truss help convert`) |

### Shell Completions

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

## Supported Formats

| Input \ Output | JPEG | PNG | WebP | AVIF | BMP | TIFF | SVG |
|-------------|:----:|:---:|:----:|:----:|:---:|:----:|:---:|
| JPEG        | Yes  | Yes | Yes  | Yes  | Yes | Yes  | -   |
| PNG         | Yes  | Yes | Yes  | Yes  | Yes | Yes  | -   |
| WebP        | Yes  | Yes | Yes  | Yes  | Yes | Yes  | -   |
| AVIF        | Yes  | Yes | Yes  | Yes  | Yes | Yes  | -   |
| BMP         | Yes  | Yes | Yes  | Yes  | Yes | Yes  | -   |
| TIFF        | Yes  | Yes | Yes  | Yes  | Yes | Yes  | -   |
| SVG         | Yes  | Yes | Yes  | Yes  | Yes | Yes  | Yes |

SVG to SVG performs sanitization only, removing scripts and external references.

## HTTP Server

By default, the server listens on `127.0.0.1:8080`. Configuration can be supplied through environment variables or CLI flags.

```sh
truss serve --bind 0.0.0.0:8080 --storage-root /var/images
```

### Core settings

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

`TRUSS_STORAGE_BACKEND` selects the source for public `GET /images/by-path`. When set to `s3`, `gcs`, or `azure`, the `path` query parameter is used as the object key. Only one backend can be active at a time. Private endpoints can still use `kind: storage` regardless of this setting.

### Signed URLs & caching

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
| `TRUSS_PRESETS_FILE` | Path to a JSON file defining named transform presets |
| `TRUSS_PRESETS` | Inline JSON defining named transform presets (ignored when `TRUSS_PRESETS_FILE` is set) |

### S3

| Variable | Description |
|------|------|
| `TRUSS_S3_BUCKET` | Default S3 bucket name (required when backend is `s3`) |
| `TRUSS_S3_FORCE_PATH_STYLE` | Use path-style S3 addressing (`true`/`1`; required for MinIO, LocalStack, etc.) |
| `AWS_REGION` | AWS region for the S3 client (e.g. `us-east-1`) |
| `AWS_ACCESS_KEY_ID` | AWS access key for S3 authentication |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key for S3 authentication |
| `AWS_ENDPOINT_URL` | Custom S3-compatible endpoint URL (e.g. `http://minio:9000` for MinIO) |

### GCS

| Variable | Description |
|------|------|
| `TRUSS_GCS_BUCKET` | Default GCS bucket name (required when backend is `gcs`) |
| `TRUSS_GCS_ENDPOINT` | Custom GCS endpoint URL (e.g. `http://fake-gcs:4443` for fake-gcs-server) |
| `GOOGLE_APPLICATION_CREDENTIALS` | Path to GCS service account JSON key file |
| `GOOGLE_APPLICATION_CREDENTIALS_JSON` | Inline GCS service account JSON (alternative to file path) |

### Azure Blob Storage

| Variable | Description |
|------|------|
| `TRUSS_AZURE_CONTAINER` | Default container name (required when backend is `azure`) |
| `TRUSS_AZURE_ENDPOINT` | Custom endpoint URL (e.g. `http://azurite:10000/devstoreaccount1` for Azurite) |
| `AZURE_STORAGE_ACCOUNT_NAME` | Storage account name (3-24 lowercase alphanumeric; used to derive the default endpoint when `TRUSS_AZURE_ENDPOINT` is not set) |

By default, truss uses anonymous access, which works for public containers and Azurite local development. For private containers, append a SAS token to `TRUSS_AZURE_ENDPOINT`. On Azure-hosted compute (App Service, AKS, VMs), managed identity is used automatically when no explicit credentials are provided.

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

### Prometheus Metrics

The server exposes a `/metrics` endpoint in Prometheus text exposition format. By default, the endpoint does not require authentication.

| Variable | Description |
|------|------|
| `TRUSS_METRICS_TOKEN` | Bearer token for `/metrics`; when set, requests must include `Authorization: Bearer <token>` |
| `TRUSS_DISABLE_METRICS` | Disable the `/metrics` endpoint entirely (`true`/`1`; returns 404) |

For the full metrics reference, bucket boundaries, and example PromQL queries, see [doc/prometheus.md](doc/prometheus.md).

### API Reference

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

## WASM Demo

The [browser demo](https://nao1215.github.io/truss/) is a static application built from the WASM target. Images are processed locally and never leave the browser.

To build the demo locally, use [`scripts/build-wasm-demo.sh`](scripts/build-wasm-demo.sh):

```sh
rustup target add wasm32-unknown-unknown
# The wasm-bindgen-cli version must match the wasm-bindgen dependency in Cargo.toml.
cargo install wasm-bindgen-cli --version 0.2.114
./scripts/build-wasm-demo.sh
```

The build output is written to `web/dist/`.

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
- Transform: `width`, `height`, `fit`, `position`, `format`, `quality`, `background`, `rotate`, `autoOrient`, `stripMetadata`, `preserveExif`, `crop`, `blur`, `sharpen`, `preset`

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
| PNG -> JPEG | 60 ms | 58 ms | 73 ms |
| PNG -> WebP | 46 ms | 45 ms | 50 ms |
| PNG -> AVIF | 6 956 ms | 6 427 ms | 8 092 ms |
| PNG -> BMP | 40 ms | 38 ms | 42 ms |
| Resize 800w + JPEG | 69 ms | 67 ms | 75 ms |
| Resize 400w + WebP | 46 ms | 44 ms | 51 ms |
| Resize 200w + AVIF | 190 ms | 185 ms | 205 ms |
| Resize 500x500 cover + JPEG | 64 ms | 63 ms | 66 ms |
| JPEG quality 50 | 54 ms | 53 ms | 61 ms |
| Inspect metadata | 5 ms | 5 ms | 6 ms |

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
