//! Prism Studio application-shell boundary.
//!
//! The graphical toolkit is intentionally not selected yet. This crate keeps
//! startup, project loading, and inspection behavior real and testable without
//! coupling the domain model to a UI framework.

use prism_persistence::{AutosaveCandidate, PersistenceError, list_autosaves, load};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};
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
    /// Print the stable default keyboard bindings.
    ListShortcuts,
    /// List supported scripts in a directory.
    ListScripts {
        /// Directory containing script files.
        directory: PathBuf,
    },
    /// Inspect one supported script file.
    InspectScript {
        /// Script file to inspect.
        script: PathBuf,
    },
    /// Inspect a script permission sidecar.
    InspectPermissions {
        /// Permission manifest JSON file.
        permissions: PathBuf,
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
    /// Script catalog filesystem access failed.
    #[error("could not inspect script catalog: {0}")]
    Io(#[from] std::io::Error),
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

/// Stable actions exposed to a future keyboard-shortcut layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ShortcutAction {
    /// Activate selection editing.
    SelectTool,
    /// Activate razor splitting.
    RazorTool,
    /// Activate trim editing.
    TrimTool,
    /// Activate slip editing.
    SlipTool,
    /// Activate slide editing.
    SlideTool,
    /// Activate rolling edit.
    RollTool,
    /// Toggle playback.
    PlayPause,
    /// Undo the last transaction.
    Undo,
    /// Redo the last transaction.
    Redo,
}

/// One deterministic default keyboard binding.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutBinding {
    /// Action triggered by the binding.
    pub action: ShortcutAction,
    /// Platform-neutral key label.
    pub key: String,
    /// Required modifier labels.
    pub modifiers: Vec<String>,
}

/// Supported authoring languages in the Studio script catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptLanguage {
    /// Python source.
    Python,
    /// JavaScript source.
    JavaScript,
    /// TypeScript source.
    TypeScript,
}

/// Stable metadata surfaced to a script browser and diagnostics panel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptSummary {
    /// Script path.
    pub path: PathBuf,
    /// Inferred source language.
    pub language: ScriptLanguage,
    /// Source size in bytes.
    pub byte_count: usize,
    /// Number of source lines.
    pub line_count: usize,
}

/// Normalized permission review data for a script workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionSummary {
    /// Capabilities requested by the script.
    pub requested: Vec<String>,
    /// Capabilities granted by the host.
    pub granted: Vec<String>,
    /// Requested capabilities not granted by the host.
    pub denied: Vec<String>,
}

fn script_language(path: &Path) -> Option<ScriptLanguage> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "py" => Some(ScriptLanguage::Python),
        "js" | "mjs" | "cjs" => Some(ScriptLanguage::JavaScript),
        "ts" | "mts" | "cts" => Some(ScriptLanguage::TypeScript),
        _ => None,
    }
}

/// Reads one script and returns stable metadata for workspace consumers.
///
/// # Errors
///
/// Returns [`StudioError::Usage`] for unsupported extensions or an I/O error
/// when the file cannot be read.
pub fn inspect_script(path: impl AsRef<Path>) -> Result<ScriptSummary, StudioError> {
    let path = path.as_ref();
    let language = script_language(path).ok_or(StudioError::Usage)?;
    let source = fs::read(path)?;
    Ok(ScriptSummary {
        path: path.to_owned(),
        language,
        byte_count: source.len(),
        line_count: if source.is_empty() {
            0
        } else {
            source.split(|byte| *byte == b'\n').count()
        },
    })
}

/// Enumerates supported scripts in one directory in deterministic path order.
///
/// # Errors
///
/// Returns an I/O error when the directory or one of its supported scripts
/// cannot be read.
pub fn list_scripts(directory: impl AsRef<Path>) -> Result<Vec<ScriptSummary>, StudioError> {
    let mut paths = fs::read_dir(directory.as_ref())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && script_language(path).is_some())
        .collect::<Vec<_>>();
    paths.sort();
    paths.into_iter().map(inspect_script).collect()
}

/// Reads and normalizes a requested/granted permission sidecar.
///
/// # Errors
///
/// Returns a JSON error for malformed manifests or an I/O error when the file
/// cannot be read.
pub fn inspect_permissions(path: impl AsRef<Path>) -> Result<PermissionSummary, StudioError> {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let values = |field: &str| -> Result<Vec<String>, StudioError> {
        let entries = value
            .get(field)
            .and_then(serde_json::Value::as_array)
            .ok_or(StudioError::Usage)?;
        let mut output = entries
            .iter()
            .map(|entry| entry.as_str().map(str::to_owned).ok_or(StudioError::Usage))
            .collect::<Result<Vec<_>, _>>()?;
        output.sort();
        output.dedup();
        Ok(output)
    };
    let requested = values("requested")?;
    let granted = values("granted")?;
    let denied = requested
        .iter()
        .filter(|permission| granted.binary_search(permission).is_err())
        .cloned()
        .collect();
    Ok(PermissionSummary {
        requested,
        granted,
        denied,
    })
}

