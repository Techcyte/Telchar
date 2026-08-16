//! Dispatches Telchar CLI commands to the binary runtime.

mod operator;
mod runtime;
mod telemetry;

fn main() -> std::process::ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("serve-stdio") => runtime::serve_stdio(),
        Some("daemon") => runtime::daemon(),
        Some("executor") => runtime::executor(),
        Some("operator") => operator::run(),
        _ => runtime::smoke(),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("telchar: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
