/// How much stack the CLI runs on.
///
/// The thread a process starts on has whatever stack the platform gave it, and on Windows that
/// is the one megabyte the linker reserves by default, so how deep a decoder or the SVG renderer
/// may go would depend on the platform rather than on truss. Running on a thread truss creates
/// takes the platform out of the question, and the size is a reservation of address space
/// rather than memory that is committed.
const CLI_STACK_SIZE: usize = 16 * 1024 * 1024;

/// The exit code a panic reports: `internal-error` after the input was read, which is what the
/// table in `docs/problems.md` calls 5.
const PANIC_EXIT_CODE: u8 = 5;

fn main() -> std::process::ExitCode {
    let arguments: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let worker = std::thread::Builder::new()
        .name("truss-cli".into())
        .stack_size(CLI_STACK_SIZE)
        .spawn(move || truss::run_cli(arguments))
        .expect("failed to start the truss thread");

    // A panic inside the CLI has already printed its message through the default hook; what is
    // left to report is the exit code, which is what a caller branches on.
    worker
        .join()
        .unwrap_or(std::process::ExitCode::from(PANIC_EXIT_CODE))
}
