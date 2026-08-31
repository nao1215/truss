use std::fs;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
use truss::{
    Artifact, Fit, MediaType, Position, RawArtifact, TransformOptions, TransformRequest,
    sniff_artifact, transform,
};

/// The size every case that touches pixels runs at, and the size its label names.
const BENCH_WIDTH: u32 = 640;
const BENCH_HEIGHT: u32 = 427;
const BENCH_LABEL: &str = "640x427";

fn fixture(name: &str) -> Vec<u8> {
    fs::read(format!("integration/fixtures/{name}")).expect("fixture file must exist")
}

/// An image with something in it, at the size the caller asks for.
///
/// The fixtures under `integration/fixtures` are 4 by 3 pixels, which is what the tests that
/// read them are about: whether the bytes are a valid file. A benchmark is about the pixels,
/// and twelve of them measure per-call setup rather than the work. The content is a gradient
/// with a fine texture over it, because the encoders are content-sensitive and a flat or
/// perfectly smooth image is the cheapest thing they will ever be handed.
fn bench_image(width: u32, height: u32) -> RgbaImage {
    let mut image = RgbaImage::new(width, height);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let fx = f32::from(u16::try_from(x).unwrap_or(u16::MAX)) / width as f32;
        let fy = f32::from(u16::try_from(y).unwrap_or(u16::MAX)) / height as f32;
        let texture = ((x * 7 + y * 13) % 32) as f32;
        let r = (40.0 + 180.0 * fx * fx + texture).min(255.0) as u8;
        let g = (30.0 + 200.0 * fy + texture * 0.5).min(255.0) as u8;
        let b = (200.0 - 150.0 * (fx + fy) * 0.5 + texture * 0.25).clamp(0.0, 255.0) as u8;
        *pixel = Rgba([r, g, b, 255]);
    }
    image
}

/// The benchmark's JPEG source, encoded once outside the measured loop.
fn bench_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = bench_image(width, height);
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 90)
        .encode_image(&image)
        .expect("encode benchmark jpeg");
    assert_dimensions(&bytes, Some(MediaType::Jpeg), width, height);
    bytes
}

/// The benchmark's PNG source, used where a case needs a second image such as a watermark.
fn bench_png(width: u32, height: u32) -> Vec<u8> {
    let image = bench_image(width, height);
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, width, height, ColorType::Rgba8.into())
        .expect("encode benchmark png");
    assert_dimensions(&bytes, Some(MediaType::Png), width, height);
    bytes
}

/// Holds a case's label to the image it actually runs on.
///
/// Every size in a `BenchmarkId` used to be written out by hand next to a fixture of a
/// different size, so the suite reported microseconds for what it called a 640x427 encode.
/// Reading the size back out of the encoded bytes is what keeps the two from drifting again.
fn assert_dimensions(bytes: &[u8], media_type: Option<MediaType>, width: u32, height: u32) {
    let artifact = make_artifact(bytes.to_vec(), media_type);
    assert_eq!(
        (artifact.metadata.width, artifact.metadata.height),
        (Some(width), Some(height)),
        "benchmark source must be the size its label names"
    );
}

fn make_artifact(bytes: Vec<u8>, media_type: Option<MediaType>) -> Artifact {
    sniff_artifact(RawArtifact::new(bytes, media_type)).expect("sniff must succeed")
}

