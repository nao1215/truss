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

To build the demo locally, use [`scripts/build-wasm-demo.sh`](../scripts/build-wasm-demo.sh):

```sh
rustup target add wasm32-unknown-unknown
# The wasm-bindgen-cli version must match the wasm-bindgen dependency in Cargo.toml.
cargo install wasm-bindgen-cli --version 0.2.114
./scripts/build-wasm-demo.sh
```

The build output is written to `web/dist/`.

## Benchmark

Measured with `doc/img/logo.png` (1536 x 1024 PNG, 1.6 MB) on AMD Ryzen 7 5800U. Each operation was run 10 times; the table shows min / avg / max wall-clock time.

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

## Contributing

Contributions are welcome. See [../CONTRIBUTING.md](../CONTRIBUTING.md) for details.

- Look for [`good first issue`](https://github.com/nao1215/truss/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) to get started.
- Report bugs and request features via [Issues](https://github.com/nao1215/truss/issues).
- If the project is useful, starring the repository helps.
- Support via [GitHub Sponsors](https://github.com/sponsors/nao1215) is also welcome.
- Sharing the project on social media or in blog posts is appreciated.
