//! The input sniffer on arbitrary bytes.
//!
//! Beyond not panicking: the answer is deterministic, declaring the detected type is accepted
//! with the same answer, declaring any other type is refused, and the metadata it reports is
//! internally consistent.
#![no_main]

use libfuzzer_sys::fuzz_target;
use truss::{MediaType, RawArtifact, sniff_artifact};

const ALL_TYPES: [MediaType; 7] = [
    MediaType::Jpeg,
    MediaType::Png,
    MediaType::Webp,
    MediaType::Svg,
    MediaType::Bmp,
    MediaType::Tiff,
    MediaType::Gif,
];

fuzz_target!(|data: &[u8]| {
    let first = sniff_artifact(RawArtifact::new(data.to_vec(), None));
    let again = sniff_artifact(RawArtifact::new(data.to_vec(), None));
    match (&first, &again) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "the sniffer is not deterministic"),
        (Err(_), Err(_)) => {}
        _ => panic!("the sniffer is not deterministic: {first:?} vs {again:?}"),
    }

    let Ok(artifact) = first else {
        // Undetected bytes are refused whatever type the caller declares.
        for declared in ALL_TYPES {
            assert!(
                sniff_artifact(RawArtifact::new(data.to_vec(), Some(declared))).is_err(),
                "declaring {declared:?} made undetectable bytes acceptable"
            );
        }
        return;
    };

    assert_eq!(artifact.bytes, data, "the sniffer changed the bytes");
    let metadata = &artifact.metadata;
    assert!(metadata.frame_count >= 1, "no frames: {metadata:?}");
    // A zero dimension is reported as the header states it (a JPEG SOF may say 0x0), so
    // it is not asserted against here; the transform target checks what decoding makes of it.
    // The oriented size is the stored size, transposed at most.
    if let (Some(stored), Some(oriented)) = (metadata.dimensions(), metadata.oriented_dimensions())
    {
        assert!(
            oriented == stored
                || (oriented.width == stored.height && oriented.height == stored.width),
            "oriented size {oriented:?} is not a transpose of {stored:?}"
        );
    }

    for declared in ALL_TYPES {
        let result = sniff_artifact(RawArtifact::new(data.to_vec(), Some(declared)));
        if declared == artifact.media_type {
            assert_eq!(
                result.as_ref().ok(),
                Some(&artifact),
                "declaring the detected type changed the answer"
            );
        } else {
            assert!(
                result.is_err(),
                "declared {declared:?} accepted for {:?} bytes",
                artifact.media_type
            );
        }
    }
});
