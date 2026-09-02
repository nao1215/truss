fn main() -> std::process::ExitCode {
    truss::run_cli(std::env::args_os())
}
