mod common;

use common::{
    png_bytes, send_public_get_request, send_public_get_request_with_headers, send_signed_get,
    send_transform_request, signed_target, spawn_fixture_server, spawn_server, split_response,
    status_code, temp_dir,
};
use std::collections::BTreeMap;
use std::fs;
use truss::{
    CropRegion, Fit, MediaType, OptimizeMode, Position, QualityMetric, RawArtifact, Rgba8,
    Rotation, ServerConfig, TargetQuality, TransformOptions, TransformRequest, sniff_artifact,
    transform,
};

#[test]
fn serve_once_transforms_a_signed_public_path_request() {
    let storage_root = temp_dir("public-path");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn serve_once_transforms_a_signed_public_url_request() {
    let storage_root = temp_dir("public-url");
    let (url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value")
            .with_insecure_url_sources(true),
    );
    let target = signed_target(
        "/images/by-url",
        BTreeMap::from([
            ("url".to_string(), url),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff transformed output");

    assert!(header.starts_with("HTTP/1.1 200 OK"));
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn serve_once_rejects_requests_without_a_bearer_token() {
    let storage_root = temp_dir("unauthorized");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let response = send_transform_request(
        addr,
        r#"{"source":{"kind":"path","path":"/image.png"}}"#,
        None,
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 401 Unauthorized"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("authorization required"));
}

// ---------------------------------------------------------------------------
// Signed public GET failure cases
// ---------------------------------------------------------------------------

#[test]
fn serve_once_rejects_expired_signed_public_request() {
    let storage_root = temp_dir("expired-sig");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "1".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 401 Unauthorized"));
    assert!(body.to_lowercase().contains("expired"));
}

#[test]
fn serve_once_rejects_signed_public_request_with_wrong_secret() {
    let storage_root = temp_dir("wrong-sig");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "wrong-secret",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);

    assert!(header.starts_with("HTTP/1.1 401 Unauthorized"));
}

#[test]
fn serve_once_rejects_signed_public_request_with_accept_json() {
    let storage_root = temp_dir("accept-json");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request_with_headers(
        addr,
        &target,
        "cdn.example.com",
        &[("Accept", "application/json")],
    );

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);

    assert!(header.starts_with("HTTP/1.1 406 Not Acceptable"));
}

#[test]
fn serve_once_rejects_signed_public_request_with_unknown_query_parameter() {
    let storage_root = temp_dir("unknown-param");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
            ("unknown".to_string(), "value".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );
    let response = send_public_get_request(addr, &target, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert!(body.to_lowercase().contains("is not supported"));
}

/// The signature encoding is part of the contract: `docs/signed-url-spec.md` fixes it at
/// lowercase hex. `hex::decode` accepts either case, so one signed URL used to verify under
/// 2^64 distinct URL strings, each of which misses a CDN cache and reaches the origin.
#[test]
fn serve_once_rejects_a_signed_public_request_with_an_uppercase_signature() {
    let storage_root = temp_dir("uppercase-signature");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");
    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value"),
    );
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("format".to_string(), "jpeg".to_string()),
        ]),
        "cdn.example.com",
        "secret-value",
    );

    let (prefix, rest) = target
        .split_once("signature=")
        .expect("the signed target carries a signature");
    let (signature, suffix) = rest.split_once('&').unwrap_or((rest, ""));
    let uppercased = format!(
        "{prefix}signature={}{}{suffix}",
        signature.to_ascii_uppercase(),
        if suffix.is_empty() { "" } else { "&" }
    );
    let response = send_public_get_request(addr, &uppercased, "cdn.example.com");

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, _content_type, _body) = split_response(&response);

    assert!(
        header.starts_with("HTTP/1.1 401 Unauthorized"),
        "unexpected response: {header}"
    );
}

