//! Adapter implementations for concrete runtimes.

/// Command-line adapter functionality.
#[cfg(feature = "cli")]
pub mod cli;
/// HTTP server adapter functionality.
#[cfg(feature = "server")]
pub mod server;
/// Browser and WebAssembly adapter functionality.
pub mod wasm;
