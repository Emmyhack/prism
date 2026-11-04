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
        Ok(prism_cli::CliOutcome::VideoSequenceCreated {
            path,
            sequence_id,
            track_id,
        }) => {
            println!(
                "Updated {}: sequence {sequence_id}, track {track_id}",
                path.display()
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ClipAdded { path, clip_id }) => {
            println!("Updated {}: clip {clip_id}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ClipMoved {
            path,
            clip_id,
            timeline_start,
        }) => {
            println!(
                "Updated {}: moved clip {clip_id} to {}/{}",
                path.display(),
                timeline_start.value(),
                timeline_start.timescale()
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ClipTrimmed { path, clip_id }) => {
            println!("Updated {}: trimmed clip {clip_id}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ClipSplit {
            path,
            clip_id,
            right_clip_id,
        }) => {
            println!(
                "Updated {}: split clip {clip_id}; right clip {right_clip_id}",
                path.display()
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ClipDeleted { path, clip_id }) => {
            println!("Updated {}: deleted clip {clip_id}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ScriptTransaction {
            path,
            revision,
            dry_run,
            event_count,
        }) => {
            println!(
                "{} {}: revision {revision}, {event_count} event(s)",
                if dry_run { "Validated" } else { "Committed" },
                path.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Prism: {error}");
            ExitCode::FAILURE
        }
    }
}
