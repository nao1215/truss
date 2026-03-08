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
- A raster backend now performs resize, rotate, format conversion, best-effort `preserve_exif` retention, and common-case `keep-metadata` retention for EXIF + ICC on local JPEG, PNG, and WebP workflows, with AVIF output support.
- Core media sniffing currently supports `jpeg`, `png`, `webp`, and best-effort `avif` container inspection for width, height, and alpha.
- The repository now has unit tests, integration tests, and doc tests for the implemented CLI and Core slices.
- GIF support is explicitly out of scope for the current product direction, and SVG remains deferred for a future phase.
- The server adapter now supports signed public GET transforms at `GET /images/by-path` and `GET /images/by-url`.
- The server adapter now supports a minimal private `POST /images:transform` flow for Bearer-authenticated `path` sources.
- The server adapter now supports `source.kind=url` with scheme validation, redirect limits, response-size limits, resolved-IP checks, pinned outbound socket targets, and an opt-in insecure allowance for local testing.
- The server adapter now supports the private multipart upload API at `POST /images` with `file` and optional JSON `options` parts.
- The server adapter now exposes an authenticated `/metrics` endpoint with minimal Prometheus-compatible counters.
- The CLI adapter now supports `truss sign` for generating signed public GET URLs using the same HMAC-SHA256 canonical request form that the server adapter verifies.
- The `sign_public_url` library function and `SignedUrlSource` enum are part of the public API.
- Coverage can now be measured with `./scripts/coverage.sh`, which wraps `cargo llvm-cov --workspace --all-targets --summary-only`.

## Initial Plan

1. Introduce shared Core types for artifacts, media types, transform options, and errors.
2. Move HTTP-specific behavior behind adapter-oriented modules while preserving the current health endpoints.
3. Add tests for new Core behavior and keep the existing health endpoint tests passing.

## Active Plan

1. Revisit AVIF decode only after the runtime dependency strategy for `dav1d` is decided.
2. Reconcile the remaining documented API gaps, especially `svg` and any behavior that is still stricter than `doc/openapi.yaml`.
3. WebP lossy quality control requires a dependency beyond the current `image` crate (v0.25.8), which only provides lossless WebP encoding through `image-webp`. A native binding such as `libwebp` would be needed, with implications for the planned WASM target.
4. XMP/IPTC Phase 2: byte-level insertion into encoded output (JPEG APP1/APP13, PNG iTXt). Deferred until Phase 1 usage demonstrates demand.

## Work Log

