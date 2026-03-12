mod common;

use common::{png_bytes, send_upload_request, spawn_server, split_response, temp_dir, upload_body};
use truss::{MediaType, RawArtifact, ServerConfig, sniff_artifact};

#[test]
fn serve_once_transforms_an_uploaded_file_over_http() {
    let storage_root = temp_dir("upload-success");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let (boundary, body) = upload_body(&png_bytes(), Some(r#"{"format":"jpeg"}"#));
    let response = send_upload_request(addr, &body, &boundary, Some("secret"));

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
fn serve_once_rejects_uploads_without_a_file_field() {
    let storage_root = temp_dir("upload-missing-file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"jpeg\"}}\r\n--{boundary}--\r\n"
    )
    .into_bytes();
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("requires a `file` field"));
}

#[test]
fn serve_once_rejects_upload_with_empty_file_field() {
    let storage_root = temp_dir("upload-empty-file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n\r\n--{boundary}--\r\n"
    )
    .into_bytes();
    let response = send_upload_request(addr, &body, boundary, Some("secret"));

    handle
        .join()
        .expect("join server thread")
        .expect("serve one request");

    let (header, content_type, body) = split_response(&response);
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("empty"));
}

#[test]
fn serve_once_rejects_upload_with_duplicate_file_field() {
    let storage_root = temp_dir("upload-dup-file");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image2.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
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
fn serve_once_rejects_upload_with_duplicate_options_field() {
    let storage_root = temp_dir("upload-dup-options");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"jpeg\"}}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{\"format\":\"png\"}}\r\n"
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
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("multiple"));
}

#[test]
fn serve_once_rejects_upload_with_invalid_json_in_options() {
    let storage_root = temp_dir("upload-bad-json");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: application/json\r\n\r\n{{invalid json\r\n"
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
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("JSON"));
}

#[test]
fn serve_once_rejects_upload_with_wrong_content_type_on_options() {
    let storage_root = temp_dir("upload-wrong-ct");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"options\"\r\nContent-Type: text/plain\r\n\r\n{{\"format\":\"jpeg\"}}\r\n"
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
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.contains("application/json"));
}

#[test]
fn serve_once_rejects_upload_with_unknown_field_name() {
    let storage_root = temp_dir("upload-unknown-field");
    let (addr, handle) = spawn_server(ServerConfig::new(storage_root, Some("secret".to_string())));
    let boundary = "truss-integration-boundary";
    let png = png_bytes();
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&png);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"extra\"\r\nContent-Type: text/plain\r\n\r\nsome data\r\n"
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
    let body = String::from_utf8(body).expect("utf8 response body");

    assert!(header.starts_with("HTTP/1.1 400 Bad Request"));
    assert_eq!(content_type, "application/problem+json");
    assert!(body.to_lowercase().contains("unsupported field"));
}
