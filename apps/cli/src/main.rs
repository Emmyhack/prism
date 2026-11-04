//! Command-line entry point for headless Prism workflows.

use std::process::ExitCode;

fn main() -> ExitCode {
    match prism_cli::run(std::env::args_os().skip(1)) {
        Ok(report) => {
            println!(
                "Exported {} bytes to {} (duration {}/{})",
                report.byte_length,
                report.output.display(),
                report.duration.value(),
                report.duration.timescale()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Prism: {error}");
            ExitCode::FAILURE
        }
    }
}
