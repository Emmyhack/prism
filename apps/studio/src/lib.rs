//! Prism Studio application-shell boundary.
//!
//! The graphical toolkit is intentionally not selected yet. This crate keeps
//! startup, project loading, and inspection behavior real and testable without
//! coupling the domain model to a UI framework.

use prism_persistence::{AutosaveCandidate, PersistenceError, list_autosaves, load};
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
    /// List revision-scoped autosaves for a project path.
    ListAutosaves {
        /// Active project path whose autosaves should be discovered.
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
    /// Number of locked tracks across all sequences.
    pub locked_track_count: usize,
    /// Number of muted tracks across all sequences.
    pub muted_track_count: usize,
    /// Number of sequence markers.
    pub marker_count: usize,
}

/// Stable autosave candidate summary exposed to recovery UI consumers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutosaveSummary {
    /// Autosave revision.
    pub revision: u64,
    /// Candidate path.
    pub path: PathBuf,
}

impl From<AutosaveCandidate> for AutosaveSummary {
    fn from(candidate: AutosaveCandidate) -> Self {
        Self {
            revision: candidate.revision,
            path: candidate.path,
        }
    }
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
        [project_flag, project] if project_flag == "--list-autosaves" => {
            Ok(StudioCommand::ListAutosaves {
                project: PathBuf::from(project),
            })
        }
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
                locked_track_count: project
                    .sequences
                    .values()
                    .flat_map(|sequence| sequence.tracks.iter())
                    .filter(|track| track.locked)
                    .count(),
                muted_track_count: project
                    .sequences
                    .values()
                    .flat_map(|sequence| sequence.tracks.iter())
                    .filter(|track| track.muted)
                    .count(),
                marker_count: project
                    .sequences
                    .values()
                    .map(|sequence| sequence.markers.len())
                    .sum(),
            };
            Ok(Some(serde_json::to_string_pretty(&summary)?))
        }
        StudioCommand::ListAutosaves { project } => {
            let candidates = list_autosaves(&project)?
                .into_iter()
                .map(AutosaveSummary::from)
                .collect::<Vec<_>>();
            Ok(Some(serde_json::to_string_pretty(&candidates)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AutosaveSummary, ProjectSummary, StudioCommand, StudioError, parse};
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
        assert!(matches!(
            parse(vec!["--list-autosaves".to_owned(), "edit.prism".to_owned()]),
            Ok(StudioCommand::ListAutosaves { project }) if project == Path::new("edit.prism")
        ));
    }

    #[test]
    fn rejects_unknown_shell_arguments() {
        assert!(matches!(
            parse(vec!["--unknown".to_owned()]),
            Err(StudioError::Usage)
        ));
    }

    #[test]
    fn summary_exposes_timeline_state_for_ui_consumers() -> Result<(), serde_json::Error> {
        let summary = ProjectSummary {
            id: "project".to_owned(),
            name: "Main".to_owned(),
            revision: 3,
            media_count: 1,
            sequence_count: 1,
            track_count: 2,
            clip_count: 4,
            locked_track_count: 1,
            muted_track_count: 1,
            marker_count: 2,
        };
        let json = serde_json::to_string(&summary)?;
        assert!(json.contains("lockedTrackCount"));
        assert!(json.contains("mutedTrackCount"));
        assert!(json.contains("markerCount"));
        Ok(())
    }

    #[test]
    fn autosave_summary_serializes_recovery_fields() -> Result<(), serde_json::Error> {
        let summary = AutosaveSummary {
            revision: 7,
            path: Path::new("edit.prism.autosave.7").to_owned(),
        };
        let json = serde_json::to_string(&summary)?;
        assert!(json.contains("revision"));
        assert!(json.contains("edit.prism.autosave.7"));
        Ok(())
    }
}
