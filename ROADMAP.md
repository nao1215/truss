# Roadmap

Items planned for future releases, roughly in priority order.

## High Priority — Structural Improvements

- [#80 — Eliminate unsafe env::set_var in tests by introducing scoped environment guards](https://github.com/nao1215/truss/issues/80)
- [#85 — Split server/mod.rs into routing, handler, and lifecycle modules](https://github.com/nao1215/truss/issues/85)
- [#86 — Split cli.rs into subcommand-specific submodules](https://github.com/nao1215/truss/issues/86)
- [#91 — Add request tracing with UUID per request](https://github.com/nao1215/truss/issues/91)

## Medium Priority — Robustness & Configurability

- [#81 — Split server_transform.rs into focused test modules](https://github.com/nao1215/truss/issues/81)
- [#82 — Add cache corruption recovery and cleanup](https://github.com/nao1215/truss/issues/82)
- [#87 — Extract deadline checking into a helper macro or method](https://github.com/nao1215/truss/issues/87)
- [#88 — Make hardcoded limits configurable via environment variables](https://github.com/nao1215/truss/issues/88)
- [#92 — Add optional per-IP rate limiting](https://github.com/nao1215/truss/issues/92)
- [#93 — Add size-based cache eviction policy](https://github.com/nao1215/truss/issues/93)
- [#97 — Add architecture guide to CONTRIBUTING.md for new contributors](https://github.com/nao1215/truss/issues/97)
- [#100 — Document design decisions in code comments](https://github.com/nao1215/truss/issues/100)
- [#102 — Add CI check for OpenAPI spec and code synchronization](https://github.com/nao1215/truss/issues/102)
- [#103 — Add explicit overflow checks for image buffer arithmetic](https://github.com/nao1215/truss/issues/103)
- [#104 — Validate watermark decoded dimensions against header-declared size](https://github.com/nao1215/truss/issues/104)

## Low Priority — Polish & Documentation

- [#83 — Add tests for graceful shutdown and connection draining](https://github.com/nao1215/truss/issues/83)
- [#84 — Add tests for remote fetch redirect chains](https://github.com/nao1215/truss/issues/84)
- [#89 — Introduce Dimensions wrapper type for (width, height) pairs](https://github.com/nao1215/truss/issues/89)
- [#90 — Enrich error messages with contextual information](https://github.com/nao1215/truss/issues/90)
- [#94 — Document synchronous I/O design decision and slow-client implications](https://github.com/nao1215/truss/issues/94)
- [#95 — Add deny.toml for dependency license and vulnerability checking](https://github.com/nao1215/truss/issues/95)
- [#96 — Optimize Cargo.toml keywords and categories for crates.io discoverability](https://github.com/nao1215/truss/issues/96)
- [#98 — Add doc-tests for public API in lib.rs](https://github.com/nao1215/truss/issues/98)
- [#99 — Automate CHANGELOG generation with git-cliff](https://github.com/nao1215/truss/issues/99)
- [#101 — Split README.md into focused documentation pages](https://github.com/nao1215/truss/issues/101)
- [#105 — Make output format preference order configurable in content negotiation](https://github.com/nao1215/truss/issues/105)
- [#106 — Add exhaustive EXIF orientation tests for all 8 rotation/flip combinations](https://github.com/nao1215/truss/issues/106)

## Health Check Hardening

- [#72 — Add hysteresis to readiness probe resource checks to prevent flapping](https://github.com/nao1215/truss/issues/72)
- [#73 — Consider authentication for /health diagnostic endpoint](https://github.com/nao1215/truss/issues/73)
- [#74 — Cache syscall results in health check endpoints](https://github.com/nao1215/truss/issues/74)
