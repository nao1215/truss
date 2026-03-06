## Contributing to truss

Thank you for building truss with us.
Every report, patch, test, and review directly improves the image toolkit for Rust developers.
Let's keep truss fast, safe, and reliable together.

## Prerequisites

- **Rust stable** toolchain (`rustup default stable`)
- **[just](https://github.com/casey/just)** task runner (`cargo install just`)
- **Docker** (for integration tests and container builds)

Optional:

- `cargo-audit` for security checks (`cargo install cargo-audit`)
- `cargo-llvm-cov` + `llvm-tools-preview` for coverage (`just setup` installs these)
- `wasm32-unknown-unknown` target and `wasm-bindgen-cli` for WASM builds

Run `just setup` to install the optional development tools.

## Quick Start

```sh
git clone https://github.com/nao1215/truss.git
cd truss
just test        # Run unit tests and doc tests
just lint        # Run clippy
just fmt-check   # Check formatting
```

## Contributing as a Developer

### 1. Start with clear communication

- **Bug report**: Use the issue template and include reproducible steps, expected behavior, and actual behavior.
- **New feature**: Open an issue first so we can agree on direction before implementation.
- **Bug fix or improvement**: Open a PR with a clear problem statement and solution summary.

### 2. Keep the quality bar high

- Add or update **unit tests** when you add features or fix bugs.
- Add **doc tests** for new or changed public APIs.
- Keep CLI behavior and error messages clear and consistent.
- Follow the structured error format: `error:`, `usage:`, `hint:`.

### 3. Run checks before opening a PR

The fastest way is `just ci`, which runs all checks at once:

```sh
just ci
```

This is equivalent to:

```sh
just test       # cargo test --all-targets && cargo test --doc
just lint       # cargo clippy --all-targets -- -D warnings
just fmt-check  # cargo fmt --all -- --check
just doc        # RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
just audit      # cargo audit
```

### 4. Run integration tests

Integration tests use Docker and are separate from `cargo test`:

```sh
just integration-cli   # CLI tests with ShellSpec
just integration-api   # API server tests with runn
just integration       # Both
```

- **CLI tests** (`integration/cli/`): Written in [ShellSpec](https://github.com/shellspec/shellspec). These specs also serve as user-facing CLI documentation.
- **API tests** (`integration/api/`): Written as [runn](https://github.com/k1LoW/runn) runbooks. These runbooks also serve as API documentation.
- **Test fixtures**: Shared images in `integration/fixtures/`.

### 5. Code style

- Follow standard Rust conventions and idioms.
- Write documentation comments (`///`) for public APIs.
- Write comments in English.
- Keep `unsafe` code to an absolute minimum and document the safety invariants.
- GIF support is out of scope; do not add or reintroduce it.

### 6. Project structure

```
src/
├── main.rs                  # Entry point
├── lib.rs                   # Public API exports
├── core.rs                  # Core types, validation, media sniffing
├── adapters/
│   ├── cli.rs               # CLI parser and command execution
│   ├── server.rs            # HTTP server, caching, signed URLs
│   └── wasm.rs              # Browser WASM adapter
└── codecs/
    ├── raster.rs            # JPEG, PNG, WebP, AVIF, BMP codec
    └── svg.rs               # SVG sanitization and rasterization

integration/
├── fixtures/                # Shared test images
├── cli/                     # ShellSpec CLI specs
│   ├── Dockerfile
│   └── spec/*.sh
└── api/                     # runn API runbooks
    ├── compose.yml
    └── runbooks/*.yml

doc/                         # Design documents and specs
```

### 7. Available `just` recipes

Run `just` with no arguments to see the full list. Key recipes:

| Recipe | Description |
|--------|-------------|
| `just test` | Unit + integration + doc tests |
| `just test-unit` | Unit tests only (fast) |
| `just lint` | Clippy with strict warnings |
| `just fmt` | Format all code |
| `just ci` | All CI checks locally |
| `just coverage` | Code coverage summary |
| `just integration` | Docker-based CLI + API tests |
| `just serve` | Start dev server |
| `just docker-build` | Build Docker image |

### 8. Exit codes

The CLI uses structured exit codes. When changing CLI error handling, keep these consistent:

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Usage error (bad arguments) |
| 2 | I/O error (file not found, network failure) |
| 3 | Input error (unsupported format, corrupt file) |
| 4 | Transform error (encode failure, size limit) |

## Contributing Outside of Coding

You can still make a huge impact even if you are not writing code:

- Give truss a GitHub Star
- Share truss with your team and community
- Open issues with clear reproduction steps
- Sponsor the project
