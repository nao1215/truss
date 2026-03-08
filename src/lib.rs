//! Shared library entry points for the `truss` project.

/// Runtime-specific adapters.
pub mod adapters;
/// Backend codec implementations.
pub mod codecs;
/// Shared Core types and validation logic.
pub mod core;

pub use adapters::server::{
    bind_addr, serve, serve_once, serve_once_with_config, serve_with_config, sign_public_url,
    ServerConfig, SignedUrlSource, DEFAULT_BIND_ADDR, DEFAULT_STORAGE_ROOT,
};
pub use codecs::raster::transform_raster;
pub use codecs::svg::transform_svg;
pub use core::{
    sniff_artifact, Artifact, ArtifactMetadata, Fit, MediaType, MetadataPolicy,
    NormalizedTransformOptions, NormalizedTransformRequest, Position, RawArtifact, Rgba8, Rotation,
    TransformError, TransformOptions, TransformRequest, TransformResult, TransformWarning,
    MetadataKind, MAX_DECODED_PIXELS, MAX_OUTPUT_PIXELS,
};
