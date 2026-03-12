# Refactoring Issues Tracker

All issues created from the comprehensive project review.
Use `Closes #N` in PR descriptions to auto-close issues on merge.

## High Priority

| Issue | Title | Category | PR Branch |
|-------|-------|----------|-----------|
| #80 | Eliminate unsafe env::set_var in tests by introducing scoped environment guards | QA | |
| #85 | Split server/mod.rs into routing, handler, and lifecycle modules | App Dev | |
| #86 | Split cli.rs into subcommand-specific submodules | App Dev | |
| #91 | Add request tracing with UUID per request | Infra | |

## Medium Priority

| Issue | Title | Category | PR Branch |
|-------|-------|----------|-----------|
| #81 | Split server_transform.rs into focused test modules | QA | |
| #82 | Add cache corruption recovery and cleanup | QA | |
| #87 | Extract deadline checking into a helper macro or method | App Dev | |
| #88 | Make hardcoded limits configurable via environment variables | App Dev | |
| #92 | Add optional per-IP rate limiting | Infra | |
| #93 | Add size-based cache eviction policy | Infra | |
| #97 | Add architecture guide to CONTRIBUTING.md for new contributors | OSS | |
| #100 | Document design decisions in code comments | Docs | |
| #102 | Add CI check for OpenAPI spec and code synchronization | Docs | |
| #103 | Add explicit overflow checks for image buffer arithmetic | Image | |
| #104 | Validate watermark decoded dimensions against header-declared size | Image | |

## Low Priority

| Issue | Title | Category | PR Branch |
|-------|-------|----------|-----------|
| #83 | Add tests for graceful shutdown and connection draining | QA | |
| #84 | Add tests for remote fetch redirect chains | QA | |
| #89 | Introduce Dimensions wrapper type for (width, height) pairs | App Dev | |
| #90 | Enrich error messages with contextual information | App Dev | |
| #94 | Document synchronous I/O design decision and slow-client implications | Infra | |
| #95 | Add deny.toml for dependency license and vulnerability checking | Infra | |
| #96 | Optimize Cargo.toml keywords and categories for crates.io discoverability | OSS | |
| #98 | Add doc-tests for public API in lib.rs | OSS | |
| #99 | Automate CHANGELOG generation with git-cliff | OSS | |
| #101 | Split README.md into focused documentation pages | Docs | |
| #105 | Make output format preference order configurable in content negotiation | Image | |
| #106 | Add exhaustive EXIF orientation tests for all 8 rotation/flip combinations | Image | |
