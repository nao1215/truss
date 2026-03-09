# truss

[![Build](https://github.com/nao1215/truss/actions/workflows/rust.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/rust.yml)
[![CLI Integration](https://github.com/nao1215/truss/actions/workflows/integration-cli.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/integration-cli.yml)
[![API Integration](https://github.com/nao1215/truss/actions/workflows/integration-api.yml/badge.svg)](https://github.com/nao1215/truss/actions/workflows/integration-api.yml)
[![Crates.io](https://img.shields.io/crates/v/truss-image)](https://crates.io/crates/truss-image)
[![Crates.io Downloads](https://img.shields.io/crates/d/truss-image)](https://crates.io/crates/truss-image)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org/)

![logo](./doc/img/logo-small.png)

truss is a library-first image toolkit that reuses the same transformation core across the CLI, HTTP server, and WASM demo. User-facing behavior is locked down with Docker-based integration tests using [ShellSpec](https://github.com/shellspec/shellspec) and [runn](https://github.com/k1LoW/runn), so the CLI and API remain aligned with executable documentation.

- Cross Platform: support Linux, macOS, Windows
- Tested contracts: the CLI is covered by [ShellSpec](https://github.com/shellspec/shellspec) and the HTTP API by [runn](https://github.com/k1LoW/runn), keeping user-visible behavior under integration test.
- Security by default: signed URLs, SSRF protections, and SVG sanitization are built in.
- Practical image pipeline: handles JPEG, PNG, WebP, AVIF, BMP, and SVG, with support for retaining metadata such as EXIF, ICC, and XMP where possible.

## Requirements

| Item | Requirement |
|------|------|
| Rust | stable toolchain (edition 2024) |
| OS | Linux, macOS, Windows |

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

## Installation

```sh
cargo install truss-image
```

This installs the `truss` command.

## Usage

### CLI

Convert image formats:

```sh
truss photo.png -o photo.jpg
```

Resize and convert:

```sh
truss photo.png -o thumb.webp --width 800 --format webp --quality 75
```

Convert from a remote URL:

```sh
truss --url https://example.com/img.png -o out.avif --format avif
```

Sanitize SVG by removing scripts and external references:

```sh
truss diagram.svg -o safe.svg
```

Rasterize SVG:

```sh
truss diagram.svg -o diagram.png --width 1024
```

Inspect image metadata:

```sh
truss inspect photo.jpg
```

Run `truss --help` to see the full set of options.

### Commands

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

### Shell Completions

Generate completion scripts with the `completions` subcommand:

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

### HTTP Server

```sh
truss serve
```

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

API reference:

- OpenAPI YAML: [doc/openapi.yaml](doc/openapi.yaml)
- Swagger UI on GitHub Pages: https://nao1215.github.io/truss/swagger/

### Docker

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
docker run -p 8080:8080 \
  -e TRUSS_BIND_ADDR=0.0.0.0:8080 \
  -e TRUSS_BEARER_TOKEN=changeme \
  -v ./images:/data:ro \
  -e TRUSS_STORAGE_ROOT=/data \
  ghcr.io/nao1215/truss:latest
```

The first GHCR package publish is private by default. To allow anonymous pulls from ECS, change the package visibility to `Public` in GitHub Packages settings once after the first publish.

### WASM

A browser demo is available on GitHub Pages:

https://nao1215.github.io/truss/

The demo is a static browser application. Selected image files are processed in the browser, are not sent to a truss backend or any other external system, and are not stored by the demo.

To build the demo locally, use [`scripts/build-wasm-demo.sh`](scripts/build-wasm-demo.sh):

```sh
rustup target add wasm32-unknown-unknown
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
- Transform: `width`, `height`, `fit`, `position`, `format`, `quality`, `background`, `rotate`, `autoOrient`, `stripMetadata`, `preserveExif`

This ensures that a cached response for one signed URL is not served to requests with different or expired signatures, and different transform options produce separate cache entries.

### `TRUSS_PUBLIC_BASE_URL`

When truss runs behind CloudFront, set `TRUSS_PUBLIC_BASE_URL` to the public CloudFront domain (e.g. `https://images.example.com`). Signed-URL verification compares the request authority against this value; a mismatch will cause signature validation to fail.

```sh
TRUSS_PUBLIC_BASE_URL=https://images.example.com truss serve
```

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

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

- Report bugs and request features via [Issues](https://github.com/nao1215/truss/issues).
- If the project is useful, starring the repository helps.
- Support via [GitHub Sponsors](https://github.com/sponsors/nao1215) is also welcome.
- Sharing the project on social media or in blog posts is appreciated.

## License

Released under the [MIT License](LICENSE).