- 2026-03-08: Added repository-level LLM instructions in `AGENTS.md`.
- 2026-03-08: Created this shared implementation log for future work.
- 2026-03-08: Started phase 1 implementation for Core types, validation, and server adapter separation.
- 2026-03-08: Added `src/core.rs` with documented Core types for artifacts, requests, options, normalization, and errors.
- 2026-03-08: Added `src/adapters/server.rs` and moved the minimal health server into an adapter-oriented module.
- 2026-03-08: Phase 1 validation rules currently treat `fit` and `position` as requiring both `width` and `height`.
- 2026-03-08: Phase 1 media types are limited to `jpeg`, `png`, `webp`, and `avif`; at that stage both `gif` and `svg` were deferred.
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
- 2026-03-08: Current raster conversion supports JPEG, PNG, lossless WebP, and AVIF output; AVIF decode is still unimplemented.
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
- 2026-03-08: Changed the CLI dispatch so implicit invocation is convert-oriented; `truss <INPUT> -o <OUTPUT>` and `truss --url <URL> -o <OUTPUT>` now route to convert without the `convert` subcommand.
- 2026-03-08: Server startup no longer happens on an empty invocation; the server now starts through `truss serve` or top-level server runtime flags such as `--bind` and `--storage-root`.
- 2026-03-08: Added unit, integration, and doc coverage for the new dispatch rules, including bare-invocation failure, implicit convert, and implicit server startup through top-level runtime flags.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the default-dispatch change.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 83.10% regions, 76.01% functions, and 82.66% lines across all targets.
- 2026-03-08: Current automated verification covers 77 unit tests, 13 integration tests, and 6 doc tests.
- 2026-03-08: Reworked server-side remote fetches so each request pins outbound socket addresses to the already-validated DNS resolution instead of resolving again at connect time.
- 2026-03-08: Added a `PinnedResolver` in the server adapter, disabled proxy-from-env for remote source fetches, and kept redirect handling by re-resolving and revalidating each redirect target.
- 2026-03-08: Added unit coverage for pinned remote-target preparation and unexpected-netloc rejection, plus integration coverage for redirected URL sources through the HTTP server adapter.
- 2026-03-08: Added a compile-checked documentation example for `ServerConfig::from_env`.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the remote-fetch pinning changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 83.67% regions, 76.77% functions, and 83.40% lines across all targets.
- 2026-03-08: Current automated verification covers 79 unit tests, 14 integration tests, and 7 doc tests.
- 2026-03-08: Implemented signed public GET endpoints at `GET /images/by-path` and `GET /images/by-url` using HMAC-SHA256 over the documented canonical request form.
- 2026-03-08: Added signed-URL server configuration via `TRUSS_SIGNED_URL_KEY_ID` / `TRUSS_SIGNED_URL_SECRET`, plus CLI `serve` overrides for those settings.
- 2026-03-08: `TRUSS_PUBLIC_BASE_URL` is now used as the canonical signed-URL authority override when the server runs behind a proxy; otherwise the incoming `Host` header is used.
- 2026-03-08: Added unit coverage for signed request verification and public query validation, integration coverage for signed public path/url requests, and doc coverage for `ServerConfig::with_signed_url_credentials`.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the signed public GET changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 83.58% regions, 74.81% functions, and 83.27% lines across all targets.
- 2026-03-08: Current automated verification covers 83 unit tests, 16 integration tests, and 8 doc tests.
- 2026-03-08: Started phase 9 planning to add real AVIF encode support while keeping AVIF decode explicitly capability-gated.
- 2026-03-08: Enabled AVIF encode in `src/codecs/raster.rs` through `image`'s `AvifEncoder`, keeping AVIF input decode explicitly capability-gated.
- 2026-03-08: Added unit coverage for successful AVIF output and continued AVIF-decode rejection, integration coverage for CLI AVIF output inference from `.avif`, and an additional runnable `transform_raster` doc test for AVIF output.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the AVIF encode changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 83.54% regions, 74.69% functions, and 83.33% lines across all targets.
- 2026-03-08: Current automated verification covers 84 unit tests, 17 integration tests, and 9 doc tests.
- 2026-03-08: Started phase 10 planning to evaluate and, if viable, enable AVIF input decode in the raster backend.
- 2026-03-08: Attempted to enable AVIF input decode through `image`'s `avif-native` feature, but the current environment lacks the required system `dav1d` library, so AVIF decode remains capability-gated.
- 2026-03-08: Implemented best-effort `preserve_exif` support in `src/codecs/raster.rs` for JPEG, PNG, and WebP output, including EXIF orientation normalization after auto-orient is applied.
- 2026-03-08: `strip_metadata=false` without `preserve_exif=true` still returns a capability error because full metadata retention is not implemented yet.
- 2026-03-08: Added unit coverage for EXIF preservation, EXIF normalization, unsupported AVIF EXIF retention, and continued AVIF-decode rejection; added CLI integration coverage for `--keep-metadata --preserve-exif`; added a runnable `transform_raster` doc test for EXIF preservation.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the EXIF preservation changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 83.39% regions, 73.79% functions, and 83.34% lines across all targets.
- 2026-03-08: Current automated verification covers 86 unit tests, 18 integration tests, and 10 doc tests.
- 2026-03-08: Started phase 11 planning to broaden `keep-metadata` beyond `preserve_exif`, while still rejecting metadata types the current encoders cannot round-trip.
- 2026-03-08: Implemented the common `keep-metadata` path in `src/codecs/raster.rs`, which now preserves EXIF and ICC profiles for JPEG, PNG, and WebP output when the input metadata is limited to fields those encoders can write.
- 2026-03-08: `preserve_exif` now strips ICC on purpose, and EXIF orientation is only normalized when auto-orient actually runs on JPEG input.
- 2026-03-08: `keep-metadata` still rejects metadata types the current encoders cannot round-trip, specifically XMP and IPTC, and AVIF output still rejects retained metadata.
- 2026-03-08: Added unit coverage for JPEG/PNG/WebP metadata round-trips, empty-metadata success, and `preserve_exif` versus `keep-metadata` behavior; added CLI integration coverage for ICC preservation; added a runnable `transform_raster` doc test for ICC retention.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the `keep-metadata` changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 84.20% regions, 72.31% functions, and 83.99% lines across all targets.
- 2026-03-08: Current automated verification covers 91 unit tests, 19 integration tests, and 11 doc tests.
- 2026-03-08: Dropped GIF support from the planned product scope and aligned repository guidance and documentation to stop advertising GIF as a current or future target.
- 2026-03-08: Started phase 12 planning to improve AVIF inspection with container-level width, height, and alpha extraction while keeping AVIF decode capability-gated.
- 2026-03-08: Implemented container-level AVIF inspection in `src/core.rs` by walking ISO BMFF boxes for `ispe`, `auxC`, and `auxl` metadata without enabling full AVIF decode.
- 2026-03-08: `sniff_artifact` can now report AVIF width, height, and best-effort alpha information when the container exposes structured metadata; minimal brand-only AVIF detection still succeeds with unknown dimensions.
- 2026-03-08: Added unit coverage for AVIF dimensions with and without alpha, a runnable `sniff_artifact` doc test for AVIF inspection, and CLI integration coverage for `truss inspect` on a local AVIF file.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the AVIF inspection changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 84.07% regions, 72.12% functions, and 83.69% lines across all targets.
- 2026-03-08: Current automated verification covers 93 unit tests, 20 integration tests, and 12 doc tests.
- 2026-03-08: Started phase 13 planning to improve HTTP image response semantics with adapter-side output negotiation and response headers for cacheability and content safety.
- 2026-03-08: Implemented adapter-side HTTP output negotiation for supported raster formats when `format` is absent, using the request `Accept` header before the Core layer defaults the output type.
- 2026-03-08: Added image response headers for transformed HTTP outputs: strong SHA-256 `ETag`, public/private `Cache-Control`, `X-Content-Type-Options: nosniff`, and `Content-Disposition`, plus `Vary: Accept` when negotiation is used.
- 2026-03-08: Public signed GET requests now honor `If-None-Match` and can return `304 Not Modified` with the corresponding cache metadata.
- 2026-03-08: Added unit coverage for negotiation and image-response header generation, plus integration coverage for public Accept negotiation, public conditional requests, and private no-store responses.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after the HTTP response-semantics changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 84.44% regions, 73.15% functions, and 84.25% lines across all targets.
- 2026-03-08: Current automated verification covers 96 unit tests, 23 integration tests, and 12 doc tests.
- 2026-03-08: Started phase 14 planning to add signed public URL generation in the library and CLI so public GET transforms can be produced without reimplementing the HMAC contract externally.
- 2026-03-08: Implemented `sign_public_url` library function and `SignedUrlSource` enum in `src/adapters/server.rs`, reusing the same HMAC-SHA256 canonical request form that the server verifies at request time.
- 2026-03-08: Implemented `truss sign` CLI command accepting `--base-url`, `--path`, `--url`, `--version`, `--key-id`, `--secret`, `--expires`, and all transform options. The generated URL is written to stdout for scripting and piping.
- 2026-03-08: Added unit tests for `sign` argument parsing (path and URL sources, transform option forwarding, error cases) and an end-to-end integration test in `tests/cli_sign.rs` that generates a signed URL, fetches it from a test server, and verifies the transformed output.
- 2026-03-08: Ran `cargo fmt`, `cargo test`, and `./scripts/coverage.sh` after phase 14 changes.
- 2026-03-08: Current coverage summary from `cargo llvm-cov` is 82.79% regions, 71.49% functions, and 83.30% lines across all targets.
- 2026-03-08: Current automated verification covers 99 unit tests, 24 integration tests, and 13 doc tests.
- 2026-03-08: Phase 14 is complete. The signed public URL generation helper and CLI entry point are now fully usable.
- 2026-03-08: Fixed 3 clippy warnings: redundant closure in `cli.rs`, collapsible if in `server.rs`, and needless lifetime in `core.rs`.
- 2026-03-08: Added Docker deployment design, cross-platform policy, and pure-Rust dependency principle to `doc/runtime-architecture.md` (sections 9, 10).
- 2026-03-08: Identified `ring` (via `ureq` → `rustls`) as the only C dependency. Self-contained build-time C dependency (source bundled, compiled via `cc` crate) is acceptable. System-installed C libraries are prohibited.
- 2026-03-08: Updated `doc/runtime-architecture.md` with C dependency policy: self-contained build-time C deps are allowed, system-installed C libraries requiring `pkg-config`/`cmake` are prohibited.
- 2026-03-08: Created `Dockerfile` with multi-stage build using `rust:1-slim` builder and `distroless/cc-debian12:nonroot` runtime. Used `cc-debian12` instead of `static-debian12` because `ring` dynamically links libc.
- 2026-03-08: Created `.dockerignore` to exclude `target/`, `.git/`, `.claude/`, `doc/`, and documentation files from the Docker build context.
- 2026-03-08: Created `compose.yml` with environment-based configuration, read-only filesystem, and `no-new-privileges` security hardening.
- 2026-03-08: Verified Docker build and runtime: image size 49MB (content 12.6MB), `/health` endpoint responds correctly from `distroless/cc-debian12:nonroot` container.
- 2026-03-08: Implemented transform pixel limits: `MAX_DECODED_PIXELS` (100M) checked before decode using sniffed metadata, `MAX_OUTPUT_PIXELS` (67M) checked before resize allocation. Added `TransformError::LimitExceeded` variant, HTTP 413 mapping, 4 unit tests, 1 integration test, 2 doc tests. Total tests: 144.
- 2026-03-08: Design decision: XMP/IPTC metadata retention uses best-effort (silent drop + warning) in Phase 1. Phase 2 will implement byte-level insertion into encoded output. Documented in `doc/runtime-architecture.md` section 11.2.
- 2026-03-08: Implemented XMP/IPTC best-effort handling. `--keep-metadata` now silently drops XMP/IPTC and returns `TransformWarning::MetadataDropped` warnings. Added `MetadataKind`, `TransformWarning`, `TransformResult` to public API. Changed `transform_raster` return type to `Result<TransformResult, TransformError>`. CLI prints warnings to stderr, server logs to stderr. 3 new unit tests, 2 new doc tests. Total tests: 149.
