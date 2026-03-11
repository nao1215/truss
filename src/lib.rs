//! Shared library entry points for the `truss` project.

/// Runtime-specific adapters.
pub mod adapters;
/// Backend codec implementations.
pub mod codecs;
/// Shared Core types and validation logic.
pub mod core;

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
    DEFAULT_BIND_ADDR, DEFAULT_STORAGE_ROOT, ServerConfig, SignedUrlSource, SignedWatermarkParams,
    TransformOptionsPayload, bind_addr, serve, serve_once, serve_once_with_config,
    serve_with_config, sign_public_url,
};
pub use codecs::raster::transform_raster;
#[cfg(feature = "svg")]
pub use codecs::svg::transform_svg;
pub use core::{
    Artifact, ArtifactMetadata, CropRegion, Fit, MAX_DECODED_PIXELS, MAX_OUTPUT_PIXELS,
    MAX_WATERMARK_PIXELS, MediaType, MetadataKind, MetadataPolicy, NormalizedTransformOptions,
    NormalizedTransformRequest, Position, RawArtifact, Rgba8, Rotation, TransformError,
    TransformOptions, TransformRequest, TransformResult, TransformWarning, WatermarkInput,
    resolve_metadata_flags, sniff_artifact,
};
