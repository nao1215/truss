//! The crate's public API, named once so a change to it has to be deliberate.
//!
//! `src/lib.rs` used to publish the module tree beside a curated `pub use` list, which made
//! the list a summary rather than the API: 124 items were reachable to advertise 55. The
//! modules are private now, so this file is the only place the whole export list is written
//! down, and a symbol that disappears fails to compile here rather than in a dependent.
//!
//! Adding an export is a minor version and adding a line here; removing one is a major
//! version and removing a line. Neither can happen by accident.

#[test]
fn every_exported_symbol_is_reachable_from_the_crate_root() {
    // Types and values that describe an image.
    #[allow(unused_imports)]
    use truss::{
        Artifact, ArtifactMetadata, CropRegion, Dimensions, MAX_DECODED_PIXELS, MAX_OUTPUT_PIXELS,
        MAX_WATERMARK_PIXELS, MediaType, MetadataKind, RawArtifact, Rgba8, sniff_artifact,
    };
    // The transform vocabulary.
    #[allow(unused_imports)]
    use truss::{
        Fit, OptimizeMode, Position, QualityMetric, Rotation, TargetQuality, TransformError,
        TransformOptions, TransformRequest, TransformResult, TransformWarning, WatermarkInput,
        resolve_metadata_flags, transform,
    };

    // A value from each group, so the test exercises the paths rather than only naming them.
    assert_eq!(MediaType::Png.as_name(), "png");
    assert_eq!(Rotation::DEG_90.as_degrees(), 90);
    const { assert!(MAX_OUTPUT_PIXELS > 0) };
    const { assert!(MAX_DECODED_PIXELS > 0) };
    const { assert!(MAX_WATERMARK_PIXELS > 0) };
    assert_eq!(Dimensions::new(4, 3).width, 4);
    assert!(TransformOptions::default().strip_metadata);
}

#[cfg(feature = "cli")]
#[test]
fn the_cli_entry_point_is_reachable_from_the_crate_root() {
    // `src/main.rs` is a separate target and therefore an external consumer of the library,
    // so the entry point needs a path that survives the module tree being private.
    #[allow(unused_imports)]
    use truss::run_cli;
}

#[cfg(feature = "server")]
#[test]
fn every_exported_server_symbol_is_reachable_from_the_crate_root() {
    #[allow(unused_imports)]
    use truss::{
        LogHandler, LogLevel, ServerConfig, SignedUrlSource, SignedWatermarkParams,
        TransformOptionsPayload, TrustedProxy, bind_addr, serve, serve_once,
        serve_once_with_config, serve_with_config, sign_public_url, sign_public_url_with_method,
    };

    // `bind_addr` resolves `TRUSS_BIND_ADDR` against the default, which is the only reason
    // the default itself does not need to be exported.
    assert!(!bind_addr().is_empty());
    // `TrustedProxy` and `LogHandler` are named in public fields of `ServerConfig`, so they
    // have to be nameable too or the field is one a caller can read and not describe.
    let config = ServerConfig::new(std::env::temp_dir(), None);
    let _: &Vec<TrustedProxy> = &config.trusted_proxies;
    let _: &Option<LogHandler> = &config.log_handler;
}

#[cfg(any(feature = "s3", feature = "gcs", feature = "azure"))]
#[test]
fn every_exported_storage_symbol_is_reachable_from_the_crate_root() {
    #[allow(unused_imports)]
    use truss::StorageBackend;
    #[cfg(feature = "azure")]
    #[allow(unused_imports)]
    use truss::{AzureContext, build_azure_context};
    #[cfg(feature = "gcs")]
    #[allow(unused_imports)]
    use truss::{GcsContext, build_gcs_context};
    #[cfg(feature = "s3")]
    #[allow(unused_imports)]
    use truss::{S3Context, build_s3_context};
}
