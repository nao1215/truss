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
/// GIF is an input-only format here. A single-frame GIF decodes and transforms like any
/// other raster input; an animated one is refused rather than silently reduced to its
/// first frame, because a caller that sends an animation and receives a still image back
/// with exit code 0 has no way to notice the animation was discarded. `truss inspect`
/// still reads animated GIFs and reports `isAnimated`, so a caller can branch on that
/// before converting.
///
/// # Errors
///
/// Returns [`TransformError::UnsupportedOutputMediaType`] if a raster input requests
/// SVG or GIF output, [`TransformError::UnsupportedInputMediaType`] for an animated GIF
/// input, [`TransformError::CapabilityMissing`] if an SVG input is provided but the `svg`
/// feature is not enabled, or any error propagated from the underlying codec.
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

    if request.options.format == Some(MediaType::Gif) {
        return Err(TransformError::UnsupportedOutputMediaType(MediaType::Gif));
    }

    // The rule is about frames, not about one container. Gating it on GIF let an animated
    // WebP, an APNG, and an animated AVIF through, where the decoder kept the first frame
    // and the caller was told nothing.
    if request.input.metadata.frame_count > 1 {
        return Err(TransformError::UnsupportedInputMediaType(format!(
            "animated {} is not supported ({} frames); truss transforms single-frame images only",
            request.input.media_type.as_name(),
            request.input.metadata.frame_count
        )));
    }

    raster::transform_raster(request)
}

#[cfg(test)]
mod tests {
    use super::transform;
    use crate::core::{
        Artifact, ArtifactMetadata, MediaType, TransformError, TransformOptions, TransformRequest,
    };

    fn animated_artifact(media_type: MediaType, signature: &[u8], frame_count: u32) -> Artifact {
        Artifact::new(
            signature.to_vec(),
            media_type,
            ArtifactMetadata {
                width: Some(4),
                height: Some(4),
                frame_count,
                duration: None,
                has_alpha: Some(false),
                orientation: None,
            },
        )
    }

    /// The rule the GIF message states is about frames, not about GIF. A picture that has
    /// more than one of them is refused whatever container it arrived in, or the container
    /// decides whether the caller is told their animation was reduced to a still.
    #[test]
    fn a_multi_frame_input_is_refused_whatever_its_container() {
        let cases: &[(MediaType, &[u8])] = &[
            (MediaType::Gif, b"GIF89a"),
            (MediaType::Png, b"\x89PNG\r\n\x1a\n"),
            (MediaType::Webp, b"RIFF\0\0\0\0WEBP"),
            (MediaType::Avif, b"\0\0\0\x18ftypavis"),
        ];

        for &(media_type, signature) in cases {
            let error = transform(TransformRequest::new(
                animated_artifact(media_type, signature, 4),
                TransformOptions {
                    format: Some(MediaType::Png),
                    ..TransformOptions::default()
                },
            ))
            .expect_err("a multi-frame input should be refused, not reduced to one frame");

            assert!(
                matches!(error, TransformError::UnsupportedInputMediaType(ref message)
                    if message.contains("4 frames")),
                "{media_type:?} was not refused for having frames: {error}"
            );
        }
    }

    fn gif_artifact(frame_count: u32) -> Artifact {
        // The dispatcher decides on the media type and metadata alone, before any decode,
        // so the bytes only need the signature that put this artifact on the GIF path.
        Artifact::new(
            b"GIF89a".to_vec(),
            MediaType::Gif,
            ArtifactMetadata {
                width: Some(4),
                height: Some(4),
                frame_count,
                duration: None,
                has_alpha: Some(false),
                orientation: None,
            },
        )
    }

    #[test]
    fn transform_rejects_an_animated_gif_input() {
        let error = transform(TransformRequest::new(
            gif_artifact(12),
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect_err("an animated gif should be refused, not reduced to one frame");

        match error {
            TransformError::UnsupportedInputMediaType(message) => {
                assert!(
                    message.contains("animated gif") && message.contains("12 frames"),
                    "the error should name the format and the frame count, got: {message}"
                );
            }
            other => panic!("expected UnsupportedInputMediaType, got: {other}"),
        }
    }

    #[test]
    fn transform_rejects_gif_output() {
        let error = transform(TransformRequest::new(
            Artifact::new(
                b"\x89PNG\r\n\x1a\n".to_vec(),
                MediaType::Png,
                ArtifactMetadata::default(),
            ),
            TransformOptions {
                format: Some(MediaType::Gif),
                ..TransformOptions::default()
            },
        ))
        .expect_err("gif output should be refused");

        assert!(
            matches!(
                error,
                TransformError::UnsupportedOutputMediaType(MediaType::Gif)
            ),
            "expected UnsupportedOutputMediaType(Gif), got: {error}"
        );
    }

    #[test]
    fn transform_accepts_a_single_frame_gif_past_the_animation_guard() {
        // The guard must not fire for a still image. The bytes here are not a decodable
        // GIF, so the request is expected to fail later, in the decoder, not at dispatch.
        let error = transform(TransformRequest::new(
            gif_artifact(1),
            TransformOptions {
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            },
        ))
        .expect_err("truncated gif bytes cannot decode");

        assert!(
            matches!(error, TransformError::DecodeFailed(_)),
            "a single-frame gif should reach the decoder, got: {error}"
        );
    }
}
