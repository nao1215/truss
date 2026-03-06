//! Backend codec implementations.

/// Raster image decoding and encoding support.
pub mod raster;

/// SVG sanitization and rasterization support.
#[cfg(feature = "svg")]
pub mod svg;
