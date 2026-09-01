use crate::adapters::server::{self, ServerConfig};
use std::io::{self, Write};
use std::net::TcpListener;

use crate::core::error_class::ErrorClass;

use super::{
    ClapServeArgs, ClapValidateArgs, CliError, Command, EXIT_RUNTIME, EXIT_USAGE, HelpTopic,
    ServeCommand, runtime_error, serve_usage, usage_error,
};

// ---------------------------------------------------------------------------
// Clap -> Command conversion
// ---------------------------------------------------------------------------

pub(super) fn serve_from_clap(args: ClapServeArgs) -> Result<Command, CliError> {
    if args.help {
        return Ok(Command::Help(HelpTopic::Serve));
    }

    Ok(Command::Serve(ServeCommand {
        bind_addr: args.bind,
        storage_root: args.storage_root,
        public_base_url: args.public_base_url,
        signed_url_key_id: args.signed_url_key_id,
        signed_url_secret: args.signed_url_secret,
        allow_insecure_url_sources: args.allow_insecure_url_sources,
    }))
}

pub(super) fn validate_from_clap(args: ClapValidateArgs) -> Result<Command, CliError> {
    if args.help {
        return Ok(Command::Help(HelpTopic::Validate));
    }
    Ok(Command::Validate)
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

pub(super) fn execute_serve(command: ServeCommand) -> Result<(), CliError> {
    let bind_addr = command.bind_addr.clone().unwrap_or_else(server::bind_addr);
    let config = resolve_server_config(command)?;
    let listener = TcpListener::bind(&bind_addr).map_err(|error| {
        runtime_error(
            EXIT_RUNTIME,
            &format!("failed to bind {bind_addr}: {error}"),
        )
    })?;
    let listen_addr = listener.local_addr().map_err(|error| {
        runtime_error(
            EXIT_RUNTIME,
            &format!("failed to read listener address: {error}"),
        )
    })?;
    let mut stdout = io::stdout().lock();

    // Server startup summary
    writeln!(stdout, "truss listening on http://{listen_addr}").map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
    })?;
    writeln!(stdout, "  storage root: {}", config.storage_root.display()).map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
    })?;

    // Signed URL verification status
    let signed_url_enabled = !config.signing_keys.is_empty()
        || (config.signed_url_key_id.is_some() && config.signed_url_secret.is_some());
    writeln!(
        stdout,
        "  signed URL verification: {}",
        if signed_url_enabled {
            "enabled"
        } else {
            "disabled"
        }
    )
    .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")))?;

    // Bearer token status (never show the value)
    writeln!(
        stdout,
        "  private API bearer token: {}",
        if config.bearer_token.is_some() {
            "configured"
        } else {
            "not set"
        }
    )
    .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")))?;

    // Cache status
    writeln!(
        stdout,
        "  cache: {}",
        if config.cache_root.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    )
    .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")))?;

    if let Some(ref public_base_url) = config.public_base_url {
        writeln!(stdout, "  public base URL: {public_base_url}").map_err(|error| {
            runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
        })?;
    }
    if config.allow_insecure_url_sources {
        writeln!(stdout, "  insecure URL sources: enabled").map_err(|error| {
            runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
        })?;
    }
    stdout.flush().map_err(|error| {
        runtime_error(EXIT_RUNTIME, &format!("failed to flush stdout: {error}"))
    })?;

    server::serve_with_config(listener, config)
        .map_err(|error| runtime_error(EXIT_RUNTIME, &format!("server runtime failed: {error}")))
}

