# Development Guide

This page covers building, testing, and contributing to truss.

## Requirements

| Item | Requirement |
|------|------|
| Rust | stable toolchain (edition 2024) |
| OS | Linux, macOS, Windows |

## Building from Source

```sh
cargo install truss-image
```

This installs the `truss` command.

To enable storage backend support, add feature flags:

```sh
# S3
cargo install truss-image --features s3

# GCS
cargo install truss-image --features gcs

# Azure Blob Storage
cargo install truss-image --features azure

# All storage backends
cargo install truss-image --features "s3,gcs,azure"
```

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

## WASM Demo

The [browser demo](https://nao1215.github.io/truss/) is a static application built from the WASM target. Images are processed locally and never leave the browser.

The Pages build intentionally enables `wasm,svg` only. If you need AVIF support or lossy WebP output in your own browser build, generate a custom artifact with `avif` and/or `webp-lossy`. See [WASM Integration](wasm.md) for the JS API contract, feature matrix, limits, and caveats.

To build the demo locally, use [`scripts/build-wasm-demo.sh`](../scripts/build-wasm-demo.sh):

```sh
rustup target add wasm32-unknown-unknown
# The wasm-bindgen-cli version must match the wasm-bindgen dependency in Cargo.toml.
cargo install wasm-bindgen-cli --version 0.2.114
./scripts/build-wasm-demo.sh
```

The build output is written to `web/dist/`.

## npm Packages

The repository contains two official npm package sources:

- [`packages/truss-wasm`](../packages/truss-wasm) for browser-side image transforms
- [`packages/truss-url-signer`](../packages/truss-url-signer) for Node.js / TypeScript signed-URL generation

Release tags build `.tgz` artifacts for both packages. Both official npm packages now publish via trusted publishing on tagged releases.
Release tags also update the Homebrew tap formula in `nao1215/homebrew-tap`; configure `HOMEBREW_TAP_GITHUB_TOKEN` before enabling that workflow path.

### WASM package

The WASM package uses a bundler-oriented build and currently ships the fixed feature set `wasm,svg,avif`.

To build and smoke-check it locally:

```sh
cat .nvmrc  # Node.js version used in CI
rustup target add wasm32-unknown-unknown
# The wasm-bindgen-cli version must match the wasm-bindgen dependency in Cargo.toml.
cargo install wasm-bindgen-cli --version 0.2.114
just wasm-package-pack
just wasm-package-consumer-smoke
just wasm-vite-example-smoke
just wasm-vite-example-runtime-smoke
```

### TypeScript URL signer package

The URL signer package is a pure ESM package with no runtime dependencies beyond Node.js.

To validate it locally:

```sh
cat .nvmrc  # Node.js version used in CI
just url-signer-package-typecheck
just url-signer-package-test
just url-signer-package-pack
```

## Benchmark

Measured with `docs/img/logo.png` (1536 x 1024 PNG, 1.6 MB) on AMD Ryzen 7 5800U. Each operation was run 10 times; the table shows min / avg / max wall-clock time.

### Conversion Speed

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

The AVIF rows predate v0.19.0, which turned on the encoder's thread pool. They are the cost of encoding on one core, and are no longer representative on a machine with cores to spare.

### Criterion Suite

`cargo bench --bench transform` runs the criterion suite in `benches/transform.rs`. Every case that touches pixels builds its own 640x427 source, so the size in a case name is the size it ran at.

Baseline on a 32-core machine, median of each case, for comparison rather than as a threshold:

| Case | Median |
|---|---|
| `format_conversion/jpeg_to_png/640x427` | 21.3 ms |
| `format_conversion/jpeg_to_webp/640x427` | 18.2 ms |
| `format_conversion/jpeg_to_avif/640x427` | 327.8 ms |
| `resize/cover/100x100` | 3.3 ms |
| `resize/cover/1920x1080` | 87.2 ms |
| `fit_modes/cover/300x300` | 6.6 ms |
| `filters/blur/sigma_5` | 8.8 ms |
| `filters/sharpen/sigma_3` | 9.7 ms |
| `watermark/bottom_right` | 21.7 ms |
| `svg/rasterize_to_png_1024w` | 639 µs |
| `sniff_artifact/sample.jpg` | 19 ns |

The AVIF case is the one that varies most with the machine, because it is the only encoder that uses more than one core.

## Contributing

Contributions are welcome. See [../CONTRIBUTING.md](../CONTRIBUTING.md) for details.

- Look for [`good first issue`](https://github.com/nao1215/truss/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) to get started.
- Report bugs and request features via [Issues](https://github.com/nao1215/truss/issues).
- If the project is useful, starring the repository helps.
- Support via [GitHub Sponsors](https://github.com/sponsors/nao1215) is also welcome.
- Sharing the project on social media or in blog posts is appreciated.
