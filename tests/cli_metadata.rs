use image::codecs::jpeg::JpegDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{ColorType, ImageDecoder, ImageEncoder, Rgb, RgbImage};
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_file_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    std::env::temp_dir().join(format!("truss-cli-metadata-{name}-{unique}.bin"))
}

fn jpeg_with_metadata_bytes(orientation: Option<u16>, icc_profile: Option<&[u8]>) -> Vec<u8> {
    let image = RgbImage::from_pixel(4, 2, Rgb([10, 20, 30]));
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 80);
    if let Some(orientation) = orientation {
        let exif = vec![
            0x49,
            0x49,
            0x2A,
            0x00,
            0x08,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
            0x12,
            0x01,
            0x03,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
            orientation as u8,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        encoder
            .set_exif_metadata(exif)
            .expect("set jpeg exif metadata");
    }
    if let Some(icc_profile) = icc_profile {
        encoder
            .set_icc_profile(icc_profile.to_vec())
            .expect("set jpeg icc profile");
    }
    encoder
        .write_image(&image, 4, 2, ColorType::Rgb8.into())
        .expect("encode jpeg");
    bytes
}

#[test]
fn convert_local_jpeg_can_preserve_exif() {
    let input_path = temp_file_path("input").with_extension("jpg");
    let output_path = temp_file_path("output").with_extension("jpg");
    fs::write(&input_path, jpeg_with_metadata_bytes(Some(6), None)).expect("write jpeg input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--preserve-exif")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let output_bytes = fs::read(&output_path).expect("read jpeg output");
    let mut decoder = JpegDecoder::new(Cursor::new(&output_bytes)).expect("decode jpeg output");
    let exif = decoder
        .exif_metadata()
        .expect("read output exif")
        .expect("retained exif");

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    assert_eq!(decoder.dimensions(), (2, 4));
    assert_eq!(
        Orientation::from_exif_chunk(&exif),
        Some(Orientation::NoTransforms)
    );
}

#[test]
fn convert_local_jpeg_can_keep_icc_profile() {
    let input_path = temp_file_path("input-icc").with_extension("jpg");
    let output_path = temp_file_path("output-icc").with_extension("jpg");
    fs::write(
        &input_path,
        jpeg_with_metadata_bytes(None, Some(b"demo-icc-profile")),
    )
    .expect("write jpeg input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--keep-metadata")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let output_bytes = fs::read(&output_path).expect("read jpeg output");
    let mut decoder = JpegDecoder::new(Cursor::new(&output_bytes)).expect("decode jpeg output");
    let icc_profile = decoder
        .icc_profile()
        .expect("read output icc profile")
        .expect("retained icc profile");

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    assert_eq!(decoder.dimensions(), (4, 2));
    assert_eq!(icc_profile, b"demo-icc-profile".to_vec());
}

/// A JPEG converted to AVIF and back keeps what the AVIF container has a place for.
///
/// The two hops are what makes this a test of the container rather than of one function: the
/// first writes the EXIF and the profile into an AVIF, and the second reads them back out of
/// one, so a writer that produces something only its own reader understands fails here.
#[cfg(feature = "avif")]
#[test]
fn convert_carries_metadata_through_an_avif_round_trip() {
    let input_path = temp_file_path("input-avif").with_extension("jpg");
    let avif_path = temp_file_path("middle-avif").with_extension("avif");
    let output_path = temp_file_path("output-avif").with_extension("jpg");
    fs::write(
        &input_path,
        jpeg_with_metadata_bytes(Some(6), Some(b"demo-icc-profile")),
    )
    .expect("write jpeg input");

    let to_avif = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&avif_path)
        .arg("--keep-metadata")
        .arg("--no-auto-orient")
        .output()
        .expect("run truss convert to avif");
    assert!(to_avif.status.success(), "{to_avif:?}");

    let back_to_jpeg = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&avif_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--keep-metadata")
        .arg("--no-auto-orient")
        .output()
        .expect("run truss convert from avif");
    assert!(back_to_jpeg.status.success(), "{back_to_jpeg:?}");

    let output_bytes = fs::read(&output_path).expect("read jpeg output");
    let mut decoder = JpegDecoder::new(Cursor::new(&output_bytes)).expect("decode jpeg output");
    let icc_profile = decoder
        .icc_profile()
        .expect("read icc")
        .expect("retained icc");
    let exif = decoder
        .exif_metadata()
        .expect("read exif")
        .expect("retained exif");

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&avif_path);
    let _ = fs::remove_file(&output_path);

    assert_eq!(icc_profile, b"demo-icc-profile".to_vec());
    assert_eq!(
        Orientation::from_exif_chunk(&exif),
        Some(Orientation::Rotate90)
    );
}
