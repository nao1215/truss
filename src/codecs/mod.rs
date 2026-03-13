//! Backend codec implementations.

use crate::core::{MediaType, TransformError, TransformRequest, TransformResult};

/// Raster image decoding and encoding support.
pub mod raster;

/// SVG sanitization and rasterization support.
#[cfg(feature = "svg")]
pub mod svg;

/// Dispatches a transform request to the appropriate codec based on the input media type.
///
/// This is the primary entry point for all image transformations. It routes SVG
/// inputs to [`svg::transform_svg`] and raster inputs to [`raster::transform_raster`],
/// and rejects unsupported conversions (e.g., raster-to-SVG output) with a clear error.
///
/// # Errors
///
/// Returns [`TransformError::UnsupportedOutputMediaType`] if a raster input requests
/// SVG output, [`TransformError::CapabilityMissing`] if an SVG input is provided but
/// the `svg` feature is not enabled, or any error propagated from the underlying codec.
#[must_use = "this function returns the transform result without side effects"]
pub fn transform(request: TransformRequest) -> Result<TransformResult, TransformError> {
    if request.input.media_type == MediaType::Svg {
        #[cfg(feature = "svg")]
        {
            return svg::transform_svg(request);
        }
        #[cfg(not(feature = "svg"))]
        {
            let _ = request;
            return Err(TransformError::CapabilityMissing(
                "SVG processing is not enabled in this build".to_string(),
            ));
        }
    }

    if request.options.format == Some(MediaType::Svg) {
        return Err(TransformError::UnsupportedOutputMediaType(MediaType::Svg));
    }

    raster::transform_raster(request)
}
