//! Dependency-light command-line workflows backed by Prism's core APIs.

use prism_commands::{Command, CommandEngine};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_export::{
    BasicMp4Exporter, ExportError, ExportPreset, ExportReport, SequenceMp4ExportRequest,
    SingleTrackMp4ExportRequest, VideoAudioMp4ExportRequest,
};
use prism_media::{
    FfmpegProxyGenerator, FfprobeInspector, MediaError, ProxyProfile, ProxyRequest, StreamKind,
};
use prism_persistence::save_atomic;
use prism_persistence::{
    PersistenceError, list_autosaves, load, recover_autosave, save_autosave, save_backup,
};
use prism_project::{Project, TrackKind, TransitionAlignment, TransitionKind};
use prism_recipe::{RecipeLanguage, RecipePermission};
use prism_script::{
    ExternalScriptRunner, PermissionGrantSet, ProcessError, ProcessLimits, RecipeExecutionPlan,
    RuntimeBundleManifest, ScriptWorker, TransactionRequest, TypeScriptCompiler,
};
use prism_timeline::{RationalTime, TimeError};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;

const DEFAULT_VIDEO_BITRATE_KBPS: u32 = 5_000;

/// Parsed `export-track` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportTrackArgs {
    /// Validated project database to load.
    pub project: PathBuf,
    /// Sequence to render.
    pub sequence_id: SequenceId,
    /// Video track to render.
    pub track_id: TrackId,
    /// New MP4 destination.
    pub output: PathBuf,
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
    /// Exact output frame rate.
    pub frame_rate: RationalTime,
    /// Target video bitrate in kilobits per second.
    pub video_bitrate_kbps: u32,
    /// Explicit encoder executable.
    pub ffmpeg: PathBuf,
    /// Explicit validation-probe executable.
    pub ffprobe: PathBuf,
}

/// Parsed `export-preset-track` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportPresetTrackArgs {
    /// Validated project database to load.
    pub project: PathBuf,
    /// Sequence to render.
    pub sequence_id: SequenceId,
    /// Video track to render.
    pub track_id: TrackId,
    /// Named geometry/frame-rate preset.
    pub preset: ExportPreset,
    /// Optional target video bitrate override in kilobits per second.
    pub video_bitrate_kbps: Option<u32>,
    /// New MP4 destination.
    pub output: PathBuf,
    /// Explicit encoder executable.
    pub ffmpeg: PathBuf,
    /// Explicit validation-probe executable.
    pub ffprobe: PathBuf,
}

/// Parsed `export-video-audio` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportVideoAudioArgs {
    /// Validated project database to load.
    pub project: PathBuf,
    /// Sequence to render.
    pub sequence_id: SequenceId,
    /// Video track to render.
    pub video_track_id: TrackId,
    /// Audio track to render and mux.
    pub audio_track_id: TrackId,
    /// New MP4 destination.
    pub output: PathBuf,
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
    /// Exact output frame rate.
    pub frame_rate: RationalTime,
    /// Target video bitrate in kilobits per second.
    pub video_bitrate_kbps: u32,
    /// Target AAC audio bitrate in kilobits per second.
    pub audio_bitrate_kbps: u32,
    /// Explicit encoder executable.
    pub ffmpeg: PathBuf,
    /// Explicit validation-probe executable.
    pub ffprobe: PathBuf,
}

