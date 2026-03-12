mod common;

use common::{
    large_png_bytes, png_bytes, send_public_get_request, send_transform_request,
    send_upload_request, signed_target, spawn_fixture_server, spawn_server, split_response,
    temp_dir,
};
use std::collections::BTreeMap;
use std::fs;
use truss::{MediaType, RawArtifact, ServerConfig, sniff_artifact};

#[test]
fn test_json_transform_with_watermark() {
    let storage_root = temp_dir("wm-json");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    // Spawn a fixture server to serve the watermark image.
    let (wm_url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.png"}},"options":{{"format":"png"}},"watermark":{{"url":"{wm_url}","position":"center","opacity":80,"margin":0}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK but got header: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "image/png");
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff watermarked output");
    assert_eq!(artifact.media_type, MediaType::Png);
}

#[test]
fn test_multipart_upload_with_watermark() {
    let storage_root = temp_dir("wm-multipart");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-wm-boundary";
    let png = large_png_bytes();
    let mut body = Vec::new();

    // file part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    // watermark part (small image used as watermark)
    let wm_png = png_bytes();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark\"; filename=\"wm.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&wm_png);
    body.extend_from_slice(b"\r\n");

    // watermark_position part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark_position\"\r\n\r\nbottom-right\r\n"
        )
        .as_bytes(),
    );

    // watermark_opacity part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark_opacity\"\r\n\r\n75\r\n"
        )
        .as_bytes(),
    );

    // watermark_margin part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark_margin\"\r\n\r\n5\r\n"
        )
        .as_bytes(),
    );

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK but got header: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "image/png");
    let artifact = sniff_artifact(RawArtifact::new(body, None)).expect("sniff watermarked upload");
    assert_eq!(artifact.media_type, MediaType::Png);
}

#[test]
fn test_multipart_upload_rejects_duplicate_watermark() {
    let storage_root = temp_dir("wm-dup");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-wm-dup-boundary";
    let png = png_bytes();
    let mut body = Vec::new();

    // file part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    // first watermark part
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark\"; filename=\"wm1.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    // second watermark part (duplicate)
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"watermark\"; filename=\"wm2.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("multiple"));
}

#[test]
fn test_public_get_with_watermark_query_params() {
    let storage_root = temp_dir("wm-public-get");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    // Spawn a fixture server to serve the watermark image.
    let (wm_url, fixture) = spawn_fixture_server(vec![(
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
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("watermarkUrl".to_string(), wm_url),
            ("watermarkPosition".to_string(), "bottom-right".to_string()),
            ("watermarkOpacity".to_string(), "60".to_string()),
            ("watermarkMargin".to_string(), "5".to_string()),
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
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 200 OK"),
        "expected 200 OK but got header: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "image/png");
    let artifact =
        sniff_artifact(RawArtifact::new(body, None)).expect("sniff watermarked public output");
    assert_eq!(artifact.media_type, MediaType::Png);
}

#[test]
fn test_watermark_opacity_zero_rejected() {
    let storage_root = temp_dir("wm-opacity-zero");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    // Validation rejects opacity before any watermark fetch, so no fixture server is needed.
    let body = r#"{"source":{"kind":"path","path":"/image.png"},"watermark":{"url":"http://127.0.0.1:1/wm.png","opacity":0}}"#;
    let response = send_transform_request(addr, body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("opacity"));
}

#[test]
fn test_watermark_opacity_over_100_rejected() {
    let storage_root = temp_dir("wm-opacity-over");
    fs::write(storage_root.join("image.png"), png_bytes()).expect("write source fixture");

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    // Validation rejects opacity before any watermark fetch, so no fixture server is needed.
    let body = r#"{"source":{"kind":"path","path":"/image.png"},"watermark":{"url":"http://127.0.0.1:1/wm.png","opacity":101}}"#;
    let response = send_transform_request(addr, body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("opacity"));
}

#[test]
fn test_orphaned_watermark_params_without_url() {
    let storage_root = temp_dir("wm-orphan-params");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string()))
            .with_signed_url_credentials("public-dev", "secret-value")
            .with_insecure_url_sources(true),
    );

    // watermarkPosition/Opacity/Margin WITHOUT watermarkUrl should be rejected.
    let target = signed_target(
        "/images/by-path",
        BTreeMap::from([
            ("path".to_string(), "/image.png".to_string()),
            ("keyId".to_string(), "public-dev".to_string()),
            ("expires".to_string(), "4102444800".to_string()),
            ("watermarkPosition".to_string(), "center".to_string()),
            ("watermarkOpacity".to_string(), "80".to_string()),
            ("watermarkMargin".to_string(), "5".to_string()),
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
    let body = String::from_utf8(body).expect("utf8 response body");
    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("watermarkUrl"));
}

#[test]
fn test_empty_watermark_url_rejected() {
    let storage_root = temp_dir("wm-empty-url");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );

    // Empty watermark URL should be rejected when watermark object is present.
    let body = r#"{"source":{"kind":"path","path":"/image.png"},"watermark":{"url":""}}"#;
    let response = send_transform_request(addr, body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");
    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("url"));
}

#[cfg(feature = "svg")]
#[test]
fn test_svg_source_with_watermark_rejected() {
    let storage_root = temp_dir("wm-svg-reject");
    let svg_content = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"100\" height=\"100\"><rect fill=\"red\" width=\"100\" height=\"100\"/></svg>";
    fs::write(storage_root.join("image.svg"), svg_content).expect("write svg fixture");

    let (wm_url, fixture) = spawn_fixture_server(vec![(
        "200 OK".to_string(),
        vec![("Content-Type".to_string(), "image/png".to_string())],
        png_bytes(),
    )]);

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.svg"}},"watermark":{{"url":"{wm_url}"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(
        header.starts_with("HTTP/1.1 400 Bad Request"),
        "expected 400 but got: {header}\nbody: {body}"
    );
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("svg"));
}

#[test]
fn test_watermark_url_redirect_followed() {
    let storage_root = temp_dir("wm-redirect");
    fs::write(storage_root.join("image.png"), large_png_bytes()).expect("write source fixture");

    let (wm_url, fixture) = spawn_fixture_server(vec![
        (
            "302 Found".to_string(),
            vec![
                ("Content-Type".to_string(), "text/plain".to_string()),
                ("Location".to_string(), "/redirected".to_string()),
            ],
            Vec::new(),
        ),
        (
            "200 OK".to_string(),
            vec![("Content-Type".to_string(), "image/png".to_string())],
            png_bytes(),
        ),
    ]);

    let (addr, handle) = spawn_server(
        ServerConfig::new(storage_root, Some("secret".to_string())).with_insecure_url_sources(true),
    );
    let body = format!(
        r#"{{"source":{{"kind":"path","path":"/image.png"}},"watermark":{{"url":"{wm_url}"}}}}"#
    );
    let response = send_transform_request(addr, &body, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");
    fixture.join().expect("join fixture server");

    let (header, content_type, body) = split_response(&response);
    let body_str = String::from_utf8_lossy(&body);
    assert!(
        header.starts_with("HTTP/1.1 200"),
        "expected HTTP/1.1 200 but got: {header}\nbody: {body_str}"
    );
    assert_eq!(content_type, "image/png");
    assert!(
        body.starts_with(&[0x89, b'P', b'N', b'G']),
        "expected PNG magic bytes in response body"
    );
}
