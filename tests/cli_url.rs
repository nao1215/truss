mod common;

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use truss::{MediaType, RawArtifact, sniff_artifact};

fn temp_file_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    std::env::temp_dir().join(format!("truss-integration-{name}-{unique}.bin"))
}

fn spawn_http_server(
    body: Vec<u8>,
    content_type: &'static str,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("server addr");
    let url = format!("http://{addr}/image");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).expect("write headers");
        stream.write_all(&body).expect("write body");
        stream.flush().expect("flush response");
    });

    (url, handle)
}

#[test]
fn inspect_url_reads_remote_png() {
    let (url, handle) = spawn_http_server(common::png_bytes(), "image/png");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("inspect")
        .arg("--url")
        .arg(url)
        .output()
        .expect("run truss inspect");

    handle.join().expect("join server thread");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("\"format\": \"png\""));
    assert!(stdout.contains("\"width\": 4"));
    assert!(stdout.contains("\"height\": 3"));
}

#[test]
fn convert_url_writes_a_local_output_file() {
    let (url, handle) = spawn_http_server(common::png_bytes(), "image/png");
    let output_path = temp_file_path("convert-url-output").with_extension("jpg");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("--url")
        .arg(url)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run truss convert");

    handle.join().expect("join server thread");

    assert!(output.status.success(), "{output:?}");

    let bytes = fs::read(&output_path).expect("read converted output");
    let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff converted output");
    let _ = fs::remove_file(&output_path);

    assert_eq!(artifact.media_type, MediaType::Jpeg);
}

#[test]
fn convert_url_can_infer_avif_output_from_the_file_extension() {
    let (url, handle) = spawn_http_server(common::png_bytes(), "image/png");
    let output_path = temp_file_path("convert-url-output-avif").with_extension("avif");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("--url")
        .arg(url)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run truss convert");

    handle.join().expect("join server thread");

    assert!(output.status.success(), "{output:?}");

    let bytes = fs::read(&output_path).expect("read converted output");
    let artifact = sniff_artifact(RawArtifact::new(bytes, None)).expect("sniff converted output");
    let _ = fs::remove_file(&output_path);

    assert_eq!(artifact.media_type, MediaType::Avif);
}

/// Answers every request with a 302 to `location`, and reports how many requests it saw.
///
/// The redirect target is not this server, so a second request means the client followed
/// the redirect rather than refusing it.
fn spawn_redirecting_server(
    location: &'static str,
) -> (String, Arc<AtomicUsize>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().expect("server addr");
    let url = format!("http://{addr}/image");
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&hits);

    let handle = thread::spawn(move || {
        listener
            .set_nonblocking(false)
            .expect("blocking test listener");
        while let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            seen.fetch_add(1, Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (url, hits, handle)
}

/// A remote server chooses where a redirect goes, so every hop has to be checked.
///
/// The CLI checked only the URL the caller typed and then let the agent follow the chain,
/// so a redirect to `169.254.169.254` was connected to rather than refused. The HTTP server
/// refuses that target on every hop, including with `TRUSS_ALLOW_INSECURE_URL_SOURCES` set,
/// because the metadata check is the one rule the flag does not relax.
#[test]
fn convert_url_refuses_a_redirect_to_a_cloud_metadata_endpoint() {
    let (url, _hits, _handle) =
        spawn_redirecting_server("http://169.254.169.254/latest/meta-data/");
    let output_path = temp_file_path("convert-url-metadata").with_extension("png");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("convert")
        .arg("--url")
        .arg(&url)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run truss convert");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("cloud metadata"),
        "the refusal must name the reason rather than report a transport failure: {stderr}"
    );
    assert!(!output_path.exists(), "no output may be written");
}

/// `inspect --url` reads the same fetch, so it needs the same assertion.
#[test]
fn inspect_url_refuses_a_redirect_to_a_cloud_metadata_endpoint() {
    let (url, _hits, _handle) =
        spawn_redirecting_server("http://169.254.169.254/latest/meta-data/");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("inspect")
        .arg("--url")
        .arg(&url)
        .output()
        .expect("run truss inspect");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("cloud metadata"),
        "the refusal must name the reason rather than report a transport failure: {stderr}"
    );
}

/// A body truss cannot identify is not read back to whoever asked for the URL.
///
/// The failure used to carry the first sixteen bytes of the input in hexadecimal, so
/// pointing truss at an endpoint returned its leading bytes and its exact length.
#[test]
fn an_unrecognized_remote_body_is_not_echoed_back() {
    let secret = b"AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_vec();
    let (url, handle) = spawn_http_server(secret.clone(), "text/plain");
    let output_path = temp_file_path("convert-url-secret").with_extension("png");
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("convert")
        .arg("--url")
        .arg(&url)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run truss convert");

    handle.join().expect("join server thread");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    let leading_hex = secret[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        !stderr.contains(&leading_hex),
        "the body's leading bytes must not be reported back: {stderr}"
    );
    assert!(
        stderr.contains(&format!("{} bytes", secret.len())),
        "the length is still worth reporting: {stderr}"
    );
}
