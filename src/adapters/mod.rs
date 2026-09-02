//! Adapter implementations for concrete runtimes.

/// Command-line adapter functionality.
#[cfg(feature = "cli")]
pub mod cli;
/// HTTP server adapter functionality.
#[cfg(feature = "server")]
pub mod server;
/// Browser and WebAssembly adapter functionality.
///
/// Compiled in every configuration on purpose, and not behind the `wasm` feature: gating it
/// meant an ordinary `cargo test` never built it, and it drifted from the vocabulary the
/// other adapters share. Its own tests run everywhere for the same reason.
///
/// Its entry points are reached by the `wasm_bindgen` glue, which only exists under the
/// feature, so without the feature nothing calls them by construction. That is what the
/// allow says; it is not a licence for dead code inside the module, which the `wasm` build
/// still reports.
#[cfg_attr(not(feature = "wasm"), allow(dead_code))]
pub mod wasm;