/// Resolves a platform-neutral key event against the default shortcut map.
///
/// Key and modifier labels are compared case-insensitively. Modifier order is
/// ignored so native adapters can pass events in their platform-defined order.
#[must_use]
pub fn resolve_shortcut(key: &str, modifiers: &[&str]) -> Option<ShortcutAction> {
    let key = key.trim().to_ascii_lowercase();
    let mut requested = modifiers
        .iter()
        .map(|modifier| modifier.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    requested.sort();
    default_shortcuts().into_iter().find_map(|binding| {
        if binding.key.to_ascii_lowercase() != key {
            return None;
        }
        let mut expected = binding
            .modifiers
            .iter()
            .map(|modifier| modifier.to_ascii_lowercase())
            .collect::<Vec<_>>();
        expected.sort();
        (expected == requested).then_some(binding.action)
    })
}

/// Returns the default keyboard map shared by Studio and future UI adapters.
#[must_use]
pub fn default_shortcuts() -> Vec<ShortcutBinding> {
    vec![
        ShortcutBinding {
            action: ShortcutAction::SelectTool,
            key: "V".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::RazorTool,
            key: "C".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::TrimTool,
            key: "T".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::SlipTool,
            key: "Y".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::SlideTool,
            key: "U".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::RollTool,
            key: "R".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::PlayPause,
            key: "Space".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::Undo,
            key: "Z".to_owned(),
            modifiers: vec!["Primary".to_owned()],
        },
        ShortcutBinding {
            action: ShortcutAction::Redo,
            key: "Z".to_owned(),
            modifiers: vec!["Primary".to_owned(), "Shift".to_owned()],
        },
    ]
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
        [shortcuts_flag] if shortcuts_flag == "--list-shortcuts" => {
            Ok(StudioCommand::ListShortcuts)
        }
        [scripts_flag, directory] if scripts_flag == "--list-scripts" => {
            Ok(StudioCommand::ListScripts {
                directory: PathBuf::from(directory),
            })
        }
        [script_flag, script] if script_flag == "--inspect-script" => {
            Ok(StudioCommand::InspectScript {
                script: PathBuf::from(script),
            })
        }
        [permissions_flag, permissions] if permissions_flag == "--inspect-permissions" => {
            Ok(StudioCommand::InspectPermissions {
                permissions: PathBuf::from(permissions),
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
        StudioCommand::ListShortcuts => {
            Ok(Some(serde_json::to_string_pretty(&default_shortcuts())?))
        }
        StudioCommand::ListScripts { directory } => Ok(Some(serde_json::to_string_pretty(
            &list_scripts(directory)?,
        )?)),
        StudioCommand::InspectScript { script } => Ok(Some(serde_json::to_string_pretty(
            &inspect_script(script)?,
        )?)),
        StudioCommand::InspectPermissions { permissions } => Ok(Some(
            serde_json::to_string_pretty(&inspect_permissions(permissions)?)?,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AutosaveSummary, ProjectSummary, ScriptLanguage, ShortcutAction, StudioCommand,
        StudioError, default_shortcuts, inspect_permissions, inspect_script, list_scripts, parse,
        resolve_shortcut,
    };
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
        assert!(matches!(
            parse(vec!["--list-shortcuts".to_owned()]),
            Ok(StudioCommand::ListShortcuts)
        ));
        assert!(matches!(
            parse(vec!["--list-scripts".to_owned(), "scripts".to_owned()]),
            Ok(StudioCommand::ListScripts { directory }) if directory == Path::new("scripts")
        ));
        assert!(matches!(
            parse(vec!["--inspect-permissions".to_owned(), "permissions.json".to_owned()]),
            Ok(StudioCommand::InspectPermissions { permissions }) if permissions == Path::new("permissions.json")
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

    #[test]
    fn default_shortcuts_cover_phase_two_editing_tools() {
        let shortcuts = default_shortcuts();
        assert!(
            shortcuts
                .iter()
                .any(|binding| binding.action == ShortcutAction::SlipTool)
        );
        assert!(
            shortcuts
                .iter()
                .any(|binding| binding.action == ShortcutAction::SlideTool)
        );
        assert!(
            shortcuts
                .iter()
                .any(|binding| binding.action == ShortcutAction::RollTool)
        );
        assert!(
            shortcuts
                .iter()
                .any(|binding| binding.action == ShortcutAction::Undo)
        );
        assert!(
            shortcuts
                .iter()
                .any(|binding| binding.action == ShortcutAction::Redo)
        );
    }

    #[test]
    fn shortcut_resolution_is_case_and_order_insensitive() {
        assert_eq!(
            resolve_shortcut("z", &["Shift", "PRIMARY"]),
            Some(ShortcutAction::Redo)
        );
        assert_eq!(
            resolve_shortcut(" space ", &[]),
            Some(ShortcutAction::PlayPause)
        );
        assert_eq!(resolve_shortcut("z", &["Shift"]), None);
    }

    #[test]
    fn script_catalog_reports_supported_files_deterministically()
    -> Result<(), Box<dyn std::error::Error>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("prism-studio-scripts-{nonce}"));
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("b.ts"), "const value = 1;\n")?;
        std::fs::write(root.join("a.py"), "print('ok')\n")?;
        std::fs::write(root.join("notes.txt"), "ignored\n")?;
        let scripts = list_scripts(&root)?;
        assert_eq!(scripts.len(), 2);
        assert_eq!(
            scripts[0].path.file_name().and_then(|name| name.to_str()),
            Some("a.py")
        );
        assert_eq!(scripts[0].language, ScriptLanguage::Python);
        assert_eq!(scripts[1].language, ScriptLanguage::TypeScript);
        assert_eq!(inspect_script(root.join("a.py"))?.line_count, 2);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn permission_review_reports_denied_capabilities() -> Result<(), Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("prism-studio-permissions-{}", std::process::id()));
        std::fs::write(
            &path,
            r#"{"requested":["project.write","media.read","project.write"],"granted":["media.read"]}"#,
        )?;
        let summary = inspect_permissions(&path)?;
        assert_eq!(summary.requested, vec!["media.read", "project.write"]);
        assert_eq!(summary.granted, vec!["media.read"]);
        assert_eq!(summary.denied, vec!["project.write"]);
        std::fs::remove_file(path)?;
        Ok(())
    }
}
