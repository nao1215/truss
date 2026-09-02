//! Shared library entry points for the `truss` project.
//!
//! # The public API
//!
//! What this page lists is the API. Nothing else in the crate is, and the sections below
//! say what that means for a version number.
//!
//! While the version is `0.x`, a minor release may change any of it. From `1.0` on, the
//! shape here is what semantic versioning covers: adding an export or a field to a
//! `#[non_exhaustive]` type is a minor release, removing or renaming an export is a major
//! one, and the minimum supported Rust version in `Cargo.toml` is raised only in a minor
//! release. This page is the Rust crate's half of the promise. The other surfaces state
//! their own half beside what they specify: the CLI's subcommands, flags, and exit codes in
//! [`docs/problems.md`], the HTTP server's routes, query vocabulary, and headers in
//! [`docs/api-reference.md`], and each npm package's exports and option keys in its own
//! README. The signed URL format is older than all of them and carries a stronger promise,
//! in [`docs/signed-url-spec.md`], which holds even before `1.0`.
//!
//! [`docs/problems.md`]: https://github.com/nao1215/truss/blob/main/docs/problems.md#compatibility
//! [`docs/api-reference.md`]: https://github.com/nao1215/truss/blob/main/docs/api-reference.md#compatibility
//! [`docs/signed-url-spec.md`]: https://github.com/nao1215/truss/blob/main/docs/signed-url-spec.md#compatibility-policy
//!
//! Everything the crate exports is re-exported here, at the root. The module tree is
//! private, so a path through it is not part of the API and does not compile:
//!
//! ```compile_fail
//! use truss::core::TransformOptions;
//! ```
//!
//! The types that grow as truss grows are `#[non_exhaustive]`, so a field a later version
//! adds is a minor change rather than a breaking one. A caller starts from `default()`, or
//! from a constructor, and assigns:
//!
//! ```compile_fail
//! use truss::TransformOptions;
//!
//! let options = TransformOptions { width: Some(800), ..TransformOptions::default() };
//! ```
//!
//! ```
//! use truss::TransformOptions;
//!
//! let mut options = TransformOptions::default();
//! options.width = Some(800);
//! assert_eq!(options.width, Some(800));
//! ```
//!
//! The value types are exhaustive on purpose, because none of them can gain a field without
//! becoming a different idea: a rectangle is four numbers, a colour is four channels, a size
//! is two, and a quality target is a metric and a threshold. Their literals and their
//! patterns keep working.
//!
//! ```
//! use truss::{CropRegion, Dimensions, Rgba8};
//!
//! let region = CropRegion { x: 0, y: 0, width: 10, height: 10 };
//! let colour = Rgba8 { r: 1, g: 2, b: 3, a: 255 };
//! let size = Dimensions::new(10, 10);
//! assert_eq!((region.width, colour.a, size.height), (10, 255, 10));
//! ```

// The module tree is private and the `pub use` block below is the API. Publishing the
// modules made every `pub` item under them reachable, which was 124 items to advertise 55,
// and a 1.0 freezes whatever is reachable rather than whatever was meant.
mod adapters;
mod codecs;
mod core;
#[cfg(test)]
mod test_support;

#[cfg(feature = "cli")]
pub use adapters::cli::run as run_cli;
#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
pub use adapters::server::StorageBackend;
#[cfg(feature = "azure")]
pub use adapters::server::azure::{AzureContext, build_azure_context};
#[cfg(feature = "gcs")]
pub use adapters::server::gcs::{GcsContext, build_gcs_context};
#[cfg(feature = "s3")]
pub use adapters::server::s3::{S3Context, build_s3_context};
#[cfg(feature = "server")]
pub use adapters::server::{
    LogHandler, LogLevel, ServerConfig, SignedUrlSource, SignedWatermarkParams,
    TransformOptionsPayload, TrustedProxy, bind_addr, serve_once_with_config, serve_with_config,
    sign_public_url, sign_public_url_with_method,
};
pub use codecs::transform;
pub use core::{
    Artifact, ArtifactMetadata, CropRegion, Dimensions, Fit, MAX_OUTPUT_PIXELS, MediaType,
    MetadataKind, OptimizeMode, Position, QualityMetric, RawArtifact, Rgba8, Rotation,
    TargetQuality, TransformError, TransformOptions, TransformRequest, TransformResult,
    TransformWarning, WatermarkInput, sniff_artifact,
};
