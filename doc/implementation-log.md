# Implementation Log

## Working Rules

- Create or update a written plan before substantial implementation work.
- Keep this file current so another LLM can resume without re-reading the whole repository.
- Write detailed Rust documentation comments for public items.
- Public functions should document behavior, inputs, outputs, and failure modes clearly.
- Write comments in English.
- Write thorough tests and keep coverage high.
- Add unit tests, integration tests, and doc tests for new or changed functionality.

## Current Status

- Rust stable toolchain is installed and `cargo test` succeeded after installation.
- The repository now exposes shared Core types and normalization logic in `src/core.rs`.
- The minimal HTTP health server now lives under `src/adapters/server.rs`.
- The CLI adapter now supports `serve` and `inspect`.
- The CLI adapter now supports local-file and stdin-based `convert`.
- The CLI adapter now supports `inspect --url` and `convert --url` over HTTP(S).
- A raster backend now performs resize, rotate, and format conversion for local JPEG, PNG, and WebP workflows.
- Core media sniffing currently supports `jpeg`, `png`, `webp`, and brand-level `avif` detection.
- The repository now has unit tests, integration tests, and doc tests for the implemented CLI and Core slices.
- `gif` and `svg` support are still out of scope for the current implementation phase.
- The server adapter now supports a minimal private `POST /images:transform` flow for Bearer-authenticated `path` sources.
- The server adapter now supports `source.kind=url` with scheme validation, redirect limits, response-size limits, resolved-IP checks, and an opt-in insecure allowance for local testing.
- The server adapter now supports the private multipart upload API at `POST /images` with `file` and optional JSON `options` parts.
- The server adapter now exposes an authenticated `/metrics` endpoint with minimal Prometheus-compatible counters.
- Coverage can now be measured with `./scripts/coverage.sh`, which wraps `cargo llvm-cov --workspace --all-targets --summary-only`.

## Initial Plan

1. Introduce shared Core types for artifacts, media types, transform options, and errors.
2. Move HTTP-specific behavior behind adapter-oriented modules while preserving the current health endpoints.
3. Add tests for new Core behavior and keep the existing health endpoint tests passing.

## Active Plan

1. Deepen server-side URL source hardening where the current adapter still relies on best-effort checks, especially connect-time peer revalidation.
2. Revisit public signed GET endpoints once the private server pipeline is more complete.
3. Keep the CLI aligned as new server runtime settings and public-endpoint flows are added.

## Work Log

