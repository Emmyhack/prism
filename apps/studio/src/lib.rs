//! Prism Studio application-shell boundary.
//!
//! The graphical toolkit is intentionally not selected yet. This crate keeps
//! startup, project loading, and inspection behavior real and testable without
//! coupling the domain model to a UI framework.

/// Toolkit-neutral professional desktop workspace and design system.
pub mod desktop;

use prism_commands::{CommandCategory, CommandRegistry, UndoStrategy};
use prism_media::{FfprobeInspector, MediaArtifactKind, MediaCatalog, MediaItemKind};
use prism_persistence::{AutosaveCandidate, PersistenceError, list_autosaves, load};
use prism_recipe::RecipePermission;
use prism_script::{
    DebugSnapshot, ExecutionError, PermissionGrantError, PermissionGrantSet, RuntimeBundleError,
    RuntimeBundleManifest, TransactionDebugger, TransactionRequest,
};
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
    /// Probe all media referenced by a project for the browser.
    ListMedia {
        /// Project database to inspect.
        project: PathBuf,
        /// Explicit `ffprobe` executable.
        ffprobe: PathBuf,
    },
    /// List revision-scoped autosaves for a project path.
    ListAutosaves {
        /// Active project path whose autosaves should be discovered.
        project: PathBuf,
    },
    /// Print the stable default keyboard bindings.
    ListShortcuts,
    /// Print the command registry consumed by menus, tools, and shortcuts.
    ListCommands,
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
    /// Persist one explicit permission decision.
    SetPermission {
        /// Permission review sidecar.
        permissions: PathBuf,
        /// Requested capability to update.
        capability: RecipePermission,
        /// New grant state.
        granted: bool,
    },
    /// Inspect a packaged scripting runtime bundle.
    InspectRuntime {
        /// Runtime bundle manifest.
        manifest: PathBuf,
    },
    /// Build the complete scripting workspace model consumed by Studio views.
    ScriptWorkspace {
        /// Script catalog directory.
        directory: PathBuf,
        /// Permission review sidecar.
        permissions: PathBuf,
        /// Runtime bundle manifest.
        runtime: PathBuf,
    },
    /// Validate and inspect a transaction at operation breakpoints.
    DebugTransaction {
        /// Authoritative project database.
        project: PathBuf,
        /// Serialized transaction request.
        request: PathBuf,
        /// Zero-based operation breakpoints.
        breakpoints: Vec<usize>,
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
    /// Media probing failed before a catalog could be built.
    #[error("could not build media catalog: {0}")]
    Media(#[from] prism_media::MediaError),
    /// Summary serialization failed.
    #[error("could not serialize project summary: {0}")]
    Json(#[from] serde_json::Error),
    /// Script catalog filesystem access failed.
    #[error("could not inspect script catalog: {0}")]
    Io(#[from] std::io::Error),
    /// A permission decision was invalid.
    #[error("invalid script permission review: {0}")]
    Permission(#[from] PermissionGrantError),
    /// A packaged runtime manifest was invalid.
    #[error("invalid scripting runtime bundle: {0}")]
    Runtime(#[from] RuntimeBundleError),
    /// Script transaction debugging failed validation.
    #[error("could not debug script transaction: {0}")]
    Debug(#[from] ExecutionError),
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

/// Stable media catalog item exposed to the native browser adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCatalogSummary {
    /// Referenced source path.
    pub path: PathBuf,
    /// Broad media classification.
    pub kind: String,
    /// Whether the source is currently unavailable.
    pub offline: bool,
    /// Probe diagnostic, when present.
    pub diagnostic: Option<String>,
    /// Exact duration represented as `value/timescale` when available.
    pub duration: Option<String>,
    /// Content-addressed thumbnail artifact key, when an asset identity exists.
    pub thumbnail_cache_key: String,
    /// Content-addressed waveform artifact key, when an asset identity exists.
    pub waveform_cache_key: String,
}

fn media_kind_name(kind: MediaItemKind) -> &'static str {
    match kind {
        MediaItemKind::Video => "video",
        MediaItemKind::Audio => "audio",
        MediaItemKind::Image => "image",
        MediaItemKind::Unknown => "unknown",
    }
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
    /// Insert source material at the playhead.
    Insert,
    /// Ripple-delete the selected edit.
    RippleDelete,
    /// Step one frame backward.
    StepBackward,
    /// Step one frame forward.
    StepForward,
    /// Toggle timeline snapping.
    ToggleSnap,
    /// Add a sequence marker.
    AddMarker,
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

/// UI-neutral command descriptor for menu and shortcut adapters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSummary {
    /// Stable command identifier.
    pub id: String,
    /// User-facing command label.
    pub label: String,
    /// Registry category.
    pub category: String,
    /// Default shortcut, when one is assigned.
    pub shortcut: Option<String>,
    /// Undo classification.
    pub undo: String,
}

fn command_summaries() -> Vec<CommandSummary> {
    CommandRegistry::descriptors()
        .iter()
        .map(|descriptor| CommandSummary {
            id: descriptor.id.to_owned(),
            label: descriptor.label.to_owned(),
            category: match descriptor.category {
                CommandCategory::Project => "project",
                CommandCategory::Timeline => "timeline",
                CommandCategory::Playback => "playback",
                CommandCategory::Application => "application",
            }
            .to_owned(),
            shortcut: descriptor.shortcut.map(str::to_owned),
            undo: match descriptor.undo {
                UndoStrategy::Transaction => "transaction",
                UndoStrategy::Ephemeral => "ephemeral",
            }
            .to_owned(),
        })
        .collect()
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

/// Packaged runtime information exposed to the Studio scripting workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSummary {
    /// Runtime bundle release.
    pub version: String,
    /// Resolved Python executable.
    pub python: Option<PathBuf>,
    /// Resolved Node executable.
    pub node: Option<PathBuf>,
    /// Resolved TypeScript compiler executable.
    pub typescript_compiler: Option<PathBuf>,
}

/// Complete UI-independent state for the Studio scripting workspace.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptWorkspaceSummary {
    /// Deterministically ordered authoring files.
    pub scripts: Vec<ScriptSummary>,
    /// Current requested/granted permission decisions.
    pub permissions: PermissionSummary,
    /// Validated packaged runtime availability.
    pub runtime: RuntimeSummary,
    /// Whether execution can proceed without another permission decision.
    pub ready_to_run: bool,
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
    let review = read_permission_review(path)?;
    let requested = review
        .requested
        .iter()
        .copied()
        .map(permission_name)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let granted = review
        .granted
        .iter()
        .copied()
        .map(permission_name)
        .map(str::to_owned)
        .collect::<Vec<_>>();
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

fn read_permission_review(path: impl AsRef<Path>) -> Result<PermissionGrantSet, StudioError> {
    let review: PermissionGrantSet = serde_json::from_slice(&fs::read(path)?)?;
    Ok(review.normalize()?)
}

fn permission_name(permission: RecipePermission) -> &'static str {
    match permission {
        RecipePermission::ProjectRead => "project.read",
        RecipePermission::ProjectWrite => "project.write",
        RecipePermission::MediaRead => "media.read",
        RecipePermission::MediaImport => "media.import",
        RecipePermission::ExportCreate => "export.create",
        RecipePermission::FilesystemRead => "filesystem.read",
        RecipePermission::FilesystemWrite => "filesystem.write",
        RecipePermission::NetworkAccess => "network.access",
        RecipePermission::PluginInvoke => "plugin.invoke",
    }
}

/// Persists one requested permission decision and returns the updated review.
///
/// # Errors
///
/// Returns a permission error for undeclared capabilities, or an I/O/JSON
/// error if the sidecar cannot be read or written.
pub fn set_permission(
    path: impl AsRef<Path>,
    capability: RecipePermission,
    granted: bool,
) -> Result<PermissionSummary, StudioError> {
    let path = path.as_ref();
    let mut review = read_permission_review(path)?;
    review.set_granted(capability, granted)?;
    fs::write(path, serde_json::to_vec_pretty(&review)?)?;
    inspect_permissions(path)
}

/// Loads and validates a packaged scripting runtime manifest.
///
/// # Errors
///
/// Returns a manifest, filesystem, or runtime validation error.
pub fn inspect_runtime(path: impl AsRef<Path>) -> Result<RuntimeSummary, StudioError> {
    let path = path.as_ref();
    let manifest: RuntimeBundleManifest = serde_json::from_slice(&fs::read(path)?)?;
    let directory = path.parent().ok_or(StudioError::Usage)?;
    let runtime = manifest.resolve(directory)?;
    Ok(RuntimeSummary {
        version: runtime.version,
        python: runtime.python,
        node: runtime.node,
        typescript_compiler: runtime.typescript_compiler,
    })
}

/// Builds the complete scripting workspace state for graphical adapters.
///
/// # Errors
///
/// Returns a catalog, permission, runtime, or serialization boundary error.
pub fn script_workspace(
    directory: impl AsRef<Path>,
    permissions: impl AsRef<Path>,
    runtime: impl AsRef<Path>,
) -> Result<ScriptWorkspaceSummary, StudioError> {
    let permissions = inspect_permissions(permissions)?;
    let ready_to_run = permissions.denied.is_empty();
    Ok(ScriptWorkspaceSummary {
        scripts: list_scripts(directory)?,
        permissions,
        runtime: inspect_runtime(runtime)?,
        ready_to_run,
    })
}

/// Probes every media reference in a project for a browser or relink view.
///
/// # Errors
///
/// Returns persistence errors while opening the project or media-probe errors
/// while constructing the catalog.
pub fn list_project_media(
    project: impl AsRef<Path>,
    ffprobe: impl Into<PathBuf>,
) -> Result<Vec<MediaCatalogSummary>, StudioError> {
    let project = load(project.as_ref())?;
    let paths = project
        .media
        .values()
        .map(|asset| PathBuf::from(&asset.source_path));
    let catalog = MediaCatalog::probe(paths, &FfprobeInspector::new(ffprobe))?;
    Ok(catalog
        .entries()
        .iter()
        .map(|entry| MediaCatalogSummary {
            path: entry.path.clone(),
            kind: media_kind_name(entry.kind).to_owned(),
            offline: entry.offline,
            diagnostic: entry.diagnostic.clone(),
            duration: entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.duration)
                .map(|time| format!("{}/{}", time.value(), time.timescale())),
            thumbnail_cache_key: entry
                .artifact_key(MediaArtifactKind::Thumbnail, b"320x180")
                .as_hex(),
            waveform_cache_key: entry
                .artifact_key(MediaArtifactKind::Waveform, b"1024")
                .as_hex(),
        })
        .collect())
}

/// Validates a transaction and returns its initial and breakpoint snapshots.
///
/// The project is never mutated; committing remains a separate atomic action.
///
/// # Errors
///
/// Returns a project, file, JSON, protocol, revision, or command error.
pub fn debug_transaction(
    project: impl AsRef<Path>,
    request: impl AsRef<Path>,
    breakpoints: &[usize],
) -> Result<Vec<DebugSnapshot>, StudioError> {
    let project = load(project.as_ref())?;
    let request: TransactionRequest = serde_json::from_slice(&fs::read(request)?)?;
    let mut debugger = TransactionDebugger::start(&project, request, breakpoints)?;
    let mut snapshots = vec![debugger.snapshot()];
    while !snapshots.last().is_some_and(|snapshot| snapshot.complete) {
        snapshots.push(debugger.continue_to_breakpoint());
    }
    Ok(snapshots)
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
            action: ShortcutAction::Insert,
            key: ",".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::RippleDelete,
            key: "Backspace".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::StepBackward,
            key: "Left".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::StepForward,
            key: "Right".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::ToggleSnap,
            key: "S".to_owned(),
            modifiers: vec![],
        },
        ShortcutBinding {
            action: ShortcutAction::AddMarker,
            key: "M".to_owned(),
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
        [media_flag, project, ffprobe] if media_flag == "--list-media" => {
            Ok(StudioCommand::ListMedia {
                project: PathBuf::from(project),
                ffprobe: PathBuf::from(ffprobe),
            })
        }
        [project_flag, project] if project_flag == "--list-autosaves" => {
            Ok(StudioCommand::ListAutosaves {
                project: PathBuf::from(project),
            })
        }
        [shortcuts_flag] if shortcuts_flag == "--list-shortcuts" => {
            Ok(StudioCommand::ListShortcuts)
        }
        [commands_flag] if commands_flag == "--list-commands" => Ok(StudioCommand::ListCommands),
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
        [permission_flag, permissions, capability, granted]
            if permission_flag == "--set-permission" =>
        {
            let capability = serde_json::from_value(serde_json::Value::String(capability.clone()))?;
            let granted = match granted.as_str() {
                "true" => true,
                "false" => false,
                _ => return Err(StudioError::Usage),
            };
            Ok(StudioCommand::SetPermission {
                permissions: PathBuf::from(permissions),
                capability,
                granted,
            })
        }
        [runtime_flag, manifest] if runtime_flag == "--inspect-runtime" => {
            Ok(StudioCommand::InspectRuntime {
                manifest: PathBuf::from(manifest),
            })
        }
        [workspace_flag, directory, permissions, runtime]
            if workspace_flag == "--script-workspace" =>
        {
            Ok(StudioCommand::ScriptWorkspace {
                directory: PathBuf::from(directory),
                permissions: PathBuf::from(permissions),
                runtime: PathBuf::from(runtime),
            })
        }
        [debug_flag, project, request, breakpoints] if debug_flag == "--debug-transaction" => {
            let breakpoints = if breakpoints.trim().is_empty() {
                Vec::new()
            } else {
                breakpoints
                    .split(',')
                    .map(|value| value.parse::<usize>().map_err(|_| StudioError::Usage))
                    .collect::<Result<Vec<_>, _>>()?
            };
            Ok(StudioCommand::DebugTransaction {
                project: PathBuf::from(project),
                request: PathBuf::from(request),
                breakpoints,
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
        StudioCommand::ListMedia { project, ffprobe } => Ok(Some(serde_json::to_string_pretty(
            &list_project_media(project, ffprobe)?,
        )?)),
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
        StudioCommand::ListCommands => {
            Ok(Some(serde_json::to_string_pretty(&command_summaries())?))
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
        StudioCommand::SetPermission {
            permissions,
            capability,
            granted,
        } => Ok(Some(serde_json::to_string_pretty(&set_permission(
            permissions,
            capability,
            granted,
        )?)?)),
        StudioCommand::InspectRuntime { manifest } => Ok(Some(serde_json::to_string_pretty(
            &inspect_runtime(manifest)?,
        )?)),
        StudioCommand::ScriptWorkspace {
            directory,
            permissions,
            runtime,
        } => Ok(Some(serde_json::to_string_pretty(&script_workspace(
            directory,
            permissions,
            runtime,
        )?)?)),
        StudioCommand::DebugTransaction {
            project,
            request,
            breakpoints,
        } => Ok(Some(serde_json::to_string_pretty(&debug_transaction(
            project,
            request,
            &breakpoints,
        )?)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AutosaveSummary, ProjectSummary, ScriptLanguage, ShortcutAction, StudioCommand,
        StudioError, default_shortcuts, inspect_permissions, inspect_runtime, inspect_script,
        list_scripts, parse, resolve_shortcut, script_workspace, set_permission,
    };
    use prism_recipe::RecipePermission;
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
            parse(vec!["--list-commands".to_owned()]),
            Ok(StudioCommand::ListCommands)
        ));
        assert!(matches!(
            parse(vec!["--list-scripts".to_owned(), "scripts".to_owned()]),
            Ok(StudioCommand::ListScripts { directory }) if directory == Path::new("scripts")
        ));
        assert!(matches!(
            parse(vec!["--inspect-permissions".to_owned(), "permissions.json".to_owned()]),
            Ok(StudioCommand::InspectPermissions { permissions }) if permissions == Path::new("permissions.json")
        ));
        assert!(matches!(
            parse(vec![
                "--debug-transaction".to_owned(),
                "edit.prism".to_owned(),
                "request.json".to_owned(),
                "1,3".to_owned()
            ]),
            Ok(StudioCommand::DebugTransaction { breakpoints, .. }) if breakpoints == vec![1, 3]
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

    #[test]
    fn scripting_workspace_combines_catalog_permissions_and_packaged_runtime()
    -> Result<(), Box<dyn std::error::Error>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("prism-studio-workspace-{nonce}"));
        std::fs::create_dir_all(root.join("bin"))?;
        std::fs::write(root.join("main.py"), "print('ok')\n")?;
        std::fs::write(root.join("bin/python"), "runtime")?;
        let permissions = root.join("permissions.json");
        std::fs::write(
            &permissions,
            r#"{"requested":["project.read"],"granted":[]}"#,
        )?;
        let runtime = root.join("runtime.json");
        std::fs::write(
            &runtime,
            r#"{"apiVersion":1,"version":"1.0.0","python":"bin/python","node":null,"typescriptCompiler":null}"#,
        )?;

        let pending = script_workspace(&root, &permissions, &runtime)?;
        assert!(!pending.ready_to_run);
        assert_eq!(pending.scripts.len(), 1);
        assert!(inspect_runtime(&runtime)?.python.is_some());
        let reviewed = set_permission(&permissions, RecipePermission::ProjectRead, true)?;
        assert!(reviewed.denied.is_empty());
        assert!(script_workspace(&root, &permissions, &runtime)?.ready_to_run);

        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
