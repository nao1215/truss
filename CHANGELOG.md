# Changelog

## v0.11.4

### Fixed

- Update `aws-lc-sys` 0.38.0 → 0.39.0 to fix CRL Distribution Point scope check logic error and X.509 Name Constraints bypass via wildcard/unicode CN (high severity).
- Update `rustls-webpki` 0.103.9 → 0.103.10 to fix certificate revocation enforcement bug (medium severity).

## v0.11.3

### Added

- Port security and edge-case tests:
  - SSRF: redirect chain to metadata endpoint, scheme rejection (ftp/file/data), userinfo rejection, private IP/port blocking in strict mode.
  - Path traversal: E2E coverage for `../../etc/passwd`, mid-path dotdot, `.git` file content leak prevention.
  - Remote errors: upstream 4xx/5xx/403 mapped to 502, Content-Length exceeding limit returns 413, unsupported Content-Encoding (deflate, zstd) returns 502.
  - Image edge cases: corrupted/empty/truncated images return 415, ETag stability and divergence across processing options, ETag mismatch returns 200.
  - IP deny-list boundary tests: CGNAT, TEST-NET 198.18/15, broadcast, multicast, documentation ranges, IPv6 mapped/compatible/6to4/Teredo variants.
  - Path resolution: null byte injection, backslash literal on Unix, unicode filenames, very long components, multiple leading slashes, trailing dotdot.
  - Content-Encoding: multiple known encodings, mixed with unknown, whitespace handling.
  - Cloud metadata: GCP/AWS path variants, non-metadata IP allowed.

### Fixed

- Align crate, npm package, OpenAPI, example lockfile, and changelog release metadata for the `v0.11.3` release.

## v0.11.2

### Added

- Publish a production-oriented Next.js example that signs public truss URLs with `@nao1215/truss-url-signer`.

### Changed

- Verify Homebrew installs against `nao1215/tap/truss` during tagged releases and keep the formula layout aligned with `nao1215/homebrew-tap`.

### Fixed

- Align crate, npm package, OpenAPI, example lockfile, and changelog release metadata for the `v0.11.2` release.

## v0.11.1

### Added

- Publish `truss` Homebrew formulas from tagged releases to `nao1215/homebrew-tap` and verify installation on macOS.

### Changed

- Publish `@nao1215/truss-url-signer` from tagged releases via npm trusted publishing.
- Add README and deployment guide install paths for Homebrew and clarify the release prerequisites for the tap automation.

### Fixed

- Align crate, npm package, OpenAPI, example lockfile, and changelog release metadata for the `v0.11.1` release.

## v0.11.0

### Added

- Official `@nao1215/truss-url-signer` npm package source and release artifact flow for Node.js / TypeScript public signed URLs.
- Type definition compile checks plus Rust/Node compatibility coverage for `HEAD` signing, presets, and watermark parameters.

### Fixed

- Validate signed URL transform and watermark options in the TypeScript signer so it rejects server-invalid values before signing.
- Align crate, npm package, OpenAPI, and changelog release metadata for the `v0.11.0` release.

## v0.10.4

### Fixed

- Align crate, package, OpenAPI, and changelog release metadata for the `v0.10.4` tag after bootstrapping the npm package and trusted publisher settings.

## v0.10.3

### Changed

- Switch npm package publishing in GitHub Actions from `NPM_TOKEN`-based authentication to npm trusted publishing with GitHub OIDC.

### Fixed

- Align crate, package, OpenAPI, and changelog release metadata for the `v0.10.3` tag.

## v0.10.2

### Fixed

- Fix GitHub release workflow validation so the npm publish job no longer references `secrets` directly in an `if:` expression.
- Align crate, package, OpenAPI, and changelog release metadata for the `v0.10.2` tag.

## v0.10.0

### Added

- Official `@nao1215/truss-wasm` npm package source for third-party browser integration with a fixed `wasm,svg,avif` feature set.
- Release automation to pack the Wasm npm package, attach its tarball to GitHub Releases, and publish to npm when `NPM_TOKEN` is configured.

### Changed

- Expanded WASM documentation with npm package quick-start guidance, bundler-focused distribution details, build-mode differences, and local packaging instructions.
- Clarified the supported browser build matrix so AVIF support and WebP lossless behavior are explicit in the official package flow.

## v0.9.0

### Added

