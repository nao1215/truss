# Changelog

## Unreleased

### Added

- `--rotate` accepts any whole number of degrees, not only quarter turns ([#303](https://github.com/nao1215/truss/issues/303)), across the CLI, the HTTP API, the WASM build, and `@nao1215/truss-url-signer`. Positive turns clockwise and negative counter-clockwise, and angles are normalized into `0`-`359`, so `-90`, `270`, and `630` are the same rotation. A multiple of 90 keeps the exact pixel-permuting path; any other angle resamples bilinearly in premultiplied alpha, grows the output to the rotated bounding box so no corner is cropped, fills the exposed area with `--background` (transparent by default, white for formats without an alpha channel), and is checked against `MAX_OUTPUT_PIXELS` before the canvas is allocated.
- Static GIF input on `convert`, `optimize`, `inspect`, the HTTP server, and the WASM build ([#301](https://github.com/nao1215/truss/issues/301)). GIF is decode-only: `format=gif` is rejected, and a GIF input that names no output format is encoded as PNG rather than echoed back as GIF. An animated GIF is refused with an error naming the frame count instead of being reduced to its first frame, while `inspect` still reads it and reports `isAnimated`. `integration/fixtures` gains `sample.gif`, `transparent.gif`, and `animated.gif`.
- `--grayscale` on `convert` and `sign`, `grayscale` in the HTTP API (query parameter and JSON body), the WASM options object, and the `@nao1215/truss-url-signer` transform set ([#302](https://github.com/nao1215/truss/issues/302)). Luminance uses the Rec. 601 weights and the alpha channel is preserved. The stage runs after resize, blur, and sharpen and before the watermark, so a watermark keeps its own colors, and it participates in the cache key and the signed-URL canonical string. SVG input is supported when the output is a raster format.

### Changed

- Dependency updates consolidated from Dependabot: `uuid` 1.24 -> 1.26, `ureq` 3.3 -> 3.4, `http` 1.4.2 -> 1.5.0, and `puppeteer-core` 25.4 -> 25.9 in the Vite example.
- The Vite example drops `vite-plugin-top-level-await` and raises its build target to browsers with native top-level await, so nothing has to transform it. `vite` stays on the 7.x line: Vite 8 builds with Rolldown, which emits the `.wasm` asset but leaves no reference to it in the bundle, so the page 404s at runtime.
- Breaking: `Rotation` is a newtype over whole degrees instead of a four-variant enum. `Rotation::Deg90` becomes `Rotation::DEG_90`, `Rotation::from_degrees(i32)` normalizes any integer, and `quarter_turns()` and `is_identity()` replace matching on the variants. `as_degrees()` still returns `u16`, so the cache key and the signed-URL canonical string are unchanged for the angles that were already expressible. In `@nao1215/truss-url-signer` the `QuarterTurn` type is replaced by `RotationDegrees`, and the signer normalizes an angle into `0`-`359` before signing, because the signature covers the query string as sent.
- An input truss can read but cannot process now exits 3 (input error) from the CLI instead of 4 (transform error), matching the documented exit-code table. The only case that reaches this in practice is an animated GIF; unreadable bytes already exited 3.

### Fixed

- Two test fixtures did not have the property they were named for, so the end-to-end scenarios built on them asserted nothing. `integration/fixtures/exif-rotated.jpg` carried no EXIF Orientation tag at all, because `magick -set 'EXIF:Orientation'` is a no-op on an image with no EXIF profile, and `semitransparent.png` was fully opaque, because `magick xc:'rgba(...)'` composites the alpha away unless the canvas already has an alpha channel. Both are now generated with Pillow and actually carry the property, and the scenarios assert the behavior instead of only the output format: an Orientation=6 JPEG must come out with its dimensions swapped, `--no-auto-orient` must leave them alone, and partial alpha must survive a PNG pass and flatten for JPEG.
- Get CI green again: `chunks_exact_mut(4)` in the SVG premultiply loop is now `as_chunks_mut::<4>()`, which the `clippy::chunks_exact_to_as_chunks` lint on current stable rejects, and `h2` moves 0.4.15 -> 0.4.19 for RUSTSEC-2026-0258. The remaining unpatched `h2` 0.3.27 is ignored with a rationale: it is reachable only as an outbound S3 client through the legacy AWS SDK hyper 0.14 path, the 0.3 line has no patched release, and truss's own inbound server does not use h2.

## v0.12.0

### Added

- WebP output carries ICC, EXIF, and XMP in `ICCP`/`EXIF`/`XMP ` container chunks, lossy encoding included. libwebp embeds no metadata, so truss now writes the chunks itself, promoting the container to the extended (`VP8X`) format when needed.
- E2E coverage (atago) for the output pixel budget, color-model preservation, and metadata retention, plus an `integration/fixtures/icc-profile.jpg` fixture carrying an embedded sRGB profile.

### Changed

- `--strip-metadata` now documents, in `truss help convert`/`help optimize` and the README, that lossy optimization keeps the ICC profile so colors are not shifted by the re-encode. The README also gains a per-format metadata support table.

### Fixed

- `convert`/`optimize`: enforce `MAX_OUTPUT_PIXELS` against the real output size when only one of `--width`/`--height` is given ([#252](https://github.com/nao1215/truss/issues/252)). The check previously used the *source* size for the omitted axis, so `--width 10000` on a small image produced a 100-megapixel output with exit 0, and a large enough single dimension stalled while allocating the resize buffer.
- `convert`/`optimize`: stop adding an alpha channel to opaque images ([#253](https://github.com/nao1215/truss/issues/253)). Every decoded image was widened to RGBA8 before encoding, so a no-op same-format pass flipped `hasAlpha` from `false` to `true` and grew the file. PNG, WebP, BMP, TIFF, and AVIF output now use an RGB color model whenever the pixels are fully opaque, and keep alpha whenever any pixel is not.
- `inspect`: report `hasAlpha` for lossless WebP (VP8L) instead of `null`; the `alpha_is_used` header bit is now read.
- `optimize --format webp --mode lossy` no longer fails on ICC-bearing input ([#279](https://github.com/nao1215/truss/issues/279)). The lossy encoder rejected any retained metadata, while a strip request is upgraded to "preserve ICC" for lossy output, so no flag combination worked — `--strip-metadata` was itself the reason the command failed.
- The lossy "preserve ICC" upgrade now applies only to formats that can carry a profile (JPEG, PNG, WebP). For AVIF it stays a full strip instead of putting the pipeline into a state the encoder rejects.

## v0.11.5

### Changed

- Batch dependency updates (consolidated from Dependabot PRs):
  - Rust: `hmac` 0.12 → 0.13 and `sha2` 0.10 → 0.11 (RustCrypto `digest` 0.11); `rav1d-safe` 0.3 → 0.5; `azure_storage_blob` 0.9 → 0.10 with `azure_core` 0.32 → 0.33; `tokio` 1.50 → 1.52, `uuid` 1.22 → 1.23, `aws-sdk-s3` 1.127 → 1.129, `google-cloud-storage` 1.9 → 1.10, `google-cloud-auth` 1.7 → 1.8, `libc` 0.2.183 → 0.2.186, `rand` 0.9.2 → 0.9.4.
  - Examples: `typescript` 5.9 → 6.0 (Next.js), `puppeteer-core` 24 → 25. `vite` kept on the 7.x line (7.3.5); Vite 8 drops the wasm-bindgen `new URL(..., import.meta.url)` asset reference and breaks the WASM example, so the upgrade is deferred.
  - GitHub Actions: `actions/configure-pages` 5 → 6, `actions/upload-pages-artifact` 4 → 5, `actions/deploy-pages` 4 → 5, `softprops/action-gh-release` 2 → 3.
- Pin integration-test container images to fixed versions (`adobe/s3mock` 4.11.0, `fsouza/fake-gcs-server` 1.54.0, `azurite` 3.35.0, `runn` v1.9.2) to avoid breakage from floating `:latest` tags. s3mock 5.0.0 changed `GET /<bucket>` to return an HTTP error instead of 200, which broke the s3 readiness probe.

### Fixed

- Update `rustls-webpki` 0.103.10 → 0.103.13 to patch RUSTSEC-2026-0098/0099/0104 (name-constraint bypass and CRL-parsing panic) on the modern rustls 0.23 path.
- Ignore RUSTSEC-2026-0098/0099/0104 for the transitive `rustls-webpki` 0.101.7 (legacy AWS SDK `rustls` 0.21 path; upstream has not migrated yet — same rationale as RUSTSEC-2026-0049).

## v0.11.4

### Changed

- Bump MSRV from 1.87 to 1.92.

### Fixed

- Update `aws-lc-sys` 0.38.0 → 0.39.0 to fix CRL Distribution Point scope check logic error and X.509 Name Constraints bypass via wildcard/unicode CN (high severity).
- Update `rustls-webpki` 0.103.9 → 0.103.10 to fix certificate revocation enforcement bug (medium severity).
- Ignore RUSTSEC-2026-0049 for `rustls-webpki` 0.101.7 (transitive dep via AWS SDK's `rustls` 0.21; upstream has not migrated yet).

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