/// A signed URL delivers what the library delivers for the same options.
///
/// The signer, the query parser, and the library each hold their own spelling of the
/// transform vocabulary, and nothing but this walks all three with the same request. The
/// table covers every field of `TransformOptions`, including the two that used to disagree:
/// a negative `rotate`, which the server refused and the CLI and the browser accepted, and
/// `format=gif`, which the server used to refuse only after fetching and decoding the source.
#[test]
fn a_signed_url_delivers_what_the_library_delivers() {
    let storage_root = temp_dir("signed-parity");
    fs::write(storage_root.join("src.png"), parity_source()).expect("write source fixture");

    let mut mismatches: Vec<String> = Vec::new();
    for (name, options) in parity_cases() {
        let (addr, _handle) = spawn_server(
            ServerConfig::new(storage_root.clone(), None)
                .with_signed_url_credentials("k", "secret-value"),
        );
        let authority = addr.to_string();
        let expected = {
            let artifact =
                sniff_artifact(RawArtifact::new(parity_source(), None)).expect("sniff source");
            transform(TransformRequest::new(artifact, options.clone()))
        };
        let target = signed_target(
            "/images/by-path",
            parity_query(&options),
            &authority,
            "secret-value",
        );
        let (header, body) = send_signed_get(addr, &target, &authority);
        let status = status_code(&header);

        match expected {
            Ok(result) => {
                if status != 200 {
                    mismatches.push(format!(
                        "{name}: the library produced an image, the server answered {status}: {}",
                        String::from_utf8_lossy(&body)
                    ));
                } else if result.artifact.bytes != body {
                    mismatches.push(format!(
                        "{name}: bytes differ (library {}, server {})",
                        result.artifact.bytes.len(),
                        body.len()
                    ));
                }
            }
            Err(error) => {
                if status == 200 {
                    mismatches.push(format!(
                        "{name}: the library refused it ({error}), the server answered 200"
                    ));
                }
            }
        }
    }

    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

/// A signed URL may say `rotate=-90`, which is what the CLI flag and the browser option
/// take, and it means the same three-quarter turn the library gives.
///
/// The wire value is what matters here: `Rotation` normalizes `-90` to `270` before it is
/// printed, so the parity table above can never put a minus sign on the query string. The
/// server used to read this field as an unsigned integer and answered 400 before any truss
/// code ran, with a message that named the Rust type.
#[test]
fn a_signed_url_may_turn_counter_clockwise() {
    let storage_root = temp_dir("signed-rotate-negative");
    fs::write(storage_root.join("src.png"), parity_source()).expect("write source fixture");
    let (addr, _handle) = spawn_server(
        ServerConfig::new(storage_root, None).with_signed_url_credentials("k", "secret-value"),
    );
    let authority = addr.to_string();

    let mut query = BTreeMap::new();
    query.insert("path".to_string(), "src.png".to_string());
    query.insert("keyId".to_string(), "k".to_string());
    query.insert("expires".to_string(), "4102444800".to_string());
    query.insert("rotate".to_string(), "-90".to_string());
    query.insert("format".to_string(), "png".to_string());

    let target = signed_target("/images/by-path", query, &authority, "secret-value");
    let (header, body) = send_signed_get(addr, &target, &authority);
    assert_eq!(status_code(&header), 200, "{header}");

    let artifact = sniff_artifact(RawArtifact::new(parity_source(), None)).expect("sniff source");
    let mut options = TransformOptions::default();
    options.format = Some(MediaType::Png);
    options.rotate = Rotation::from_degrees(-90);
    let expected = transform(TransformRequest::new(artifact, options))
        .expect("turn the picture counter-clockwise");

    assert_eq!(expected.artifact.bytes, body);
}

/// A `format` truss reads but cannot write is refused from the options, before the source is
/// read, and with the class the pipeline gives the same refusal.
#[test]
fn a_signed_url_asking_for_a_decode_only_format_is_refused() {
    let storage_root = temp_dir("signed-gif-output");
    // No source file: the refusal has to come before the server looks for one, so a 404
    // here would mean the format was still being checked after the fetch.
    let (addr, _handle) = spawn_server(
        ServerConfig::new(storage_root, None).with_signed_url_credentials("k", "secret-value"),
    );
    let authority = addr.to_string();

    let mut query = BTreeMap::new();
    query.insert("path".to_string(), "src.png".to_string());
    query.insert("keyId".to_string(), "k".to_string());
    query.insert("expires".to_string(), "4102444800".to_string());
    query.insert("format".to_string(), "gif".to_string());

    let target = signed_target("/images/by-path", query, &authority, "secret-value");
    let (header, body) = send_signed_get(addr, &target, &authority);

    assert_eq!(status_code(&header), 415, "{header}");
    let body = String::from_utf8(body).expect("utf-8 problem body");
    assert!(body.contains("unsupported-output-media-type"), "{body}");
    assert!(body.contains("input-only"), "{body}");
}

fn parity_source() -> Vec<u8> {
    use image::codecs::png::PngEncoder;
    use image::{ColorType, ImageEncoder, Rgba, RgbaImage};
    let image = RgbaImage::from_fn(64, 48, |x, y| {
        Rgba([
            u8::try_from(x * 4).unwrap_or(255),
            u8::try_from(y * 5).unwrap_or(255),
            128,
            u8::try_from(200 + (x % 55)).unwrap_or(255),
        ])
    });
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&image, 64, 48, ColorType::Rgba8.into())
        .expect("encode source png");
    bytes
}

