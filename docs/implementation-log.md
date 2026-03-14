# Implementation Log

- 2026-03-08: Expanded `docs/openapi.yaml` with more human-readable guidance: clearer top-level API description, endpoint selection notes, authentication context, request examples, richer parameter descriptions, and schema/response explanations for Swagger/OpenAPI readers.
- 2026-03-08: Added the shared implementation log file required by `AGENTS.md`. Remaining follow-up: keep README, OpenAPI, and UI wording synchronized when supported formats or endpoint semantics change.
- 2026-03-08: Updated `Cargo.toml` for the new non-published posture: added `publish = false` and removed `docs.rs` / crates.io-oriented metadata that no longer reflects the project's distribution model.
- 2026-03-08: Corrected the manifest after clarifying distribution intent: the package remains publishable for the `truss` command, so `publish = false` was removed. The package description was rewritten to avoid advertising the crate as a public library while keeping crates.io metadata for command distribution.