- Format-aware image optimization across the CLI, HTTP API, signed URLs, presets, and WASM with `optimize=auto|lossless|lossy` plus perceptual `targetQuality` controls.
- Optional Bearer token authentication for `/health` via `TRUSS_HEALTH_TOKEN`, while keeping `/health/live` and `/health/ready` unauthenticated for orchestrator probes (#73).
- Readiness probe hysteresis via `TRUSS_HEALTH_HYSTERESIS_MARGIN` to reduce flapping near disk and memory thresholds (#72).
- Additional fast coverage for lifecycle signal handling, public `HEAD` endpoints, and CLI runtime error paths.

### Fixed

- Gate AVIF/WebP native dependencies behind feature flags so the WASM build no longer imports unavailable C-backed components.
- Skip serializing transformed image bytes into WASM response JSON to avoid OOM on large outputs.
- Reject truncated JPEG input during lossless optimization.
- Stabilize HEAD and optimization-related tests after the runtime-target optimization work.

### Changed

- Consolidate project documentation under `docs/` and expand CLI examples for piping, stdin/stdout usage, and optimization workflows.
- Deduplicate cloud integration test helpers and parameterize HEAD request tests with `rstest`.
- Update the OpenAPI and configuration docs to cover optimization controls, `/health` authentication, and readiness hysteresis behavior.

## v0.8.0

### Added

- Lock-free syscall caching for health check endpoints (`disk_free_bytes`, `process_rss_bytes`) with configurable TTL via `TRUSS_HEALTH_CACHE_TTL_SECS` (default: 5s, range: 0–300). Eliminates redundant kernel context switches under high-frequency polling (#74).
- `ServerConfig::with_health_cache_ttl_secs()` builder method for programmatic TTL override.
- Per-IP rate limiting with sharded buckets to reduce mutex contention (#127).
- Reverse proxy support: resolve real client IP behind trusted proxies for rate limiting via `TRUSS_TRUSTED_PROXIES` (#117).
- `#[must_use]` annotations on key public types and functions (#130).
- `#[non_exhaustive]` on public enums for semver safety (#122).
- Integration tests for HEAD requests (#123).
- Unit tests for routing, signing, and inspect modules (#124).
- Non-ASCII input tests for `Rgba8::from_hex` (#131).
- Security audit CI on pull requests (#128).
- PR template and updated stale bug report placeholder (#126).

### Fixed

- Block SSRF bypass via IPv4-compatible, 6to4, and Teredo IPv6 addresses (#118).
- Add element count and nesting depth limits to SVG sanitizer; fix CSS `url()` search performance (#119).
- Disambiguate NUL escape to avoid clippy `octal_escapes` lint (#124).
- Guard `Rgba8::from_hex` against non-ASCII input (#131).
- Add `#[serial]` to cloud integration tests that use `env::set_var` (#116).
- Prevent flaky redirect-limit test on Windows (WSAECONNABORTED).
- Use acquire/release memory ordering in `HealthCache` for correctness on weakly-ordered architectures.

### Changed

- Extract `collect_resource_checks()` to deduplicate ~70 lines of identical logic between `handle_health()` and `handle_health_ready()`.
- Introduce unified transform dispatch to eliminate SVG/raster routing duplication (#115).
- Remove ~2400 lines of duplicated code from `server/mod.rs` (#114).
- Replace relay imports with direct submodule references in `auth.rs` and `metrics.rs`.
- Consolidate duplicated test helpers in CLI integration tests (#121).
- Replace manual JSON construction with `serde_json` in inspect command (#129).
- Throttle cache eviction scans and remove unnecessary `fsync` (#120).
- Hide `HealthCache` from public API; expose TTL via builder method.
- Document `TRUSS_HEALTH_CACHE_TTL_SECS`, `TRUSS_HEALTH_CACHE_MIN_FREE_BYTES`, and `TRUSS_HEALTH_MAX_MEMORY_BYTES` in `from_env` rustdoc.
- Update pipeline and Prometheus docs with crop/sharpen stages and watermark metric (#125).
- Bump clap 4.5→4.6, clap_complete 4.5→4.6, aws-sdk-s3 1.125→1.126.

## v0.7.2

### Fixed

- Fix aarch64 cross-compilation failure by using newer cross-rs base image with OpenSSL 3.x support.

## v0.7.1

### Added

- Hot-reload for transform presets via `TRUSS_PRESETS_FILE` with file-watching support.
- Dynamic log level switching via `TRUSS_LOG_LEVEL` env var and `SIGUSR1` signal.
- Unit and integration tests for log level and preset hot-reload.
- Crop, rotate, fit, and inspect examples to README.

### Fixed

- Use `saturating_duration_since` in rate limiter for Windows compatibility.
- Do not update `last_modified` on preset parse failure to handle torn reads.
- Use `wasm32-wasip1` C target for wasi-sdk sysroot header resolution in Pages CI.

### Changed

- Update `Cargo.toml` keywords for better crates.io discoverability.
- Comprehensive project improvements from multi-perspective review.

## v0.7.0

### Added

- Configurable max input pixel limit (`TRUSS_MAX_INPUT_PIXELS`) with 422 response for oversized images.
- Configurable max upload body size (`TRUSS_MAX_UPLOAD_BYTES`) with 413 response for oversized uploads.
- Optional Bearer token protection for `/metrics` endpoint (`TRUSS_METRICS_TOKEN`) and disable flag (`TRUSS_DISABLE_METRICS`).
- Configurable keep-alive max requests (`TRUSS_KEEP_ALIVE_MAX_REQUESTS`).
- Config validation subcommand (`truss validate`) for CI/CD pre-flight checks.
- Enhanced health checks: cache disk free space (`TRUSS_HEALTH_CACHE_MIN_FREE_BYTES`), transform capacity, and process memory usage (`TRUSS_HEALTH_MAX_MEMORY_BYTES`).
- Graceful shutdown with configurable drain period (`TRUSS_SHUTDOWN_DRAIN_SECS`); `/health/ready` returns 503 immediately on SIGTERM/SIGINT.
- Custom response headers via `TRUSS_RESPONSE_HEADERS` JSON env var with security-critical header rejection.
- Gzip response compression for non-image responses with configurable level (`TRUSS_COMPRESSION_LEVEL`) and disable flag (`TRUSS_DISABLE_COMPRESSION`).
- Crop control in the WASM demo page UI.
- SVG and lossy WebP features enabled in the WASM demo build.

### Fixed

- `Box::leak` per-request memory leak in custom response headers.
- Reject security-critical headers (framing, hop-by-hop) in `TRUSS_RESPONSE_HEADERS` at startup.
- Merge `Vary` headers into a single line to avoid duplication.
- Reduce worker drain timeout to 15 s for Kubernetes compatibility.
- Replace busy-wait accept loop with `poll(2)` on Unix.
- Windows graceful shutdown via SIGINT handler and draining check.
- Use `sigaction`, `AtomicI32`, `cast_mut`, and `O_NONBLOCK` on write fd for signal safety.
- Pixel-cap check moved before cache lookup to prevent unnecessary cache reads.
- Early-reject `/metrics` before body read.
- README: `--bearer-token` CLI flag corrected to `TRUSS_BEARER_TOKEN` env var.
- README: `POST /images:transform` curl example corrected to `POST /images` for multipart uploads.

### Changed

- OpenAPI spec documents HEAD method support on all GET endpoints.
- `UnprocessableEntity` response includes example in OpenAPI spec.
- `maxInputPixels` marked as required in `HealthDiagnosticResponse` schema.
- Extracted `parse_env_u64_ranged` helper for env var parsing.

## v0.6.2

### Fixed

- aarch64 cross-compilation failure: `Cross.toml` pre-build now installs `libssl-dev:arm64` instead of the host-architecture package, so `openssl-sys` finds the correct headers.

### Changed

- Release profile: enable thin LTO, single codegen unit, and binary stripping for smaller, faster binaries.
- Unified `stderr_write` usage across S3, GCS, and Azure backends to avoid Rust 2024 `ReentrantLock` issues with `eprintln!`.
- Cache key computation uses streaming `Sha256` hasher and inline parameter builder, eliminating intermediate allocations and sort.
- Watermark margin capped at 9999 with explicit validation on both JSON and multipart endpoints.
- Docker Compose healthcheck added for the `truss` service.

### Added

- Unit tests for `auth`, `http_parse`, `multipart`, `negotiate`, and `response` modules (314 new tests).

## v0.6.1

### Fixed

- HTTP response splitting (CRLF injection) via `X-Request-Id` header — CR, LF, and NUL bytes are now rejected.
- Integer overflow in AVIF decode when frame dimensions exceed address space (`width * height * 4`).
- aarch64-unknown-linux-gnu release build failure caused by missing OpenSSL (`Cross.toml` pre-build step).

### Changed

- Extracted `ServerConfig` and related types into dedicated `config.rs` module (~980 lines out of `mod.rs`).
- Deduplicated `read_remote_source_bytes` / `read_remote_watermark_bytes` into shared `fetch_remote_bytes` with `RemoteFetchPolicy`.
- Cleaned up unused imports in server module after config extraction.

### Added

- Integration tests: health endpoint 200, unknown path 404, CRLF injection prevention, missing Content-Type 415, invalid JSON body 400, missing source file 404.
- Characterization unit tests for `extract_request_id`, `ServerConfig` defaults/builder, `route_request`, and `TransformSlot` concurrency.

## v0.6.0

### Added

- Explicit crop operation (`--crop x,y,w,h` CLI flag, `crop` query parameter, JSON/WASM adapters). Applied after auto-orient and rotation but before resize. Not supported for SVG inputs.
- Signed URL key rotation via `TRUSS_SIGNING_KEYS` JSON env var. Multiple key IDs can be active simultaneously for zero-downtime key rotation.
- Server-side transform presets via `TRUSS_PRESETS` / `TRUSS_PRESETS_FILE` env vars with `preset` query parameter.
- Sharpen filter (`--sharpen` CLI flag, `sharpen` query parameter, WASM adapter) using unsharp mask. Valid sigma range 0.1–100.0.
- TIFF format support for input and output across CLI, HTTP server, and WASM.
- Watermark overlay support for signed public URLs (`watermarkUrl`, `watermarkPosition`, `watermarkOpacity`, `watermarkMargin` query params).
- `sign_public_url` and CLI `sign` command now accept watermark parameters.
- `truss_watermark_transforms_total` Prometheus counter.
- `watermark` field in structured access log entries.
- `MAX_WATERMARK_PIXELS` limit (4 MP) checked before watermark decode.
- Request deadline (60 s) caps total outbound fetch time per request.
- Origin cache namespace separation (`src:` / `wm:`) prevents cross-contamination.
- WASM UI: watermark file type validation, 10 MB size limit, loading/clear feedback.
- Integration tests for orphaned watermark params, empty URL, SVG + watermark rejection, and redirect following.
- Prebuilt release binaries with checksums for Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (x86_64).
- Multi-arch container images (amd64, arm64) published to GHCR on release.

### Changed

- Watermark fetch is deferred until concurrency slot is acquired (two-phase validation + fetch).
- SVG sources with watermark requests are rejected early with 400.
- Watermark fetch errors are sanitized; detailed errors logged server-side only.
- Cache key normalization uses parsed `Position` for consistent hashing.
- WASM UI: blur values below 0.1 treated as no blur; "Blur sigma" label simplified to "Blur".
- WASM UI: `.is-busy` scoped to interactive elements instead of entire page.
- WASM UI: download filename includes `-watermarked` suffix when applicable.
- Integration test workflow refactored from 4 duplicate jobs to a single matrix strategy.
- `Dockerfile.release` uses `COPY --chown` and explicit `chmod` for binary permissions.
- `parse_presets_from_env` treats empty `TRUSS_PRESETS_FILE` as unset; JSON parse errors include source info.
- `ServerConfig::PartialEq` compares preset contents instead of only length.

### Fixed

- Accessibility: `role="alert"` on error box, `:focus-within` on dropzones, `name` attributes on watermark inputs, `<noscript>` fallback.

## v0.5.0

### Added

- Prometheus `/metrics` endpoint with histograms (HTTP request duration, transform duration, storage duration) and error counters.
- Prometheus metrics documentation (`docs/prometheus.md`).
- Dedicated 304 status counter for cache-validation traffic tracking.

### Changed

- `/metrics` endpoint no longer requires bearer token authentication for Prometheus scraper compatibility.
- Cross-platform CI tests (macOS/Windows) now run on pull requests, not only on main pushes.
- Storage duration metrics now reflect actual source kind (filesystem/S3/GCS/Azure) instead of server config default.
- HTTP request duration histogram records on all exit paths including auth and body-read errors.

### Fixed

- Windows compilation error: `unsafe extern "system"` block for Rust 2024 edition.
- Cross-platform `stderr_write` using `GetStdHandle` on Windows.

## v0.4.0

### Added

- S3-compatible object storage backend (`--features s3`).
- Google Cloud Storage backend (`--features gcs`).
- Azure Blob Storage backend (`--features azure`).
- SSRF validation for S3/GCS/Azure backend endpoint URLs.
- Signed URL support for S3/GCS/Azure source images.
- Structured JSON access logs with request ID (`X-Request-Id`) and RAII concurrency guard.
- Configurable server concurrency and deadline limits.
- Startup health check for storage backends (fail-fast).
- Configurable storage timeout via `TRUSS_STORAGE_TIMEOUT_SECS`.

### Changed

- Bump `quick-xml` 0.37→0.39 and `resvg` 0.45→0.47.
- Azure environment variable renamed from `TRUSS_AZURE_BUCKET` to `TRUSS_AZURE_CONTAINER`.
- Use `subtle::ConstantTimeEq` for bearer token comparison.
- Graceful shutdown with 30-second deadline.
- Backend 401 responses mapped to 502 Bad Gateway.
- Health check name unified to `storageBackend` across all backends.
- Debug output masks `bearer_token` and `signed_url_secret` as `[REDACTED]`.

### Fixed

- Access-log latency measured after header read and after response write.
- Per-server in-flight counter and pool sizing.

## v0.3.0

### Added

- Blur filter support (`blur` query parameter) for image transforms.
- Watermark overlay support for image transforms.
- Sample image and template for documentation.

### Changed

- Refactored README for clarity.
- Optimized GitHub Actions workflows for faster CI.

### Fixed

- Blur cache key precision issue.
- SVG blur/watermark rejection handling.
- Watermark pixel limit validation.
- Relaxed watermark size check to match position-based margin usage.
- Pass watermark to `transform_svg` for proper SVG input rejection.
- Updated help text and OpenAPI spec for blur/watermark options.
- Update OpenAPI spec version from 0.2.0 to 0.3.0.

## v0.2.0

### Added

- HTTP/1.1 keep-alive and HEAD method support for CDN origin use.
- SVG rasterization and input-format preservation in Accept negotiation.
- `TRUSS_DISABLE_ACCEPT_NEGOTIATION` flag to avoid CDN cache key mismatches.
- Configurable `Cache-Control` max-age / stale-while-revalidate via environment variables.
- Signed URL support for public GET endpoints (`GET /images/by-path`, `GET /images/by-url`).
- Download counter.
- Benchmark results to README.
- CDN architecture documentation and cache key configuration guidance.
- Mobile-friendly WASM demo with aspect ratio lock.
- Edge case tests.
- `truss help completions` and `truss help version` help topics.
- Shell completions now expose implicit-convert (`-o`, `INPUT`) and implicit-serve (`--bind`, `--storage-root`) arguments.
- Commands table and shell completion setup guide in README.
- Exit code 5 (runtime error) documented in `--help` exit code listing.

### Changed

- Refactored `server.rs` into 9 sub-modules for maintainability.
- Normalize default fit/position in cache key for better hit rate.
- Authenticate private POST routes before reading request body.
- Use unique temp-file suffix for concurrent cache writes.
- Accept negotiation uses specificity to break ties (e.g. `image/png` over `image/*`).

### Fixed

- Validate multipart boundary suffix to prevent payload collision.
- Apply rotation in SVG rasterization path.
- Treat extensionless files as implicit `convert` input; use `is_file()` to exclude directories.
- Reject Transfer-Encoding header to prevent request smuggling.
- Warn at startup when signed URL credentials are set without `TRUSS_PUBLIC_BASE_URL`.
- Accept `Authorization: bearer` (case-insensitive scheme) per RFC 7235.
- Preserve tail bytes for keep-alive connections instead of truncating.
- Reject header names with leading/trailing whitespace.
- Enforce `MAX_HEADER_BYTES` at header terminator, not just buffer size.
- Handle weak ETags (`W/"..."`) in `If-None-Match` comparison.
- Only treat 2xx HTTP responses as successful remote fetches.
- Block IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`) in SSRF check.
- Correct inverted `data:image/*` allowlist in SVG sanitizer; `data:image/png` etc. were incorrectly blocked while `data:image/svg+xml` was incorrectly allowed.
- Clamp aspect-ratio synced dimensions to minimum of 1 in WASM demo.
- Reduce idle timeout for unconsumed fixture responses to speed up tests.
- Map `InvalidOptions` to exit code 1 (usage) and `InvalidInput` to exit code 3 (input); previously both mapped to exit code 2 (I/O).
- Map output file write failure to exit code 2 (I/O) instead of exit code 5 (runtime).
- Use Drop guard for `TRANSFORMS_IN_FLIGHT` in backpressure test to prevent flaky parallel test failures.
- Update OpenAPI spec version from 0.1.0 to 0.2.0.

### Security

- Sanitize SVG `href`/`xlink:href` with allowlist approach; block embedded SVG payloads.
- Validate remote fetch targets against SSRF policy before serving cached responses.
- Reject whitespace-padded HTTP header names to prevent proxy interpretation differences.

## v0.1.0

- Initial release.
