use crate::adapters::server::{self, ServerConfig};
use std::io::{self, Write};
use std::net::TcpListener;

use super::{
    ClapServeArgs, ClapValidateArgs, CliError, Command, EXIT_RUNTIME, EXIT_USAGE, HelpTopic,
    ServeCommand, runtime_error, serve_usage,
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
            writeln!(stdout, "configuration is valid").map_err(|error| {
                runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}"))
            })?;
            writeln!(stdout, "  storage root: {}", config.storage_root.display()).map_err(
                |error| runtime_error(EXIT_RUNTIME, &format!("failed to write stdout: {error}")),
            )?;
            Ok(())
        }
        Err(error) => Err(runtime_error(
            EXIT_USAGE,
            &format!("invalid configuration: {error}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn resolve_server_config(command: ServeCommand) -> Result<ServerConfig, CliError> {
    let mut config = ServerConfig::from_env().map_err(|error| {
        runtime_error(
            EXIT_RUNTIME,
            &format!("failed to load server configuration: {error}"),
        )
    })?;

    if let Some(storage_root) = command.storage_root {
        config.storage_root = storage_root.canonicalize().map_err(|error| {
            runtime_error(
                EXIT_RUNTIME,
                &format!(
                    "failed to resolve storage root {}: {error}",
                    storage_root.display()
                ),
            )
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

    Ok(config)
}
