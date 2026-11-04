//! Command-line entry point for headless Prism workflows.

use std::process::ExitCode;

fn main() -> ExitCode {
    match prism_cli::run(std::env::args_os().skip(1)) {
        Ok(prism_cli::CliOutcome::Exported(report)) => {
            println!(
                "Exported {} bytes to {} (duration {}/{})",
                report.byte_length,
                report.output.display(),
                report.duration.value(),
                report.duration.timescale()
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::Inspected(json)) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ProjectCreated { path, project_id }) => {
            println!("Created project {project_id} at {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Prism: {error}");
            ExitCode::FAILURE
        }
    }
}
