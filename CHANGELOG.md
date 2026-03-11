# Changelog

## Unreleased

### Added

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

### Changed

- Watermark fetch is deferred until concurrency slot is acquired (two-phase validation + fetch).
- SVG sources with watermark requests are rejected early with 400.
- Watermark fetch errors are sanitized; detailed errors logged server-side only.
- Cache key normalization uses parsed `Position` for consistent hashing.
- WASM UI: blur values below 0.1 treated as no blur; "Blur sigma" label simplified to "Blur".
- WASM UI: `.is-busy` scoped to interactive elements instead of entire page.
- WASM UI: download filename includes `-watermarked` suffix when applicable.

### Fixed

- Accessibility: `role="alert"` on error box, `:focus-within` on dropzones, `name` attributes on watermark inputs, `<noscript>` fallback.

## v0.5.0

### Added

- Prometheus `/metrics` endpoint with histograms (HTTP request duration, transform duration, storage duration) and error counters.
- Prometheus metrics documentation (`doc/prometheus.md`).
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
