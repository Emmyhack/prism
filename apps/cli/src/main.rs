//! Command-line entry point for headless Prism workflows.

use std::process::ExitCode;

#[allow(clippy::too_many_lines)]
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
        Ok(prism_cli::CliOutcome::ProxyGenerated { path }) => {
            println!("Generated proxy {}", path.display());
            ExitCode::SUCCESS
        }
        Ok(
            prism_cli::CliOutcome::Inspected(json)
            | prism_cli::CliOutcome::AutosavesListed { json },
        ) => {
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
        Ok(prism_cli::CliOutcome::ClipDuplicated {
            path,
            clip_id,
            new_clip_id,
        }) => {
            println!(
                "Updated {}: duplicated clip {clip_id} as {new_clip_id}",
                path.display()
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::ClipRippleDeleted { path, clip_id }) => {
            println!("Updated {}: ripple-deleted clip {clip_id}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::TransitionAdded {
            path,
            transition_id,
        }) => {
            println!(
                "Updated {}: added transition {transition_id}",
                path.display()
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::TransitionDeleted {
            path,
            transition_id,
        }) => {
            println!(
                "Updated {}: deleted transition {transition_id}",
                path.display()
            );
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
        Ok(prism_cli::CliOutcome::BackupCreated { path, revision }) => {
            println!("Created backup {} at revision {revision}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::AutosaveCreated { path, revision }) => {
            println!("Created autosave {} at revision {revision}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::TrackLockChanged {
            path,
            track_id,
            locked,
        }) => {
            println!(
                "Updated {}: track {track_id} {}",
                path.display(),
                if locked { "locked" } else { "unlocked" }
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::TrackVisibilityChanged {
            path,
            track_id,
            visible,
        }) => {
            println!(
                "Updated {}: track {track_id} {}",
                path.display(),
                if visible { "visible" } else { "hidden" }
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::TrackMuteChanged {
            path,
            track_id,
            muted,
        }) => {
            println!(
                "Updated {}: track {track_id} {}",
                path.display(),
                if muted { "muted" } else { "unmuted" }
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::TrackReordered {
            path,
            track_id,
            destination_index,
        }) => {
            println!(
                "Updated {}: track {track_id} moved to index {destination_index}",
                path.display()
            );
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::MarkerAdded { path, marker_id }) => {
            println!("Updated {}: added marker {marker_id}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::MarkerDeleted { path, marker_id }) => {
            println!("Updated {}: deleted marker {marker_id}", path.display());
            ExitCode::SUCCESS
        }
        Ok(prism_cli::CliOutcome::RecipeValidated {
            recipe_id,
            version,
            input_count,
        }) => {
            println!("Validated recipe {recipe_id} version {version} ({input_count} input(s))");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Prism: {error}");
            ExitCode::FAILURE
        }
    }
}
