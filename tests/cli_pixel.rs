use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ColorType, GenericImageView, ImageEncoder, ImageReader, Rgba, RgbaImage};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_file_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    std::env::temp_dir().join(format!("truss-cli-pixel-{name}-{unique}.bin"))
}

/// Create a 4x2 PNG where left half is red and right half is blue.
fn create_red_blue_4x2_png() -> Vec<u8> {
    let mut img = RgbaImage::new(4, 2);
    let red = Rgba([255, 0, 0, 255]);
    let blue = Rgba([0, 0, 255, 255]);
    for y in 0..2 {
        for x in 0..4 {
            if x < 2 {
                img.put_pixel(x, y, red);
            } else {
                img.put_pixel(x, y, blue);
            }
        }
    }
    let mut bytes = Vec::new();
    let encoder = PngEncoder::new(&mut bytes);
    encoder
        .write_image(&img, 4, 2, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

/// Create a solid-color 4x2 PNG with all pixels blue.
fn create_solid_blue_4x2_png() -> Vec<u8> {
    let img = RgbaImage::from_pixel(4, 2, Rgba([0, 0, 255, 255]));
    let mut bytes = Vec::new();
    let encoder = PngEncoder::new(&mut bytes);
    encoder
        .write_image(&img, 4, 2, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

/// Create a 2x2 solid green PNG.
fn create_solid_green_2x2_png() -> Vec<u8> {
    let img = RgbaImage::from_pixel(2, 2, Rgba([0, 255, 0, 255]));
    let mut bytes = Vec::new();
    let encoder = PngEncoder::new(&mut bytes);
    encoder
        .write_image(&img, 2, 2, ColorType::Rgba8.into())
        .expect("encode png");
    bytes
}

/// Create a 4x2 JPEG with EXIF Orientation=6 (90° CW rotation).
fn create_4x2_jpeg_with_orientation6() -> Vec<u8> {
    use image::{Rgb, RgbImage};
    let img = RgbImage::from_pixel(4, 2, Rgb([10, 20, 30]));
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 90);
    let exif = vec![
        0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    encoder
        .set_exif_metadata(exif)
        .expect("set jpeg exif metadata");
    encoder
        .write_image(&img, 4, 2, ColorType::Rgb8.into())
        .expect("encode jpeg");
    bytes
}

/// Create a 4x2 JPEG with EXIF Orientation=1 (no rotation).
fn create_4x2_jpeg_with_orientation1() -> Vec<u8> {
    use image::{Rgb, RgbImage};
    let img = RgbImage::from_pixel(4, 2, Rgb([10, 20, 30]));
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut bytes, 90);
    let exif = vec![
        0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00, 0x01,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    encoder
        .set_exif_metadata(exif)
        .expect("set jpeg exif metadata");
    encoder
        .write_image(&img, 4, 2, ColorType::Rgb8.into())
        .expect("encode jpeg");
    bytes
}

// ---------------------------------------------------------------------------
// Test 1: fit=cover + position pixel verification
// ---------------------------------------------------------------------------

#[test]
fn fit_cover_position_left_keeps_red() {
    let input_path = temp_file_path("cover-left-in").with_extension("png");
    let output_path = temp_file_path("cover-left-out").with_extension("png");
    fs::write(&input_path, create_red_blue_4x2_png()).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--width")
        .arg("2")
        .arg("--height")
        .arg("2")
        .arg("--fit")
        .arg("cover")
        .arg("--position")
        .arg("left")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let result = image::open(&output_path).expect("open output").to_rgba8();

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    // With position=left on a 4x2→2x2 cover crop, the left portion is kept,
    // so pixel (0,0) should be red.
    let px = result.get_pixel(0, 0);
    assert!(
        px[0] > 200 && px[1] < 50 && px[2] < 50,
        "expected red-ish pixel at (0,0), got {px:?}"
    );
}

#[test]
fn fit_cover_position_right_keeps_blue() {
    let input_path = temp_file_path("cover-right-in").with_extension("png");
    let output_path = temp_file_path("cover-right-out").with_extension("png");
    fs::write(&input_path, create_red_blue_4x2_png()).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--width")
        .arg("2")
        .arg("--height")
        .arg("2")
        .arg("--fit")
        .arg("cover")
        .arg("--position")
        .arg("right")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let result = image::open(&output_path).expect("open output").to_rgba8();

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    // With position=right on a 4x2→2x2 cover crop, the right portion is kept,
    // so pixel (0,0) should be blue.
    let px = result.get_pixel(0, 0);
    assert!(
        px[0] < 50 && px[1] < 50 && px[2] > 200,
        "expected blue-ish pixel at (0,0), got {px:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: fit=contain + background padding color
// ---------------------------------------------------------------------------

#[test]
fn fit_contain_background_padding_color() {
    let input_path = temp_file_path("contain-bg-in").with_extension("png");
    let output_path = temp_file_path("contain-bg-out").with_extension("png");
    fs::write(&input_path, create_solid_blue_4x2_png()).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--width")
        .arg("4")
        .arg("--height")
        .arg("4")
        .arg("--fit")
        .arg("contain")
        .arg("--background")
        .arg("ff0000")
        .arg("--format")
        .arg("png")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let result = image::open(&output_path).expect("open output").to_rgba8();

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    // Output should be 4x4.
    assert_eq!(
        result.dimensions(),
        (4, 4),
        "expected 4x4 output, got {:?}",
        result.dimensions()
    );

    // Pixel at (0,0) should be red padding (top row).
    let top_left = result.get_pixel(0, 0);
    assert!(
        top_left[0] > 200 && top_left[1] < 50 && top_left[2] < 50,
        "expected red padding at (0,0), got {top_left:?}"
    );

    // Pixel at (0,3) should be red padding (bottom row).
    let bottom_left = result.get_pixel(0, 3);
    assert!(
        bottom_left[0] > 200 && bottom_left[1] < 50 && bottom_left[2] < 50,
        "expected red padding at (0,3), got {bottom_left:?}"
    );

    // Pixel at (0,1) should be blue (image content, centered vertically).
    let content = result.get_pixel(0, 1);
    assert!(
        content[0] < 50 && content[1] < 50 && content[2] > 200,
        "expected blue content at (0,1), got {content:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: --no-auto-orient verification
// ---------------------------------------------------------------------------

#[test]
fn auto_orient_rotates_dimensions() {
    let input_path = temp_file_path("orient-auto-in").with_extension("jpg");
    let output_path = temp_file_path("orient-auto-out").with_extension("png");
    fs::write(&input_path, create_4x2_jpeg_with_orientation6()).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--format")
        .arg("png")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let result = ImageReader::open(&output_path)
        .expect("open output")
        .decode()
        .expect("decode output");

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    // Default auto-orient with orientation=6 (90° CW): 4x2 becomes 2x4.
    assert_eq!(
        result.dimensions(),
        (2, 4),
        "auto-orient should rotate 4x2 to 2x4, got {:?}",
        result.dimensions()
    );
}

#[test]
fn no_auto_orient_preserves_dimensions() {
    let input_path = temp_file_path("orient-no-in").with_extension("jpg");
    let output_path = temp_file_path("orient-no-out").with_extension("png");
    fs::write(&input_path, create_4x2_jpeg_with_orientation6()).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--format")
        .arg("png")
        .arg("--no-auto-orient")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let result = ImageReader::open(&output_path)
        .expect("open output")
        .decode()
        .expect("decode output");

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    // With --no-auto-orient, original 4x2 dimensions are preserved.
    assert_eq!(
        result.dimensions(),
        (4, 2),
        "--no-auto-orient should keep 4x2, got {:?}",
        result.dimensions()
    );
}

// ---------------------------------------------------------------------------
// Test 4: CLI warning output for --keep-metadata --format webp --quality
// ---------------------------------------------------------------------------

#[test]
fn keep_metadata_webp_emits_warning() {
    let input_path = temp_file_path("warn-meta-in").with_extension("jpg");
    let output_path = temp_file_path("warn-meta-out").with_extension("webp");
    fs::write(&input_path, create_4x2_jpeg_with_orientation1()).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--keep-metadata")
        .arg("--format")
        .arg("webp")
        .arg("--quality")
        .arg("80")
        .output()
        .expect("run truss convert");

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    // Command should succeed even if metadata cannot be preserved.
    assert!(
        output.status.success(),
        "expected exit code 0, got {output:?}"
    );

    // Stderr should contain a warning about metadata being dropped.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("warning"),
        "expected stderr to contain 'warning', got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: fit=inside never upscales
// ---------------------------------------------------------------------------

#[test]
fn fit_inside_does_not_upscale() {
    let input_path = temp_file_path("inside-noup-in").with_extension("png");
    let output_path = temp_file_path("inside-noup-out").with_extension("png");
    fs::write(&input_path, create_solid_green_2x2_png()).expect("write input");

    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("--width")
        .arg("4")
        .arg("--height")
        .arg("4")
        .arg("--fit")
        .arg("inside")
        .arg("--background")
        .arg("ff0000")
        .arg("--format")
        .arg("png")
        .output()
        .expect("run truss convert");

    assert!(output.status.success(), "{output:?}");

    let result = image::open(&output_path).expect("open output").to_rgba8();

    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);

    // fit=inside pads to target box but does not upscale the content.
    // Output is 4x4 with 2x2 green content centered and red padding.
    assert_eq!(result.dimensions(), (4, 4));

    // Corner pixel (0,0) should be red padding, not green.
    let corner = result.get_pixel(0, 0);
    assert!(
        corner[0] > 200 && corner[1] < 50 && corner[2] < 50,
        "expected red padding at corner (0,0), got {corner:?}"
    );

    // Center pixel should be green (content was not upscaled, placed at center).
    let center = result.get_pixel(1, 1);
    assert!(
        center[1] > 200 && center[0] < 50 && center[2] < 50,
        "expected green content at center, got {center:?}"
    );
}
