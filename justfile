# truss — image transformation tool and server
#
# Run `just` with no arguments to see available recipes.
# Run `just --list` for a compact overview.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Default recipe: show available tasks
default:
    @just --list

# ---------------------------------------------------------------------------
# Development
# ---------------------------------------------------------------------------

# Run unit tests and doc tests (use `just integration` for Docker-based tests)
test:
    cargo test --all-targets
    cargo test --doc

# Run only unit tests (fast, no integration)
test-unit:
    cargo test --lib

# Run clippy linter with strict warnings
lint:
    cargo clippy --all-targets -- -D warnings

# Check code formatting (does not modify files)
fmt-check:
    cargo fmt --all -- --check

# Format all source files
fmt:
    cargo fmt --all

# Build documentation with strict warnings
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Run security audit (requires cargo-audit)
audit:
    cargo audit

# Run all CI checks locally (test + lint + fmt + doc + audit)
ci: test lint fmt-check doc audit

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

# Build debug binary
build:
    cargo build

# Build release binary
build-release:
    cargo build --release --locked

# Build Docker image
docker-build:
    docker build -t truss .

# ---------------------------------------------------------------------------
# Coverage
# ---------------------------------------------------------------------------

# Run code coverage (requires cargo-llvm-cov)
coverage:
    cargo llvm-cov --workspace --all-targets --summary-only

# Run code coverage with HTML report
coverage-html:
    cargo llvm-cov --workspace --all-targets --html
    @echo "Report: target/llvm-cov/html/index.html"

# ---------------------------------------------------------------------------
# Integration tests (Docker-based)
# ---------------------------------------------------------------------------

# Run CLI integration tests with ShellSpec in Docker
integration-cli:
    docker build -t truss-cli-test -f integration/cli/Dockerfile .
    docker run --rm truss-cli-test

# Run API server integration tests with runn in Docker Compose
integration-api:
    cd integration/api && docker compose up --build --abort-on-container-exit --exit-code-from runn

# Clean up API integration test containers
integration-api-clean:
    cd integration/api && docker compose down --volumes --remove-orphans

# Run S3 backend integration tests (runn → nginx → truss → s3mock)
integration-s3:
    cd integration/s3 && docker compose up --build --abort-on-container-exit --exit-code-from runn

# Clean up S3 integration test containers
integration-s3-clean:
    cd integration/s3 && docker compose down --volumes --remove-orphans

# Run all integration tests
integration: integration-cli integration-api

# ---------------------------------------------------------------------------
# WASM
# ---------------------------------------------------------------------------

# Build WASM demo for GitHub Pages
wasm-build:
    ./scripts/build-wasm-demo.sh

# Check WASM feature slice compiles
wasm-check:
    cargo check --no-default-features --features wasm --lib

# Lint WASM feature slice
wasm-lint:
    cargo clippy --no-default-features --features wasm --lib -- -D warnings

# ---------------------------------------------------------------------------
# Server
# ---------------------------------------------------------------------------

# Start development server (cargo run)
serve *ARGS:
    cargo run -- serve {{ARGS}}

# Start development server with Docker Compose
serve-docker:
    docker compose up --build

# ---------------------------------------------------------------------------
# Utility
# ---------------------------------------------------------------------------

# Generate integration test fixture images (requires ImageMagick 7 + Python 3)
generate-fixtures:
    ./scripts/generate-fixtures.sh

# Remove build artifacts
clean:
    cargo clean

# Install development tools
setup:
    cargo install cargo-audit cargo-llvm-cov
    rustup component add llvm-tools-preview
    @echo "Optional: cargo install wasm-bindgen-cli --version 0.2.114"
    @echo "Optional: rustup target add wasm32-unknown-unknown"
