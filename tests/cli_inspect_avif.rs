use image::codecs::avif::AvifEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_file_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    std::env::temp_dir().join(format!("truss-cli-inspect-avif-{name}-{unique}.bin"))
}

fn avif_bytes(width: u32, height: u32, fill: Rgba<u8>) -> Vec<u8> {
    let image = RgbaImage::from_pixel(width, height, fill);
    let mut bytes = Vec::new();
    AvifEncoder::new(&mut bytes)
        .write_image(&image, width, height, ColorType::Rgba8.into())
        .expect("encode avif");
    bytes
}

#[test]
fn inspect_local_avif_reports_dimensions_and_alpha() {
    let input_path = temp_file_path("input").with_extension("avif");
    fs::write(&input_path, avif_bytes(6, 4, Rgba([10, 20, 30, 0]))).expect("write avif input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("inspect")
        .arg(&input_path)
        .output()
        .expect("run truss inspect");

    let _ = fs::remove_file(&input_path);

    assert!(output.status.success(), "{output:?}");

    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"format\": \"avif\""));
    assert!(stdout.contains("\"width\": 6"));
    assert!(stdout.contains("\"height\": 4"));
    assert!(stdout.contains("\"hasAlpha\": true"));
}
