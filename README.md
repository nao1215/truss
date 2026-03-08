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

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for details.

- Report bugs and request features via [Issues](https://github.com/nao1215/truss/issues).
- If the project is useful, starring the repository helps.
- Support via [GitHub Sponsors](https://github.com/sponsors/nao1215) is also welcome.
- Sharing the project on social media or in blog posts is appreciated.

## License

Released under the [MIT License](LICENSE).