type ScriptPermissionFile = PermissionGrantSet;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecipeExportRequest {
    sequence_id: Uuid,
    output: PathBuf,
    preset: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecipeCompleteOutput {
    transaction: TransactionRequest,
    export: RecipeExportRequest,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RecipeWorkerOutput {
    Transaction(TransactionRequest),
    Complete(RecipeCompleteOutput),
}

fn recipe_export_runtime(
    export: Option<&RecipeExportRequest>,
    request: &TransactionRequest,
    permissions: &ScriptPermissionFile,
) -> Result<Option<(PathBuf, PathBuf, ExportPreset)>, CliError> {
    let Some(export) = export else {
        return Ok(None);
    };
    if request.dry_run {
        return Err(CliError::Command(
            "a recipe export cannot use a dry-run transaction".to_owned(),
        ));
    }
    if !permissions
        .granted
        .contains(&RecipePermission::ExportCreate)
    {
        return Err(CliError::Command(
            "recipe permission was not granted: ExportCreate".to_owned(),
        ));
    }
    let ffmpeg = std::env::var_os("PRISM_FFMPEG")
        .map(PathBuf::from)
        .ok_or_else(|| CliError::Command("PRISM_FFMPEG is not set".to_owned()))?;
    let ffprobe = std::env::var_os("PRISM_FFPROBE")
        .map(PathBuf::from)
        .ok_or_else(|| CliError::Command("PRISM_FFPROBE is not set".to_owned()))?;
    Ok(Some((
        ffmpeg,
        ffprobe,
        ExportPreset::parse(&export.preset)?,
    )))
}

fn read_permissions(path: impl AsRef<Path>) -> Result<ScriptPermissionFile, CliError> {
    let permissions: ScriptPermissionFile = serde_json::from_slice(
        &std::fs::read(path).map_err(|error| CliError::Command(error.to_string()))?,
    )?;
    permissions
        .normalize()
        .map_err(|error| CliError::Command(error.to_string()))
}

/// Successful CLI operation.
#[derive(Debug)]
pub enum CliOutcome {
    /// Informational text requested without mutating state.
    Information(String),
    /// A finalized media export.
    Exported(ExportReport),
    /// A finalized proxy artifact.
    ProxyGenerated {
        /// Finalized proxy path.
        path: PathBuf,
    },
    /// Versioned project inspection JSON.
    Inspected(String),
    /// A newly created, atomically saved project.
    ProjectCreated {
        /// Saved project path.
        path: PathBuf,
        /// Stable project UUID.
        project_id: String,
    },
    /// A new video sequence and first clip were committed and saved.
    VideoSequenceCreated {
        /// Updated project path.
        path: PathBuf,
        /// New sequence UUID.
        sequence_id: String,
        /// New video-track UUID.
        track_id: String,
    },
    /// A clip was appended to an existing video track.
    ClipAdded {
        /// Updated project path.
        path: PathBuf,
        /// New clip UUID.
        clip_id: String,
    },
    /// A clip was moved and the project was saved.
    ClipMoved {
        /// Updated project path.
        path: PathBuf,
        /// Moved clip UUID.
        clip_id: String,
        /// New exact timeline start.
        timeline_start: RationalTime,
    },
    /// A clip was trimmed and the project was saved.
    ClipTrimmed {
        /// Updated project path.
        path: PathBuf,
        /// Trimmed clip UUID.
        clip_id: String,
    },
    /// A clip was split and the project was saved.
    ClipSplit {
        /// Updated project path.
        path: PathBuf,
        /// Existing left clip UUID.
        clip_id: String,
        /// Newly created right clip UUID.
        right_clip_id: String,
    },
    /// A clip was deleted and the project was saved.
    ClipDeleted {
        /// Updated project path.
        path: PathBuf,
        /// Deleted clip UUID.
        clip_id: String,
    },
    /// A video clip was duplicated and the project was saved.
    ClipDuplicated {
        /// Updated project path.
        path: PathBuf,
        /// Source clip UUID.
        clip_id: String,
        /// New duplicate UUID.
        new_clip_id: String,
    },
    /// A video clip was ripple-deleted and the project was saved.
    ClipRippleDeleted {
        /// Updated project path.
        path: PathBuf,
        /// Removed clip UUID.
        clip_id: String,
    },
    /// A transition was added and the project was saved.
    TransitionAdded {
        /// Updated project path.
        path: PathBuf,
        /// Added transition UUID.
        transition_id: String,
    },
    /// A transition was deleted and the project was saved.
    TransitionDeleted {
        /// Updated project path.
        path: PathBuf,
        /// Deleted transition UUID.
        transition_id: String,
    },
    /// A protocol transaction was validated or committed.
    ScriptTransaction {
        /// Updated project path.
        path: PathBuf,
        /// Revision after the operation (unchanged for dry-run).
        revision: u64,
        /// Whether the request was validation-only.
        dry_run: bool,
        /// Number of validated command events.
        event_count: usize,
    },
    /// A no-clobber recovery backup was created.
    BackupCreated {
        /// Backup destination.
        path: PathBuf,
        /// Project revision captured by the backup.
        revision: u64,
    },
    /// A revision-scoped autosave was created.
    AutosaveCreated {
        /// Autosave destination.
        path: PathBuf,
        /// Captured project revision.
        revision: u64,
    },
    /// A JSON list of recoverable autosave candidates.
    AutosavesListed {
        /// Versioned candidate listing.
        json: String,
    },
    /// An autosave was validated and restored to a new project path.
    AutosaveRecovered {
        /// Recovered project destination.
        path: PathBuf,
        /// Revision captured by the autosave.
        revision: u64,
    },
    /// A track lock state was changed and saved.
    TrackLockChanged {
        /// Updated project path.
        path: PathBuf,
        /// Updated track ID.
        track_id: String,
        /// New lock state.
        locked: bool,
    },
    /// A track visibility state was changed and saved.
    TrackVisibilityChanged {
        /// Updated project path.
        path: PathBuf,
        /// Updated track ID.
        track_id: String,
        /// New visibility state.
        visible: bool,
    },
    /// A track mute state was changed and saved.
    TrackMuteChanged {
        /// Updated project path.
        path: PathBuf,
        /// Updated track ID.
        track_id: String,
        /// New mute state.
        muted: bool,
    },
    /// A track was reordered and the project was saved.
    TrackReordered {
        /// Updated project path.
        path: PathBuf,
        /// Reordered track ID.
        track_id: String,
        /// New zero-based position.
        destination_index: usize,
    },
    /// A sequence marker was added and the project was saved.
    MarkerAdded {
        /// Updated project path.
        path: PathBuf,
        /// Added marker ID.
        marker_id: String,
    },
    /// A sequence marker was deleted and the project was saved.
    MarkerDeleted {
        /// Updated project path.
        path: PathBuf,
        /// Deleted marker ID.
        marker_id: String,
    },
    /// A recipe manifest and its inputs were validated without execution.
    RecipeValidated {
        /// Stable recipe identifier.
        recipe_id: String,
        /// Recipe release version.
        version: String,
        /// Number of supplied input values.
        input_count: usize,
    },
}

/// CLI parse and workflow failures.
#[derive(Debug, Error)]
pub enum CliError {
    /// No supported subcommand was supplied.
    #[error(
        "usage: prism-cli <create-project|inspect|create-video-sequence|add-video-clip|add-audio-clip|move-video-clip|trim-video-clip|split-video-clip|delete-video-clip|duplicate-video-clip|ripple-delete-video-clip|add-transition|delete-transition|generate-proxy|set-track-locked|set-track-visible|set-track-muted|reorder-track|add-marker|delete-marker|script-transaction|run-python-script|run-javascript-script|run-typescript-script|validate-recipe|run-recipe|backup-project|autosave-project|list-autosaves|recover-autosave|export-track|export-preset-track|export-video-audio> ..."
    )]
    Usage,
    /// A flag was unknown or repeated.
    #[error("unknown or duplicate option: {0}")]
    InvalidOption(String),
    /// A required flag was absent.
    #[error("missing required option: {0}")]
    MissingOption(&'static str),
    /// A flag requiring textual parsing was not valid UTF-8.
    #[error("option value is not valid UTF-8: {0}")]
    NonUtf8Value(&'static str),
    /// A typed option value was malformed.
    #[error("invalid value for {option}: {value}")]
    InvalidValue {
        /// Flag name.
        option: &'static str,
        /// Rejected value.
        value: String,
    },
    /// Exact rate construction failed.
    #[error("invalid frame rate: {0}")]
    Time(#[from] TimeError),
    /// Project loading failed.
    #[error("could not load project: {0}")]
    Persistence(#[from] PersistenceError),
    /// Validated export failed.
    #[error("could not export track: {0}")]
    Export(#[from] ExportError),
    /// Inspection JSON serialization failed.
    #[error("could not serialize project inspection: {0}")]
    Json(#[from] serde_json::Error),
    /// External script process supervision failed.
    #[error("could not run script: {0}")]
    ScriptProcess(#[from] ProcessError),
    /// Project creation never overwrites an existing path.
    #[error("project already exists: {0}")]
    ProjectExists(PathBuf),
    /// Media inspection failed before mutation.
    #[error("could not inspect media: {0}")]
    Media(#[from] MediaError),
    /// Inspected media did not contain video.
    #[error("media has no video stream")]
    NoVideoStream,
    /// Inspected media did not contain audio.
    #[error("media has no audio stream")]
    NoAudioStream,
    /// Requested exact range exceeded inspected duration.
    #[error("clip source range exceeds inspected media duration")]
    SourceRangeOutOfBounds,
    /// The shared command engine rejected the edit.
    #[error("could not edit project: {0}")]
    Command(String),
}

/// Parses and runs one CLI invocation, excluding the executable name.
///
/// # Errors
///
/// Returns [`CliError`] for invalid arguments, project loading, or export.
pub fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<CliOutcome, CliError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.first().and_then(|argument| argument.to_str()) {
        Some("--help" | "-h") if arguments.len() == 1 => {
            Ok(CliOutcome::Information(CliError::Usage.to_string()))
        }
        Some("--version" | "-V") if arguments.len() == 1 => Ok(CliOutcome::Information(format!(
            "prism-cli {}",
            env!("CARGO_PKG_VERSION")
        ))),
        Some("export-track") => run_export_track(arguments),
        Some("export-preset-track") => run_export_preset_track(arguments),
        Some("export-video-audio") => run_export_video_audio(arguments),
        Some("inspect") => run_inspect(&arguments),
        Some("create-project") => run_create_project(&arguments),
        Some("create-video-sequence") => run_create_video_sequence(&arguments),
        Some("add-video-clip") => run_add_video_clip(&arguments),
        Some("add-audio-clip") => run_add_audio_clip(&arguments),
        Some("move-video-clip") => run_move_video_clip(&arguments),
        Some("trim-video-clip") => run_trim_video_clip(&arguments),
        Some("split-video-clip") => run_split_video_clip(&arguments),
        Some("delete-video-clip") => run_delete_video_clip(&arguments),
        Some("duplicate-video-clip") => run_duplicate_video_clip(&arguments),
        Some("ripple-delete-video-clip") => run_ripple_delete_video_clip(&arguments),
        Some("add-transition") => run_add_transition(&arguments),
        Some("delete-transition") => run_delete_transition(&arguments),
        Some("generate-proxy") => run_generate_proxy(&arguments),
        Some("set-track-locked") => run_set_track_locked(&arguments),
        Some("set-track-visible") => run_set_track_visible(&arguments),
        Some("set-track-muted") => run_set_track_muted(&arguments),
        Some("reorder-track") => run_reorder_track(&arguments),
        Some("add-marker") => run_add_marker(&arguments),
        Some("delete-marker") => run_delete_marker(&arguments),
        Some("script-transaction") => run_script_transaction(&arguments),
        Some("run-python-script") => run_python_script(&arguments),
        Some("run-javascript-script") => run_javascript_script(&arguments),
        Some("run-typescript-script") => run_typescript_script(&arguments),
        Some("validate-recipe") => run_validate_recipe(&arguments),
        Some("run-recipe") => run_recipe(&arguments),
        Some("backup-project") => run_backup_project(&arguments),
        Some("autosave-project") => run_autosave_project(&arguments),
        Some("list-autosaves") => run_list_autosaves(&arguments),
        Some("recover-autosave") => run_recover_autosave(&arguments),
        Some(_) | None => Err(CliError::Usage),
    }
}

mod edit_commands;
mod export_commands;
mod project_commands;
mod script_commands;

use edit_commands::{
    run_add_audio_clip, run_add_video_clip, run_create_project, run_create_video_sequence,
    run_delete_video_clip, run_duplicate_video_clip, run_move_video_clip,
    run_ripple_delete_video_clip, run_split_video_clip, run_trim_video_clip,
};
pub use export_commands::{
    INSPECTION_FORMAT_VERSION, parse_export_preset_track, parse_export_track,
    parse_export_video_audio,
};
use export_commands::{
    parse_bool, parse_exact_time, parse_u32, parse_usize, parse_uuid, run_export_preset_track,
    run_export_track, run_export_video_audio, run_inspect,
};
use project_commands::{
    run_add_marker, run_add_transition, run_autosave_project, run_backup_project,
    run_delete_marker, run_delete_transition, run_generate_proxy, run_list_autosaves, run_recipe,
    run_recover_autosave, run_reorder_track, run_set_track_locked, run_set_track_muted,
    run_set_track_visible, run_validate_recipe,
};
use script_commands::{
    run_javascript_script, run_python_script, run_script_transaction, run_typescript_script,
};

#[cfg(test)]
use export_commands::ProjectInspection;

#[cfg(test)]
mod tests;