- 2026-03-08: Added repository-level LLM instructions in `AGENTS.md`.
- 2026-03-08: Created this shared implementation log for future work.
- 2026-03-08: Started phase 1 implementation for Core types, validation, and server adapter separation.
- 2026-03-08: Added `src/core.rs` with documented Core types for artifacts, requests, options, normalization, and errors.
- 2026-03-08: Added `src/adapters/server.rs` and moved the minimal health server into an adapter-oriented module.
- 2026-03-08: Phase 1 validation rules currently treat `fit` and `position` as requiring both `width` and `height`.
- 2026-03-08: Phase 1 media types are limited to `jpeg`, `png`, `webp`, and `avif`; `gif` and `svg` remain future work.
- 2026-03-08: Normalized metadata handling uses `MetadataPolicy` instead of carrying contradictory booleans into the backend pipeline.
- 2026-03-08: Installed `rustfmt`, ran `cargo fmt`, and verified the implementation with `cargo test` (18 tests passed).
- 2026-03-08: Started phase 2 implementation for Core media sniffing and a real CLI `inspect` command.
- 2026-03-08: Added `sniff_artifact` to `src/core.rs` with best-effort metadata extraction for `jpeg`, `png`, and `webp`, plus brand-level `avif` detection.
- 2026-03-08: Added string parsers for `MediaType`, `Fit`, `Position`, `Rotation`, and `Rgba8`.
- 2026-03-08: Added `src/adapters/cli.rs` and routed `src/main.rs` through the CLI adapter.
- 2026-03-08: `truss inspect <INPUT>` now reads local files or stdin and prints JSON metadata based on Core inspection.
- 2026-03-08: At the end of phase 2, `truss convert` and `inspect --url` were still intentionally unimplemented.
- 2026-03-08: AVIF inspection is currently limited to format detection; width, height, and alpha remain unknown.
- 2026-03-08: Ran `cargo fmt` and `cargo test` after phase 2 changes; 38 tests passed.
- 2026-03-08: Started phase 3 implementation for raster conversion and the CLI `convert` command.
- 2026-03-08: Added `image` and `kamadak-exif` dependencies for raster decode/encode and JPEG EXIF orientation handling.
- 2026-03-08: Added `src/codecs/raster.rs` with local raster transform support for resize, contain/cover/fill/inside handling, background padding, rotation, and encoding.
- 2026-03-08: Added CLI `convert` support for local files and stdin/stdout, including strict option parsing and output-format inference from the output extension.
- 2026-03-08: Current raster conversion supports JPEG, PNG, and lossless WebP output; AVIF encode/decode is still unimplemented.
- 2026-03-08: Current raster conversion does not retain metadata; `--keep-metadata` and `--preserve-exif` fail with a capability error.
- 2026-03-08: Current raster conversion does not support `convert --url` or `inspect --url`.
- 2026-03-08: WebP quality control is still unimplemented; requesting it currently fails with a capability error.
- 2026-03-08: When padding is needed and no background is provided, opaque outputs currently default to white and alpha-capable outputs default to transparent.
- 2026-03-08: Ran `cargo fmt` and `cargo test` after phase 3 changes; 50 tests passed.
- 2026-03-08: Strengthened `AGENTS.md` and this log to require unit tests, integration tests, doc tests, and more detailed documentation comments for public functions.
- 2026-03-08: Added `ureq` and implemented CLI URL input for `inspect --url` and `convert --url`.
- 2026-03-08: URL input currently uses a basic adapter-side HTTP resolver with an explicit 32 MiB response limit.
- 2026-03-08: URL input currently accepts `http://` and `https://` syntax, but deeper SSRF hardening and redirect policy are not implemented yet.
- 2026-03-08: Added integration tests in `tests/cli_url.rs` that execute the compiled `truss` binary against a local HTTP fixture server.
- 2026-03-08: Added runnable doc tests for `sniff_artifact` and `transform_raster`.
- 2026-03-08: Ran `cargo fmt` and `cargo test` after URL-input changes; 54 unit tests, 2 integration tests, and 2 doc tests passed.
- 2026-03-08: Started phase 4 planning for the private HTTP transform API in the server adapter.
- 2026-03-08: Phase 4 is scoped to `POST /images:transform` with Bearer auth and `path` sources only; URL sources, upload API, and public signed GET endpoints remain deferred.
- 2026-03-08: Added `serde` and `serde_json` to support strict JSON parsing in the server adapter.
- 2026-03-08: Added `ServerConfig`, explicit-config server entry points, and a minimal HTTP request parser in `src/adapters/server.rs`.
- 2026-03-08: `POST /images:transform` now supports Bearer-authenticated `source.kind=path` requests, storage-root resolution, Core media sniffing, and raster transform responses.
- 2026-03-08: The server adapter currently returns `501 Not Implemented` for `source.kind=url`, and it still does not implement upload transforms, public signed GET endpoints, or metrics.
- 2026-03-08: Added integration tests in `tests/server_transform.rs` for authenticated and unauthorized HTTP transform requests.
- 2026-03-08: Added a runnable doc test for `ServerConfig::new`.
- 2026-03-08: Ran `cargo fmt` and `cargo test` after the server adapter changes; 62 unit tests, 4 integration tests, and 3 doc tests passed.
- 2026-03-08: Started phase 5 planning for repeatable coverage measurement and server-side URL source support.
- 2026-03-08: Installed `llvm-tools-preview` and `cargo-llvm-cov` so coverage can be measured with `cargo llvm-cov`.
- 2026-03-08: Added `url` as a direct dependency so the server adapter can parse, validate, and join remote URLs explicitly.
- 2026-03-08: Added `ServerConfig::with_insecure_url_sources` and `TRUSS_ALLOW_INSECURE_URL_SOURCES` for local development and test environments that need loopback or non-standard-port URL sources.
- 2026-03-08: `POST /images:transform` now supports `source.kind=url` with explicit `http`/`https` scheme checks, host resolution, redirect following up to 5 hops, remote response-size limits, and private-network blocking by default.
- 2026-03-08: The current URL-source implementation is still best-effort: it validates resolved IPs before each fetch, but it does not yet revalidate the final connected peer IP or enforce the documented compression-response policy.
- 2026-03-08: Added `scripts/coverage.sh` as the shared coverage entry point for future work.
- 2026-03-08: Added unit and integration coverage for private-URL rejection, insecure test allowances, and remote redirect handling.
- 2026-03-08: Added a runnable doc test for `ServerConfig::with_insecure_url_sources`.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the URL-source server changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 81.23% regions, 74.65% functions, and 82.01% lines across all targets.
- 2026-03-08: Started phase 6 planning for the private multipart upload API at `POST /images`.
- 2026-03-08: `POST /images` now accepts `multipart/form-data` with a required `file` part and an optional JSON `options` part.
- 2026-03-08: Added strict multipart validation for boundary parsing, part headers, supported field names, duplicate fields, and JSON parsing for the `options` part.
- 2026-03-08: Reused the existing transform pipeline for uploaded bytes so upload, path-based transform, and URL-based transform all converge after source resolution.
- 2026-03-08: Added unit and integration coverage for successful uploads, missing file fields, non-multipart requests, and multipart option parsing.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the multipart upload changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 82.18% regions, 75.48% functions, and 81.49% lines across all targets.
- 2026-03-08: Started phase 7 planning for `/metrics` and the remaining URL-source compression policy.
- 2026-03-08: Added a minimal authenticated `/metrics` endpoint that exposes Prometheus-compatible counters for parsed requests, per-route request totals, response-status totals, and a simple `truss_process_up` gauge.
- 2026-03-08: The current metrics implementation uses process-local atomic counters in the server adapter; it is sufficient for the current single-process runtime but not yet pluggable or persistent.
- 2026-03-08: Added remote response `Content-Encoding` validation so URL sources now reject encodings other than `gzip`, `br`, or `identity`.
- 2026-03-08: Added unit and integration coverage for `/metrics` authentication and payload format, plus rejection of unsupported remote content encodings.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the metrics and compression-policy changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 82.94% regions, 76.99% functions, and 82.40% lines across all targets.
- 2026-03-08: Started phase 8 planning to align the CLI with the current server runtime configuration surface.
- 2026-03-08: Extended `truss serve` to accept `--storage-root`, `--public-base-url`, and `--allow-insecure-url-sources`, with CLI-side config resolution layered on top of `ServerConfig::from_env`.
- 2026-03-08: `truss serve` now prints the resolved storage root, optional public base URL, and the actual listener address, including the real ephemeral port when `--bind 127.0.0.1:0` is used.
- 2026-03-08: Added unit coverage for `serve` argument parsing and config override resolution, plus integration coverage for help output and `serve` startup logging.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the CLI `serve` alignment changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 82.64% regions, 75.73% functions, and 82.28% lines across all targets.
- 2026-03-08: Current automated verification covers 74 unit tests, 12 integration tests, and 4 doc tests.
