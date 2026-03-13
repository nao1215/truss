mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
fn help_lists_serve_runtime_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("serve")
        .arg("--help")
        .output()
        .expect("run truss serve --help");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("--storage-root"), "{stdout}");
    assert!(stdout.contains("--public-base-url"), "{stdout}");
    assert!(stdout.contains("--signed-url-key-id"), "{stdout}");
    assert!(stdout.contains("--signed-url-secret"), "{stdout}");
    assert!(stdout.contains("--allow-insecure-url-sources"), "{stdout}");
}

#[test]
fn bare_invocation_shows_top_level_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .output()
        .expect("run bare truss");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("COMMANDS:"), "{stdout}");
    assert!(stdout.contains("convert"), "{stdout}");
    assert!(stdout.contains("inspect"), "{stdout}");
    assert!(stdout.contains("serve"), "{stdout}");
    assert!(stdout.contains("sign"), "{stdout}");
}

#[test]
fn top_level_help_shows_exit_codes() {
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("--help")
        .output()
        .expect("run truss --help");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("EXIT CODES:"), "{stdout}");
    assert!(stdout.contains("0  Success"), "{stdout}");
    assert!(stdout.contains("1  Usage error"), "{stdout}");
}

#[test]
fn help_convert_shows_convert_specific_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("help")
        .arg("convert")
        .output()
        .expect("run truss help convert");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("truss convert"), "{stdout}");
    assert!(stdout.contains("--output"), "{stdout}");
    assert!(stdout.contains("--width"), "{stdout}");
    // Should NOT contain serve options
    assert!(!stdout.contains("--bind"), "{stdout}");
}

#[test]
fn convert_dash_help_shows_convert_specific_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("convert")
        .arg("--help")
        .output()
        .expect("run truss convert --help");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("truss convert"), "{stdout}");
}

#[test]
fn top_level_server_flags_start_the_server() {
    let storage_root = common::temp_dir("serve-startup");
    let mut child = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--storage-root")
        .arg(&storage_root)
        .arg("--public-base-url")
        .arg("https://assets.example.com")
        .arg("--signed-url-key-id")
        .arg("public-dev")
        .arg("--signed-url-secret")
        .arg("secret-value")
        .arg("--allow-insecure-url-sources")
        .env("TRUSS_BEARER_TOKEN", "secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn truss serve");

    let stdout = child.stdout.take().expect("take child stdout");
    let mut stdout = BufReader::new(stdout);
    let mut lines = Vec::new();

    // Read startup summary lines: 7 lines for this config
    // (listening, storage root, signed URL, bearer token, cache, public base URL, insecure)
    for _ in 0..7 {
        let mut line = String::new();
        let read = stdout.read_line(&mut line).expect("read startup line");
        assert!(read > 0, "server exited before printing expected output");
        lines.push(line.trim_end().to_string());
    }

    child.kill().expect("kill serve process");
    let output = child.wait_with_output().expect("wait for serve process");
    let _ = fs::remove_dir_all(&storage_root);

    let combined_stdout = lines.join("\n");
    assert!(
        combined_stdout.contains("truss listening on http://127.0.0.1:"),
        "{combined_stdout}"
    );
    assert!(
        !combined_stdout.contains("http://127.0.0.1:0"),
        "{combined_stdout}"
    );
    assert!(
        combined_stdout.contains("storage root:"),
        "{combined_stdout}"
    );
    assert!(
        combined_stdout.contains("signed URL verification: enabled"),
        "{combined_stdout}"
    );
    assert!(
        combined_stdout.contains("private API bearer token: configured"),
        "{combined_stdout}"
    );
    assert!(
        combined_stdout.contains("insecure URL sources: enabled"),
        "{combined_stdout}"
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn serve_startup_summary_includes_state_info() {
    let storage_root = common::temp_dir("serve-summary");
    let mut child = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("serve")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--storage-root")
        .arg(&storage_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn truss serve");

    let stdout_handle = child.stdout.take().expect("take child stdout");
    let mut stdout = BufReader::new(stdout_handle);
    let mut lines = Vec::new();

    // 5 lines: listening, storage root, signed URL disabled, bearer not set, cache disabled
    for _ in 0..5 {
        let mut line = String::new();
        let read = stdout.read_line(&mut line).expect("read startup line");
        assert!(read > 0, "server exited before printing expected output");
        lines.push(line.trim_end().to_string());
    }

    child.kill().expect("kill serve process");
    let _ = child.wait_with_output();
    let _ = fs::remove_dir_all(&storage_root);

    let combined = lines.join("\n");
    // Verify all summary fields are present
    assert!(combined.contains("truss listening on"), "{combined}");
    assert!(combined.contains("storage root:"), "{combined}");
    assert!(
        combined.contains("signed URL verification: disabled"),
        "{combined}"
    );
    assert!(
        combined.contains("private API bearer token: not set"),
        "{combined}"
    );
    assert!(combined.contains("cache: disabled"), "{combined}");
}