fn parity_base() -> TransformOptions {
    // Every field is named on purpose, so a field truss adds later fails to be named here
    // and the parity table is extended deliberately rather than by omission.
    let mut options = TransformOptions::default();
    options.width = Some(40);
    options.height = Some(30);
    options.fit = Some(Fit::Cover);
    options.position = Some(Position::Top);
    options.format = Some(MediaType::Jpeg);
    options.quality = Some(70);
    options.optimize = OptimizeMode::None;
    options.target_quality = None;
    options.background = Some(Rgba8 {
        r: 1,
        g: 2,
        b: 3,
        a: 255,
    });
    options.rotate = Rotation::DEG_90;
    options.auto_orient = true;
    options.strip_metadata = true;
    options.preserve_exif = false;
    options.crop = Some(CropRegion {
        x: 1,
        y: 2,
        width: 30,
        height: 40,
    });
    options.blur = Some(1.0);
    options.sharpen = Some(2.0);
    options.grayscale = false;
    options.without_enlargement = false;
    options.deadline = None;
    options
}

fn parity_cases() -> Vec<(&'static str, TransformOptions)> {
    let m = |f: fn(&mut TransformOptions)| {
        let mut options = parity_base();
        f(&mut options);
        options
    };
    vec![
        ("base", parity_base()),
        ("width", m(|o| o.width = Some(41))),
        ("height", m(|o| o.height = Some(31))),
        ("fit-contain", m(|o| o.fit = Some(Fit::Contain))),
        ("fit-fill", m(|o| o.fit = Some(Fit::Fill))),
        ("fit-inside", m(|o| o.fit = Some(Fit::Inside))),
        ("position", m(|o| o.position = Some(Position::Bottom))),
        (
            "format-png",
            m(|o| {
                o.format = Some(MediaType::Png);
                o.quality = None;
            }),
        ),
        ("format-webp", m(|o| o.format = Some(MediaType::Webp))),
        ("quality", m(|o| o.quality = Some(30))),
        (
            "optimize-lossless",
            m(|o| {
                o.format = Some(MediaType::Png);
                o.quality = None;
                o.optimize = OptimizeMode::Lossless;
            }),
        ),
        (
            "optimize-auto",
            m(|o| {
                o.quality = None;
                o.optimize = OptimizeMode::Auto;
            }),
        ),
        (
            "targetQuality",
            m(|o| {
                o.quality = None;
                o.optimize = OptimizeMode::Lossy;
                o.target_quality = Some(TargetQuality {
                    metric: QualityMetric::Psnr,
                    value: 30.0,
                });
            }),
        ),
        (
            "background",
            m(|o| {
                o.background = Some(Rgba8 {
                    r: 200,
                    g: 30,
                    b: 40,
                    a: 255,
                });
            }),
        ),
        (
            "background-alpha",
            m(|o| {
                o.background = Some(Rgba8 {
                    r: 200,
                    g: 30,
                    b: 40,
                    a: 128,
                });
            }),
        ),
        ("rotate-180", m(|o| o.rotate = Rotation::DEG_180)),
        ("rotate-45", m(|o| o.rotate = Rotation::from_degrees(45))),
        (
            "rotate-negative",
            m(|o| o.rotate = Rotation::from_degrees(-90)),
        ),
        ("autoOrient-off", m(|o| o.auto_orient = false)),
        ("keepMetadata", m(|o| o.strip_metadata = false)),
        (
            "preserveExif",
            m(|o| {
                o.strip_metadata = false;
                o.preserve_exif = true;
            }),
        ),
        (
            "crop",
            m(|o| {
                o.crop = Some(CropRegion {
                    x: 3,
                    y: 4,
                    width: 20,
                    height: 20,
                });
            }),
        ),
        ("crop-none", m(|o| o.crop = None)),
        ("blur", m(|o| o.blur = Some(3.5))),
        ("blur-none", m(|o| o.blur = None)),
        ("sharpen", m(|o| o.sharpen = Some(5.5))),
        ("sharpen-none", m(|o| o.sharpen = None)),
        ("grayscale", m(|o| o.grayscale = true)),
        (
            "withoutEnlargement",
            m(|o| {
                o.width = Some(400);
                o.height = Some(300);
                o.without_enlargement = true;
            }),
        ),
        (
            "no-geometry",
            m(|o| {
                o.width = None;
                o.height = None;
                o.fit = None;
                o.position = None;
            }),
        ),
    ]
}

