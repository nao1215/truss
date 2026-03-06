//! Shared library entry points for the `truss` project.

/// Runtime-specific adapters.
pub mod adapters;
/// Backend codec implementations.
pub mod codecs;
/// Shared Core types and validation logic.
pub mod core;

#[cfg(feature = "server")]
pub use adapters::server::{
    DEFAULT_BIND_ADDR, DEFAULT_STORAGE_ROOT, ServerConfig, SignedUrlSource, bind_addr, serve,
    serve_once, serve_once_with_config, serve_with_config, sign_public_url,
};
pub use codecs::raster::transform_raster;
#[cfg(feature = "svg")]
pub use codecs::svg::transform_svg;
pub use core::{
    Artifact, ArtifactMetadata, Fit, MAX_DECODED_PIXELS, MAX_OUTPUT_PIXELS, MediaType,
    MetadataKind, MetadataPolicy, NormalizedTransformOptions, NormalizedTransformRequest, Position,
    RawArtifact, Rgba8, Rotation, TransformError, TransformOptions, TransformRequest,
    TransformResult, TransformWarning, resolve_metadata_flags, sniff_artifact,
};