// ---------------------------------------------------------------------------
// Format conversion: JPEG -> various output formats
// ---------------------------------------------------------------------------
fn bench_format_conversion(c: &mut Criterion) {
    let jpeg_bytes = bench_jpeg(BENCH_WIDTH, BENCH_HEIGHT);
    let targets: &[(&str, MediaType, Option<u8>)] = &[
        ("jpeg_to_png", MediaType::Png, None),
        ("jpeg_to_webp", MediaType::Webp, Some(80)),
        #[cfg(feature = "avif")]
        ("jpeg_to_avif", MediaType::Avif, Some(80)),
    ];

    let mut group = c.benchmark_group("format_conversion");
    // An AVIF encode of this size is measured in tenths of a second, so the default hundred
    // samples would put the group into the minutes on its own.
    group.sample_size(20);
    for (label, target_format, quality) in targets {
        group.bench_with_input(
            BenchmarkId::new(*label, BENCH_LABEL),
            &jpeg_bytes,
            |b, data| {
                b.iter(|| {
                    let input = make_artifact(data.clone(), Some(MediaType::Jpeg));
                    let opts = TransformOptions {
                        format: Some(*target_format),
                        quality: *quality,
                        ..TransformOptions::default()
                    };
                    let _ = transform(TransformRequest::new(input, opts)).unwrap();
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Resize: different dimensions with cover fit
// ---------------------------------------------------------------------------
fn bench_resize(c: &mut Criterion) {
    let jpeg_bytes = bench_jpeg(BENCH_WIDTH, BENCH_HEIGHT);
    let sizes: &[(u32, u32)] = &[(100, 100), (400, 300), (800, 600), (1920, 1080)];

    let mut group = c.benchmark_group("resize");
    for &(w, h) in sizes {
        let label = format!("{w}x{h}");
        group.bench_with_input(BenchmarkId::new("cover", &label), &jpeg_bytes, |b, data| {
            b.iter(|| {
                let input = make_artifact(data.clone(), Some(MediaType::Jpeg));
                let opts = TransformOptions {
                    width: Some(w),
                    height: Some(h),
                    fit: Some(Fit::Cover),
                    format: Some(MediaType::Jpeg),
                    quality: Some(80),
                    ..TransformOptions::default()
                };
                let _ = transform(TransformRequest::new(input, opts)).unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Fit modes: contain, cover, fill, inside at the same target size
// ---------------------------------------------------------------------------
fn bench_fit_modes(c: &mut Criterion) {
    let jpeg_bytes = bench_jpeg(BENCH_WIDTH, BENCH_HEIGHT);
    let modes: &[(&str, Fit)] = &[
        ("contain", Fit::Contain),
        ("cover", Fit::Cover),
        ("fill", Fit::Fill),
        ("inside", Fit::Inside),
    ];

    let mut group = c.benchmark_group("fit_modes");
    for (label, fit) in modes {
        group.bench_with_input(
            BenchmarkId::new(*label, "300x300"),
            &jpeg_bytes,
            |b, data| {
                b.iter(|| {
                    let input = make_artifact(data.clone(), Some(MediaType::Jpeg));
                    let opts = TransformOptions {
                        width: Some(300),
                        height: Some(300),
                        fit: Some(*fit),
                        format: Some(MediaType::Jpeg),
                        quality: Some(80),
                        ..TransformOptions::default()
                    };
                    let _ = transform(TransformRequest::new(input, opts)).unwrap();
                });
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Filters: blur and sharpen
// ---------------------------------------------------------------------------
fn bench_filters(c: &mut Criterion) {
    let jpeg_bytes = bench_jpeg(BENCH_WIDTH, BENCH_HEIGHT);

    let mut group = c.benchmark_group("filters");
    group.bench_with_input(
        BenchmarkId::new("blur", "sigma_5"),
        &jpeg_bytes,
        |b, data| {
            b.iter(|| {
                let input = make_artifact(data.clone(), Some(MediaType::Jpeg));
                let opts = TransformOptions {
                    blur: Some(5.0),
                    format: Some(MediaType::Jpeg),
                    quality: Some(80),
                    ..TransformOptions::default()
                };
                let _ = transform(TransformRequest::new(input, opts)).unwrap();
            });
        },
    );

    group.bench_with_input(
        BenchmarkId::new("sharpen", "sigma_3"),
        &jpeg_bytes,
        |b, data| {
            b.iter(|| {
                let input = make_artifact(data.clone(), Some(MediaType::Jpeg));
                let opts = TransformOptions {
                    sharpen: Some(3.0),
                    format: Some(MediaType::Jpeg),
                    quality: Some(80),
                    ..TransformOptions::default()
                };
                let _ = transform(TransformRequest::new(input, opts)).unwrap();
            });
        },
    );
    group.finish();
}

// ---------------------------------------------------------------------------
// Watermark: overlay a small image onto the main image
// ---------------------------------------------------------------------------
fn bench_watermark(c: &mut Criterion) {
    let jpeg_bytes = bench_jpeg(BENCH_WIDTH, BENCH_HEIGHT);
    // A watermark is a small image over a large one; sizing it to the source would measure
    // a composite that no caller asks for.
    let watermark_bytes = bench_png(BENCH_WIDTH / 8, BENCH_HEIGHT / 8);

    let mut group = c.benchmark_group("watermark");
    group.bench_function("bottom_right", |b| {
        b.iter(|| {
            let input = make_artifact(jpeg_bytes.clone(), Some(MediaType::Jpeg));
            let wm_artifact = make_artifact(watermark_bytes.clone(), Some(MediaType::Png));
            let wm = truss::WatermarkInput {
                image: wm_artifact,
                position: Position::BottomRight,
                opacity: 50,
                margin: 10,
            };
            let opts = TransformOptions {
                width: Some(800),
                height: Some(600),
                fit: Some(Fit::Cover),
                format: Some(MediaType::Jpeg),
                quality: Some(80),
                ..TransformOptions::default()
            };
            let _ = transform(TransformRequest::with_watermark(input, opts, wm)).unwrap();
        });
    });
    group.finish();
}

// ---------------------------------------------------------------------------
// sniff_artifact: format detection and metadata extraction
// ---------------------------------------------------------------------------
fn bench_sniff(c: &mut Criterion) {
    let fixtures: &[(&str, Option<MediaType>)] = &[
        ("sample.jpg", Some(MediaType::Jpeg)),
        ("sample.png", Some(MediaType::Png)),
        ("sample.bmp", Some(MediaType::Bmp)),
    ];

    let mut group = c.benchmark_group("sniff_artifact");
    for (name, media_type) in fixtures {
        let data = fixture(name);
        group.bench_with_input(BenchmarkId::from_parameter(name), &data, |b, data| {
            b.iter(|| {
                let _ = sniff_artifact(RawArtifact::new(data.clone(), *media_type)).unwrap();
            });
        });
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// PNG input: different transparency scenarios
// ---------------------------------------------------------------------------
fn bench_png_variants(c: &mut Criterion) {
    let variants: &[&str] = &["sample.png", "transparent.png", "semitransparent.png"];

    let mut group = c.benchmark_group("png_to_jpeg");
    for name in variants {
        let data = fixture(name);
        group.bench_with_input(BenchmarkId::from_parameter(name), &data, |b, data| {
            b.iter(|| {
                let input = make_artifact(data.clone(), Some(MediaType::Png));
                let opts = TransformOptions {
                    width: Some(400),
                    format: Some(MediaType::Jpeg),
                    quality: Some(80),
                    ..TransformOptions::default()
                };
                let _ = transform(TransformRequest::new(input, opts)).unwrap();
            });
        });
    }
    group.finish();
}

#[cfg(not(feature = "svg"))]
fn bench_svg(_c: &mut Criterion) {}

#[cfg(feature = "svg")]
fn bench_svg(c: &mut Criterion) {
    let svg_bytes = fixture("svg-minimal.svg");

    let mut group = c.benchmark_group("svg");
    group.bench_function("sanitize_passthrough", |b| {
        b.iter(|| {
            let input = make_artifact(svg_bytes.clone(), Some(MediaType::Svg));
            let opts = TransformOptions {
                format: Some(MediaType::Svg),
                ..TransformOptions::default()
            };
            let _ = transform(TransformRequest::new(input, opts)).unwrap();
        });
    });

    group.bench_function("rasterize_to_png_1024w", |b| {
        b.iter(|| {
            let input = make_artifact(svg_bytes.clone(), Some(MediaType::Svg));
            let opts = TransformOptions {
                width: Some(1024),
                format: Some(MediaType::Png),
                ..TransformOptions::default()
            };
            let _ = transform(TransformRequest::new(input, opts)).unwrap();
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_format_conversion,
    bench_resize,
    bench_fit_modes,
    bench_filters,
    bench_watermark,
    bench_sniff,
    bench_png_variants,
    bench_svg,
);
criterion_main!(benches);