pub(super) fn execute_validate<W: Write>(stdout: &mut W) -> Result<(), CliError> {
    match ServerConfig::from_env() {
        Ok(config) => {
            ensure_storage_is_usable(&config)?;
            writeln!(stdout, "configuration is valid").map_err(|error| {
                runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
            })?;
            writeln!(stdout, "  storage root: {}", config.storage_root.display()).map_err(
                |error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")),
            )?;
            Ok(())
        }
        Err(error) => Err(usage_error(&format!("invalid configuration: {error}"))),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Refuses a configuration whose storage the server could not serve from.
///
/// [`crate::adapters::server::serve`] runs this before it accepts a connection, and the two
/// CLI commands did not, so a storage root that resolved to a file bound a port and
/// answered 500 to every transform while `truss validate` called the same configuration
/// valid. A storage root that cannot be used is a fault in the configuration, so it is
/// exit 1 here, the code `resolve_server_config` gives every other one.
///
/// For a cloud backend this reaches the endpoint, which is what makes it worth doing before
/// the port is bound rather than after the first request.
fn ensure_storage_is_usable(config: &ServerConfig) -> Result<(), CliError> {
    for (ok, name) in server::storage_health_check(config) {
        if ok {
            continue;
        }
        let detail = if name == "storageRoot" {
            format!(
                "storage root `{}` is not a directory",
                config.storage_root.display()
            )
        } else {
            "the storage backend is not reachable — verify the endpoint, credentials, and \
             the container or bucket"
                .to_string()
        };
        return Err(usage_error(&format!("invalid configuration: {detail}")));
    }
    Ok(())
}

pub(super) fn resolve_server_config(command: ServeCommand) -> Result<ServerConfig, CliError> {
    // A configuration fault exits 1, the same code `truss validate` reports for the same
    // fault. Exit 5 is for what happens after the configuration is accepted — a port
    // already in use, a stream that cannot be written.
    let mut config = ServerConfig::from_env()
        .map_err(|error| usage_error(&format!("failed to load server configuration: {error}")))?;

    if let Some(storage_root) = command.storage_root {
        config.storage_root = storage_root.canonicalize().map_err(|error| {
            usage_error(&format!(
                "failed to resolve storage root {}: {error}",
                storage_root.display()
            ))
        })?;
    }

    if let Some(public_base_url) = command.public_base_url {
        config.public_base_url = Some(public_base_url);
    }

    match (command.signed_url_key_id, command.signed_url_secret) {
        (Some(key_id), Some(secret)) => {
            config = config.with_signed_url_credentials(key_id, secret);
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(CliError {
                exit_code: EXIT_USAGE,
                class: ErrorClass::InvalidRequest,
                message: "--signed-url-key-id and --signed-url-secret must be provided together"
                    .to_string(),
                usage: Some(serve_usage().to_string()),
                hint: Some("run 'truss serve --help' for serve options".to_string()),
            });
        }
        (None, None) => {}
    }

    if command.allow_insecure_url_sources {
        config.allow_insecure_url_sources = true;
    }

    ensure_storage_is_usable(&config)?;

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::io;

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(name: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(name);
            // SAFETY: test-only env mutation guarded by serial execution.
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => {
                    // SAFETY: restoring the pre-test env var value during test teardown.
                    unsafe { std::env::set_var(self.name, previous) };
                }
                None => {
                    // SAFETY: removing the test-only env var during test teardown.
                    unsafe { std::env::remove_var(self.name) };
                }
            }
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("writer failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    #[serial]
    fn execute_serve_returns_runtime_error_for_invalid_bind_addr() {
        let storage_root = tempfile::tempdir().expect("create tempdir");
        let _storage_root_guard = EnvVarGuard::set_path("TRUSS_STORAGE_ROOT", storage_root.path());

        let error = execute_serve(ServeCommand {
            bind_addr: Some("invalid-bind-addr".to_string()),
            storage_root: Some(storage_root.path().to_path_buf()),
            public_base_url: None,
            signed_url_key_id: None,
            signed_url_secret: None,
            allow_insecure_url_sources: false,
        })
        .expect_err("invalid bind address should fail");

        assert_eq!(error.exit_code, super::EXIT_RUNTIME);
        assert!(error.message.contains("failed to bind"));
    }

    #[test]
    #[serial]
    fn execute_validate_reports_writer_failures() {
        let storage_root = tempfile::tempdir().expect("create tempdir");
        let _storage_root_guard = EnvVarGuard::set_path("TRUSS_STORAGE_ROOT", storage_root.path());

        let error =
            execute_validate(&mut FailingWriter).expect_err("writer failure should be reported");

        assert_eq!(error.exit_code, super::EXIT_RUNTIME);
        assert!(error.message.contains("failed to write stdout"));
    }

    /// A storage root that is a file resolves, so the configuration parses, and every
    /// request under it fails.
    ///
    /// `serve_with_config` is reached without the check `serve` runs, so the port is bound
    /// and each transform answers 500, while `/health/ready` reports `storageRoot: fail`.
    /// `truss validate` exists to say so before any of that happens.
    #[test]
    #[serial]
    fn a_storage_root_that_is_a_file_is_a_usage_error_from_validate_and_serve() {
        let parent = tempfile::tempdir().expect("create tempdir");
        let file = parent.path().join("root-is-a-file.png");
        std::fs::write(&file, b"not a directory").expect("write the file");
        let _storage_root_guard = EnvVarGuard::set_path("TRUSS_STORAGE_ROOT", &file);

        let validate_error = execute_validate(&mut Vec::new())
            .expect_err("a storage root that is a file is not one");
        assert_eq!(validate_error.exit_code, super::EXIT_USAGE);
        assert!(
            validate_error.message.contains("storage root"),
            "validate should say what it could not use, got: {}",
            validate_error.message
        );

        let serve_error = execute_serve(ServeCommand {
            bind_addr: Some("127.0.0.1:0".to_string()),
            storage_root: Some(file.clone()),
            public_base_url: None,
            signed_url_key_id: None,
            signed_url_secret: None,
            allow_insecure_url_sources: false,
        })
        .expect_err("serve must not bind a port it cannot serve from");
        assert_eq!(serve_error.exit_code, super::EXIT_USAGE);
        assert!(
            serve_error.message.contains("storage root"),
            "serve should say the same thing, got: {}",
            serve_error.message
        );
    }

    #[test]
    #[serial]
    fn a_storage_root_that_does_not_exist_is_a_usage_error_and_names_the_setting() {
        let missing = std::env::temp_dir().join("truss-storage-root-that-does-not-exist");
        let _storage_root_guard = EnvVarGuard::set_path("TRUSS_STORAGE_ROOT", &missing);

        // `truss validate` and `truss serve` disagreed here: the same misconfiguration
        // exited 1 from one and 5 from the other, and neither said which setting it was.
        let validate_error =
            execute_validate(&mut Vec::new()).expect_err("a missing storage root is invalid");
        assert_eq!(validate_error.exit_code, super::EXIT_USAGE);
        assert!(
            validate_error.message.contains("TRUSS_STORAGE_ROOT"),
            "validate should name the setting, got: {}",
            validate_error.message
        );

        let serve_error = resolve_server_config(ServeCommand {
            bind_addr: None,
            storage_root: None,
            public_base_url: None,
            signed_url_key_id: None,
            signed_url_secret: None,
            allow_insecure_url_sources: false,
        })
        .expect_err("a missing storage root is invalid");
        assert_eq!(serve_error.exit_code, super::EXIT_USAGE);
        assert!(
            serve_error.message.contains("TRUSS_STORAGE_ROOT"),
            "serve should name the setting, got: {}",
            serve_error.message
        );
    }
}
