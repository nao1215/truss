use std::fs;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use truss::{
    Artifact, Fit, MediaType, Position, RawArtifact, TransformOptions, TransformRequest,
    sniff_artifact, transform,
};

fn fixture(name: &str) -> Vec<u8> {
    fs::read(format!("integration/fixtures/{name}")).expect("fixture file must exist")
}

fn make_artifact(bytes: Vec<u8>, media_type: Option<MediaType>) -> Artifact {
    sniff_artifact(RawArtifact::new(bytes, media_type)).expect("sniff must succeed")
}

// ---------------------------------------------------------------------------
// Format conversion: JPEG -> various output formats
// ---------------------------------------------------------------------------
fn bench_format_conversion(c: &mut Criterion) {
    let jpeg_bytes = fixture("sample.jpg");
    let targets: &[(&str, MediaType, Option<u8>)] = &[
        ("jpeg_to_png", MediaType::Png, None),
        ("jpeg_to_webp", MediaType::Webp, Some(80)),
        #[cfg(feature = "avif")]
        ("jpeg_to_avif", MediaType::Avif, Some(80)),
    ];

    let mut group = c.benchmark_group("format_conversion");
    for (label, target_format, quality) in targets {
        group.bench_with_input(
            BenchmarkId::new(*label, "640x427"),
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
    let jpeg_bytes = fixture("sample.jpg");
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
    let jpeg_bytes = fixture("sample.jpg");
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
    let jpeg_bytes = fixture("sample.jpg");

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
    let jpeg_bytes = fixture("sample.jpg");
    let watermark_bytes = fixture("sample.png");

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
