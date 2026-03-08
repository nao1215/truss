use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("truss-cli-{name}-{unique}"));
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

#[test]
fn help_lists_serve_runtime_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("--help")
        .output()
        .expect("run truss --help");

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("--storage-root <PATH>"));
    assert!(stdout.contains("--public-base-url <URL>"));
    assert!(stdout.contains("--allow-insecure-url-sources"));
}

#[test]
fn serve_prints_runtime_configuration_on_startup() {
    let storage_root = temp_dir("serve-startup");
    let mut child = Command::new(env!("CARGO_BIN_EXE_truss"))
        .arg("serve")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--storage-root")
        .arg(&storage_root)
        .arg("--public-base-url")
        .arg("https://assets.example.com")
        .arg("--allow-insecure-url-sources")
        .env("TRUSS_BEARER_TOKEN", "secret")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn truss serve");

    let stdout = child.stdout.take().expect("take child stdout");
    let mut stdout = BufReader::new(stdout);
    let mut lines = Vec::new();

    for _ in 0..4 {
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
        combined_stdout.contains(&format!("storage root: {}", storage_root.display())),
        "{combined_stdout}"
    );
    assert!(
        combined_stdout.contains("public base URL: https://assets.example.com"),
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
