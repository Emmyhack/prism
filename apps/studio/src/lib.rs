//! Prism Studio application-shell boundary.
//!
//! The graphical toolkit is intentionally not selected yet. This crate keeps
//! startup, project loading, and inspection behavior real and testable without
//! coupling the domain model to a UI framework.

use prism_persistence::{PersistenceError, load};
use serde::Serialize;
use std::path::PathBuf;
use thiserror::Error;

/// Parsed Studio shell invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudioCommand {
    /// Start the shell without opening a project.
    Start,
    /// Open a project and print its authoritative summary.
    Inspect {
        /// Project database to open.
        project: PathBuf,
    },
}

/// Structured shell failures.
#[derive(Debug, Error)]
pub enum StudioError {
    /// Command-line arguments are malformed.
    #[error("invalid Prism Studio arguments")]
    Usage,
    /// Project loading failed.
    #[error("could not open project: {0}")]
    Persistence(#[from] PersistenceError),
    /// Summary serialization failed.
    #[error("could not serialize project summary: {0}")]
    Json(#[from] serde_json::Error),
}

/// Stable, UI-independent project summary exposed to the shell and future UI.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    /// Stable project identifier.
    pub id: String,
    /// User-visible project name.
    pub name: String,
    /// Monotonic command revision.
    pub revision: u64,
    /// Number of imported assets.
    pub media_count: usize,
    /// Number of sequences.
    pub sequence_count: usize,
    /// Number of tracks across all sequences.
    pub track_count: usize,
    /// Number of clips across all tracks.
    pub clip_count: usize,
}

/// Parses one Studio shell invocation.
///
/// # Errors
///
/// Returns [`StudioError::Usage`] for unsupported argument shapes.
pub fn parse<I>(arguments: I) -> Result<StudioCommand, StudioError>
where
    I: IntoIterator<Item = String>,
{
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(StudioCommand::Start),
        [project_flag, project] if project_flag == "--project" => Ok(StudioCommand::Inspect {
            project: PathBuf::from(project),
        }),
        _ => Err(StudioError::Usage),
    }
}

/// Runs the non-graphical Studio shell command.
///
/// # Errors
///
/// Returns persistence or serialization errors while opening and summarizing a
/// project.
pub fn run(command: StudioCommand) -> Result<Option<String>, StudioError> {
    match command {
        StudioCommand::Start => Ok(None),
        StudioCommand::Inspect { project } => {
            let project = load(&project)?;
            let summary = ProjectSummary {
                id: project.id.as_uuid().to_string(),
                name: project.name.clone(),
                revision: project.revision(),
                media_count: project.media.len(),
                sequence_count: project.sequences.len(),
                track_count: project
                    .sequences
                    .values()
                    .map(|sequence| sequence.tracks.len())
                    .sum(),
                clip_count: project
                    .sequences
                    .values()
                    .flat_map(|sequence| sequence.tracks.iter())
                    .map(|track| track.clips.len())
                    .sum(),
            };
            Ok(Some(serde_json::to_string_pretty(&summary)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StudioCommand, StudioError, parse};
    use std::path::Path;

    #[test]
    fn parses_start_and_project_inspection() {
        assert!(matches!(
            parse(Vec::<String>::new()),
            Ok(StudioCommand::Start)
        ));
        assert!(matches!(
            parse(vec!["--project".to_owned(), "edit.prism".to_owned()]),
            Ok(StudioCommand::Inspect { project }) if project == Path::new("edit.prism")
        ));
    }

    #[test]
    fn rejects_unknown_shell_arguments() {
        assert!(matches!(
            parse(vec!["--unknown".to_owned()]),
            Err(StudioError::Usage)
        ));
    }
}