/// Writes the options the way a caller writes them into a signed URL's query.
fn parity_query(options: &TransformOptions) -> BTreeMap<String, String> {
    let mut query = BTreeMap::new();
    query.insert("path".to_string(), "src.png".to_string());
    query.insert("keyId".to_string(), "k".to_string());
    query.insert("expires".to_string(), "4102444800".to_string());
    if let Some(value) = options.width {
        query.insert("width".to_string(), value.to_string());
    }
    if let Some(value) = options.height {
        query.insert("height".to_string(), value.to_string());
    }
    if let Some(value) = options.fit {
        query.insert("fit".to_string(), value.as_name().to_string());
    }
    if let Some(value) = options.position {
        query.insert("position".to_string(), value.as_name().to_string());
    }
    if let Some(value) = options.format {
        query.insert("format".to_string(), value.as_name().to_string());
    }
    if let Some(value) = options.quality {
        query.insert("quality".to_string(), value.to_string());
    }
    if options.optimize != OptimizeMode::None {
        query.insert(
            "optimize".to_string(),
            options.optimize.as_name().to_string(),
        );
    }
    if let Some(value) = options.target_quality {
        query.insert("targetQuality".to_string(), value.to_string());
    }
    if let Some(value) = options.background {
        let hex = if value.a == u8::MAX {
            format!("{:02X}{:02X}{:02X}", value.r, value.g, value.b)
        } else {
            format!(
                "{:02X}{:02X}{:02X}{:02X}",
                value.r, value.g, value.b, value.a
            )
        };
        query.insert("background".to_string(), hex);
    }
    if !options.rotate.is_identity() {
        query.insert("rotate".to_string(), options.rotate.to_string());
    }
    if !options.auto_orient {
        query.insert("autoOrient".to_string(), "false".to_string());
    }
    if !options.strip_metadata {
        query.insert("stripMetadata".to_string(), "false".to_string());
    }
    if options.preserve_exif {
        query.insert("preserveExif".to_string(), "true".to_string());
    }
    if let Some(value) = options.crop {
        query.insert("crop".to_string(), value.to_string());
    }
    if let Some(value) = options.blur {
        query.insert("blur".to_string(), format!("{value}"));
    }
    if let Some(value) = options.sharpen {
        query.insert("sharpen".to_string(), format!("{value}"));
    }
    if options.grayscale {
        query.insert("grayscale".to_string(), "true".to_string());
    }
    if options.without_enlargement {
        query.insert("withoutEnlargement".to_string(), "true".to_string());
    }
    query
}
