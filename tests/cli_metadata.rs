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
        .arg("--keep-metadata")
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
