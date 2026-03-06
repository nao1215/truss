//! Shared library entry points for the `truss` project.

/// Runtime-specific adapters.
pub mod adapters;
/// Backend codec implementations.
pub mod codecs;
/// Shared Core types and validation logic.
pub mod core;

pub use adapters::server::{
    bind_addr, serve, serve_once, serve_once_with_config, serve_with_config, ServerConfig,
    DEFAULT_BIND_ADDR, DEFAULT_STORAGE_ROOT,
};
pub use codecs::raster::transform_raster;
pub use core::{
    sniff_artifact, Artifact, ArtifactMetadata, Fit, MediaType, MetadataPolicy,
    NormalizedTransformOptions, NormalizedTransformRequest, Position, RawArtifact, Rgba8, Rotation,
    TransformError, TransformOptions, TransformRequest,
};
