use std::process::ExitCode;

fn main() -> ExitCode {
    match orc::cli::run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
