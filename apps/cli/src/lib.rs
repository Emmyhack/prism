//! Dependency-light command-line workflows backed by Prism's core APIs.

use prism_commands::{Command, CommandEngine};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_export::{
    BasicMp4Exporter, ExportError, ExportPreset, ExportReport, SingleTrackMp4ExportRequest,
    VideoAudioMp4ExportRequest,
};
use prism_media::{
    FfmpegProxyGenerator, FfprobeInspector, MediaError, ProxyProfile, ProxyRequest, StreamKind,
};
use prism_persistence::save_atomic;
use prism_persistence::{
    PersistenceError, list_autosaves, load, recover_autosave, save_autosave, save_backup,
};
use prism_project::{Project, TrackKind, TransitionKind};
use prism_recipe::RecipePermission;
use prism_script::{
    ExternalScriptRunner, ProcessError, ProcessLimits, ScriptWorker, TransactionRequest,
    TypeScriptCompiler,
};
use prism_timeline::{RationalTime, TimeError};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::{ffi::OsString, path::PathBuf, time::Duration};
use thiserror::Error;
use uuid::Uuid;

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
    /// Explicit encoder executable.
    pub ffmpeg: PathBuf,
    /// Explicit validation-probe executable.
    pub ffprobe: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScriptPermissionFile {
    requested: Vec<RecipePermission>,
    granted: Vec<RecipePermission>,
}

/// Successful CLI operation.
#[derive(Debug)]
pub enum CliOutcome {
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
        "usage: prism-cli <create-project|inspect|create-video-sequence|add-video-clip|add-audio-clip|move-video-clip|trim-video-clip|split-video-clip|delete-video-clip|duplicate-video-clip|ripple-delete-video-clip|add-transition|delete-transition|generate-proxy|set-track-locked|set-track-visible|set-track-muted|reorder-track|add-marker|delete-marker|script-transaction|run-python-script|run-javascript-script|run-typescript-script|validate-recipe|backup-project|autosave-project|list-autosaves|recover-autosave|export-track|export-preset-track|export-video-audio> ..."
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
        Some("backup-project") => run_backup_project(&arguments),
        Some("autosave-project") => run_autosave_project(&arguments),
        Some("list-autosaves") => run_list_autosaves(&arguments),
        Some("recover-autosave") => run_recover_autosave(&arguments),
        Some(_) | None => Err(CliError::Usage),
    }
}

fn run_set_track_visible(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_set_track_boolean(arguments, "set-track-visible", "--visible", true)
}

fn run_set_track_muted(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_set_track_boolean(arguments, "set-track-muted", "--muted", false)
}

fn run_set_track_boolean(
    arguments: &[OsString],
    command_name: &str,
    state_flag: &'static str,
    visible_state: bool,
) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        flag,
        value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != command_name
        || project_flag != "--project"
        || track_flag != "--track"
        || flag != state_flag
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let state = parse_bool(value, state_flag)?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .iter()
        .find(|(_, sequence)| sequence.tracks.iter().any(|track| track.id == track_id))
        .map(|(id, _)| *id)
        .ok_or_else(|| CliError::Command("track not found".to_owned()))?;
    let command = if visible_state {
        Command::SetTrackVisible {
            sequence_id,
            track_id,
            visible: state,
        }
    } else {
        Command::SetTrackMuted {
            sequence_id,
            track_id,
            muted: state,
        }
    };
    CommandEngine::execute(&mut project, command)
        .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    if visible_state {
        Ok(CliOutcome::TrackVisibilityChanged {
            path: project_path,
            track_id: track_id.as_uuid().to_string(),
            visible: state,
        })
    } else {
        Ok(CliOutcome::TrackMuteChanged {
            path: project_path,
            track_id: track_id.as_uuid().to_string(),
            muted: state,
        })
    }
}

fn run_reorder_track(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        index_flag,
        index_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "reorder-track"
        || project_flag != "--project"
        || track_flag != "--track"
        || index_flag != "--index"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let destination_index = parse_usize(index_value, "--index")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .iter()
        .find(|(_, sequence)| sequence.tracks.iter().any(|track| track.id == track_id))
        .map(|(id, _)| *id)
        .ok_or_else(|| CliError::Command("track not found".to_owned()))?;
    CommandEngine::execute(
        &mut project,
        Command::ReorderTrack {
            sequence_id,
            track_id,
            destination_index,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TrackReordered {
        path: project_path,
        track_id: track_id.as_uuid().to_string(),
        destination_index,
    })
}

fn run_add_marker(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        marker_flag,
        marker_value,
        time_flag,
        time_value,
        label_flag,
        label_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-marker"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || marker_flag != "--marker"
        || time_flag != "--time"
        || label_flag != "--label"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let marker_id = MarkerId::from_uuid(parse_uuid(marker_value.clone(), "--marker")?);
    let time = parse_exact_time(time_value.clone(), "--time")?;
    let label = label_value
        .to_str()
        .ok_or(CliError::NonUtf8Value("--label"))?
        .to_owned();
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::AddMarker {
            sequence_id,
            id: marker_id,
            time,
            label,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::MarkerAdded {
        path: project_path,
        marker_id: marker_id.as_uuid().to_string(),
    })
}

fn run_add_transition(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        transition_flag,
        transition_value,
        outgoing_flag,
        outgoing_value,
        incoming_flag,
        incoming_value,
        duration_flag,
        duration_value,
        kind_flag,
        kind_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-transition"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || transition_flag != "--transition"
        || outgoing_flag != "--outgoing-clip"
        || incoming_flag != "--incoming-clip"
        || duration_flag != "--duration"
        || kind_flag != "--kind"
    {
        return Err(CliError::Usage);
    }
    if kind_value != "dissolve" {
        return Err(CliError::InvalidValue {
            option: "--kind",
            value: kind_value.to_string_lossy().into_owned(),
        });
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let transition_id =
        TransitionId::from_uuid(parse_uuid(transition_value.clone(), "--transition")?);
    let outgoing_clip = ClipId::from_uuid(parse_uuid(outgoing_value.clone(), "--outgoing-clip")?);
    let incoming_clip = ClipId::from_uuid(parse_uuid(incoming_value.clone(), "--incoming-clip")?);
    let duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::AddTransition {
            sequence_id,
            id: transition_id,
            outgoing_clip,
            incoming_clip,
            duration,
            kind: TransitionKind::Dissolve,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TransitionAdded {
        path: project_path,
        transition_id: transition_id.as_uuid().to_string(),
    })
}

fn run_delete_transition(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        transition_flag,
        transition_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "delete-transition"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || transition_flag != "--transition"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let transition_id =
        TransitionId::from_uuid(parse_uuid(transition_value.clone(), "--transition")?);
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::DeleteTransition {
            sequence_id,
            transition_id,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TransitionDeleted {
        path: project_path,
        transition_id: transition_id.as_uuid().to_string(),
    })
}

fn run_generate_proxy(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let (source, output, width, height, video_codec, audio_codec, ffmpeg) = match arguments {
        [
            command,
            source_flag,
            source,
            output_flag,
            output,
            width_flag,
            width,
            height_flag,
            height,
            video_codec_flag,
            video_codec,
            ffmpeg_flag,
            ffmpeg,
        ] if command == "generate-proxy"
            && source_flag == "--source"
            && output_flag == "--output"
            && width_flag == "--width"
            && height_flag == "--height"
            && video_codec_flag == "--video-codec"
            && ffmpeg_flag == "--ffmpeg" =>
        {
            (
                PathBuf::from(source),
                PathBuf::from(output),
                parse_u32(width.clone(), "--width")?,
                parse_u32(height.clone(), "--height")?,
                video_codec
                    .clone()
                    .into_string()
                    .map_err(|_| CliError::NonUtf8Value("--video-codec"))?,
                None,
                PathBuf::from(ffmpeg),
            )
        }
        [
            command,
            source_flag,
            source,
            output_flag,
            output,
            width_flag,
            width,
            height_flag,
            height,
            video_codec_flag,
            video_codec,
            audio_codec_flag,
            audio_codec,
            ffmpeg_flag,
            ffmpeg,
        ] if command == "generate-proxy"
            && source_flag == "--source"
            && output_flag == "--output"
            && width_flag == "--width"
            && height_flag == "--height"
            && video_codec_flag == "--video-codec"
            && audio_codec_flag == "--audio-codec"
            && ffmpeg_flag == "--ffmpeg" =>
        {
            (
                PathBuf::from(source),
                PathBuf::from(output),
                parse_u32(width.clone(), "--width")?,
                parse_u32(height.clone(), "--height")?,
                video_codec
                    .clone()
                    .into_string()
                    .map_err(|_| CliError::NonUtf8Value("--video-codec"))?,
                Some(
                    audio_codec
                        .clone()
                        .into_string()
                        .map_err(|_| CliError::NonUtf8Value("--audio-codec"))?,
                ),
                PathBuf::from(ffmpeg),
            )
        }
        _ => return Err(CliError::Usage),
    };
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("proxy");
    let profile = ProxyProfile::new(width, height, video_codec, audio_codec, extension)?;
    let request = ProxyRequest {
        source,
        output: output.clone(),
        profile,
    };
    FfmpegProxyGenerator::new(ffmpeg).generate(&request, None)?;
    Ok(CliOutcome::ProxyGenerated { path: output })
}

fn run_delete_marker(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        marker_flag,
        marker_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "delete-marker"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || marker_flag != "--marker"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let marker_id = MarkerId::from_uuid(parse_uuid(marker_value.clone(), "--marker")?);
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::DeleteMarker {
            sequence_id,
            marker_id,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::MarkerDeleted {
        path: project_path,
        marker_id: marker_id.as_uuid().to_string(),
    })
}

fn run_set_track_locked(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        locked_flag,
        locked_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "set-track-locked"
        || project_flag != "--project"
        || track_flag != "--track"
        || locked_flag != "--locked"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let locked_text = locked_value
        .to_str()
        .ok_or(CliError::NonUtf8Value("--locked"))?;
    let locked = match locked_text {
        "true" => true,
        "false" => false,
        value => {
            return Err(CliError::InvalidValue {
                option: "--locked",
                value: value.to_owned(),
            });
        }
    };
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .iter()
        .find(|(_, sequence)| sequence.tracks.iter().any(|track| track.id == track_id))
        .map(|(id, _)| *id)
        .ok_or_else(|| CliError::Command("track not found".to_owned()))?;
    CommandEngine::execute(
        &mut project,
        Command::SetTrackLocked {
            sequence_id,
            track_id,
            locked,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TrackLockChanged {
        path: project_path,
        track_id: track_id.as_uuid().to_string(),
        locked,
    })
}

fn run_validate_recipe(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let (manifest_path, inputs_path) = match arguments {
        [command, manifest_flag, manifest_path]
            if command == "validate-recipe" && manifest_flag == "--manifest" =>
        {
            (PathBuf::from(manifest_path), None)
        }
        [
            command,
            manifest_flag,
            manifest_path,
            inputs_flag,
            inputs_path,
        ] if command == "validate-recipe"
            && manifest_flag == "--manifest"
            && inputs_flag == "--inputs" =>
        {
            (
                PathBuf::from(manifest_path),
                Some(PathBuf::from(inputs_path)),
            )
        }
        _ => return Err(CliError::Usage),
    };
    let manifest: prism_recipe::RecipeManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|error| CliError::Command(error.to_string()))?,
    )?;
    let inputs = match inputs_path {
        Some(path) => serde_json::from_slice(
            &std::fs::read(path).map_err(|error| CliError::Command(error.to_string()))?,
        )?,
        None => serde_json::Map::new(),
    };
    let request = prism_script::RecipeRequest { manifest, inputs };
    let plan = request
        .into_execution_plan()
        .map_err(|error| CliError::Command(error.to_string()))?;
    Ok(CliOutcome::RecipeValidated {
        recipe_id: plan.manifest.id,
        version: plan.manifest.version,
        input_count: plan.inputs.len(),
    })
}

fn run_backup_project(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        output_flag,
        output_path,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "backup-project" || project_flag != "--project" || output_flag != "--output" {
        return Err(CliError::Usage);
    }
    let project = load(PathBuf::from(project_path).as_path())?;
    let revision = project.revision();
    let output_path = PathBuf::from(output_path);
    save_backup(&project, &output_path)?;
    Ok(CliOutcome::BackupCreated {
        path: output_path,
        revision,
    })
}

fn run_autosave_project(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [command, project_flag, project_path] = arguments else {
        return Err(CliError::Usage);
    };
    if command != "autosave-project" || project_flag != "--project" {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let project = load(&project_path)?;
    let revision = project.revision();
    let path = save_autosave(&project, &project_path)?;
    Ok(CliOutcome::AutosaveCreated { path, revision })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AutosaveSummary {
    revision: u64,
    path: String,
}

fn run_list_autosaves(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [command, project_flag, project_path] = arguments else {
        return Err(CliError::Usage);
    };
    if command != "list-autosaves" || project_flag != "--project" {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let candidates = list_autosaves(&project_path)?;
    let summaries = candidates
        .into_iter()
        .map(|candidate| AutosaveSummary {
            revision: candidate.revision,
            path: candidate.path.to_string_lossy().into_owned(),
        })
        .collect::<Vec<_>>();
    Ok(CliOutcome::AutosavesListed {
        json: serde_json::to_string_pretty(&summaries)?,
    })
}

fn run_recover_autosave(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [command, source_flag, source_path, output_flag, output_path] = arguments else {
        return Err(CliError::Usage);
    };
    if command != "recover-autosave" || source_flag != "--source" || output_flag != "--output" {
        return Err(CliError::Usage);
    }
    let output = PathBuf::from(output_path);
    let revision = recover_autosave(PathBuf::from(source_path).as_path(), &output)?;
    Ok(CliOutcome::AutosaveRecovered {
        path: output,
        revision,
    })
}

fn run_script_transaction(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        request_flag,
        request_path,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "script-transaction" || project_flag != "--project" || request_flag != "--request"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let request_path = PathBuf::from(request_path);
    let payload =
        std::fs::read(&request_path).map_err(|error| CliError::Command(error.to_string()))?;
    let mut project = load(&project_path)?;
    let result = prism_script::ScriptWorker::default()
        .execute_json(&mut project, &payload, &AtomicBool::new(false))
        .map_err(|error| CliError::Command(error.to_string()))?;
    if !result.dry_run {
        save_atomic(&project, &project_path)?;
    }
    Ok(CliOutcome::ScriptTransaction {
        path: project_path,
        revision: result.revision,
        dry_run: result.dry_run,
        event_count: result.event_count,
    })
}

fn run_python_script(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_language_script(arguments, "run-python-script", "--python")
}

fn run_javascript_script(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_language_script(arguments, "run-javascript-script", "--node")
}

fn run_typescript_script(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        script_flag,
        script_path,
        compiler_flag,
        compiler_path,
        node_flag,
        node_path,
        permissions_flag,
        permissions_path,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "run-typescript-script"
        || project_flag != "--project"
        || script_flag != "--script"
        || compiler_flag != "--tsc"
        || node_flag != "--node"
        || permissions_flag != "--permissions"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let script_path = PathBuf::from(script_path);
    let permissions: ScriptPermissionFile = serde_json::from_slice(
        &std::fs::read(permissions_path).map_err(|error| CliError::Command(error.to_string()))?,
    )?;
    let mut project = load(&project_path)?;
    let revision = project.revision().to_string();
    let cancelled = AtomicBool::new(false);
    let output_directory =
        std::env::temp_dir().join(format!("prism-typescript-{}", Uuid::new_v4()));
    let result = (|| {
        let compiler = TypeScriptCompiler::new(
            PathBuf::from(compiler_path),
            ProcessLimits {
                timeout: Duration::from_secs(30),
                max_output_bytes: 8 * 1024 * 1024,
            },
        );
        let compiled_path = compiler.compile(
            &script_path,
            &output_directory,
            &permissions.requested,
            &permissions.granted,
            &cancelled,
        )?;
        let runner = ExternalScriptRunner::new(
            PathBuf::from(node_path),
            ProcessLimits {
                timeout: Duration::from_secs(30),
                max_output_bytes: 8 * 1024 * 1024,
            },
        );
        let output = runner.run_authorized(
            &compiled_path,
            &[],
            &[("PRISM_PROJECT_REVISION", revision)],
            &permissions.requested,
            &permissions.granted,
            &cancelled,
        )?;
        let request: TransactionRequest = serde_json::from_slice(&output.stdout)?;
        let execution = ScriptWorker::default()
            .execute_authorized(&mut project, request, &cancelled, &permissions.granted)
            .map_err(|error| CliError::Command(error.to_string()))?;
        if !execution.dry_run {
            save_atomic(&project, &project_path)?;
        }
        Ok::<_, CliError>(CliOutcome::ScriptTransaction {
            path: project_path.clone(),
            revision: execution.revision,
            dry_run: execution.dry_run,
            event_count: execution.event_count,
        })
    })();
    let _ = std::fs::remove_dir_all(&output_directory);
    result
}

fn run_language_script(
    arguments: &[OsString],
    command_name: &str,
    interpreter_flag: &str,
) -> Result<CliOutcome, CliError> {
    let (project_path, script_path, interpreter_path, permission_path) = match arguments {
        [
            command,
            project_flag,
            project_path,
            script_flag,
            script_path,
            interpreter_flag_arg,
            interpreter_path,
        ] if command == command_name
            && project_flag == "--project"
            && script_flag == "--script"
            && interpreter_flag_arg == interpreter_flag =>
        {
            (project_path, script_path, interpreter_path, None)
        }
        [
            command,
            project_flag,
            project_path,
            script_flag,
            script_path,
            interpreter_flag_arg,
            interpreter_path,
            permissions_flag,
            permissions_path,
        ] if command == command_name
            && project_flag == "--project"
            && script_flag == "--script"
            && interpreter_flag_arg == interpreter_flag
            && permissions_flag == "--permissions" =>
        {
            (
                project_path,
                script_path,
                interpreter_path,
                Some(PathBuf::from(permissions_path)),
            )
        }
        _ => return Err(CliError::Usage),
    };
    let project_path = PathBuf::from(project_path);
    let script_path = PathBuf::from(script_path);
    let mut project = load(&project_path)?;
    let revision = project.revision().to_string();
    let permissions = match permission_path {
        Some(path) => serde_json::from_slice::<ScriptPermissionFile>(
            &std::fs::read(path).map_err(|error| CliError::Command(error.to_string()))?,
        )?,
        None => ScriptPermissionFile {
            requested: Vec::new(),
            granted: Vec::new(),
        },
    };
    let runner = ExternalScriptRunner::new(
        PathBuf::from(interpreter_path),
        ProcessLimits {
            timeout: Duration::from_secs(30),
            max_output_bytes: 8 * 1024 * 1024,
        },
    );
    let cancelled = AtomicBool::new(false);
    let output = runner.run_authorized(
        &script_path,
        &[],
        &[("PRISM_PROJECT_REVISION", revision)],
        &permissions.requested,
        &permissions.granted,
        &cancelled,
    )?;
    let request: TransactionRequest = serde_json::from_slice(&output.stdout)?;
    let result = ScriptWorker::default()
        .execute_authorized(&mut project, request, &cancelled, &permissions.granted)
        .map_err(|error| CliError::Command(error.to_string()))?;
    if !result.dry_run {
        save_atomic(&project, &project_path)?;
    }
    Ok(CliOutcome::ScriptTransaction {
        path: project_path,
        revision: result.revision,
        dry_run: result.dry_run,
        event_count: result.event_count,
    })
}

fn run_delete_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "delete-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute_transaction(
        &mut project,
        "Delete video clip",
        vec![Command::DeleteClip {
            sequence_id,
            track_id,
            clip_id,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipDeleted {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

fn run_ripple_delete_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "ripple-delete-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute(
        &mut project,
        Command::RippleDelete {
            sequence_id,
            track_id,
            clip_id,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipRippleDeleted {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

fn run_duplicate_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        new_flag,
        new_value,
        timeline_flag,
        timeline_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "duplicate-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || new_flag != "--new-clip"
        || timeline_flag != "--timeline-start"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let new_clip_id = ClipId::from_uuid(parse_uuid(new_value.clone(), "--new-clip")?);
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute(
        &mut project,
        Command::DuplicateClip {
            sequence_id,
            track_id,
            clip_id,
            new_clip_id,
            timeline_start,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipDuplicated {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
        new_clip_id: new_clip_id.as_uuid().to_string(),
    })
}

fn run_split_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        position_flag,
        position_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "split-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || position_flag != "--timeline-position"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let timeline_position = parse_exact_time(position_value.clone(), "--timeline-position")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    let right_clip_id = ClipId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Split video clip",
        vec![Command::SplitClip {
            sequence_id,
            track_id,
            clip_id,
            right_clip_id,
            timeline_position,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipSplit {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
        right_clip_id: right_clip_id.as_uuid().to_string(),
    })
}

fn run_trim_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        timeline_flag,
        timeline_value,
        source_flag,
        source_value,
        duration_flag,
        duration_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "trim-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || timeline_flag != "--timeline-start"
        || source_flag != "--source-start"
        || duration_flag != "--duration"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let source_start = parse_exact_time(source_value.clone(), "--source-start")?;
    let source_duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute_transaction(
        &mut project,
        "Trim video clip",
        vec![Command::TrimClip {
            sequence_id,
            track_id,
            clip_id,
            timeline_start,
            source_start,
            source_duration,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipTrimmed {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

fn run_move_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        timeline_flag,
        timeline_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "move-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || timeline_flag != "--timeline-start"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute_transaction(
        &mut project,
        "Move video clip",
        vec![Command::MoveClip {
            sequence_id,
            track_id,
            clip_id,
            timeline_start,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipMoved {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
        timeline_start,
    })
}

fn run_add_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        media_flag,
        media_value,
        source_flag,
        source_value,
        timeline_flag,
        timeline_value,
        duration_flag,
        duration_value,
        probe_flag,
        probe_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || media_flag != "--media"
        || source_flag != "--source-start"
        || timeline_flag != "--timeline-start"
        || duration_flag != "--duration"
        || probe_flag != "--ffprobe"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let media_path = PathBuf::from(media_value);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let source_start = parse_exact_time(source_value.clone(), "--source-start")?;
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let metadata = FfprobeInspector::new(probe_value).inspect(&media_path)?;
    if !metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
    {
        return Err(CliError::NoVideoStream);
    }
    let requested_end = source_start.checked_add(duration)?;
    if !source_start.is_non_negative()
        || !duration.is_positive()
        || metadata
            .duration
            .is_none_or(|available| requested_end > available)
    {
        return Err(CliError::SourceRangeOutOfBounds);
    }
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence
                .tracks
                .iter()
                .any(|track| track.id == track_id && track.kind == TrackKind::Video)
        })
        .ok_or_else(|| CliError::Command("video track not found".to_owned()))?
        .id;
    let media_id = MediaAssetId::new();
    let clip_id = ClipId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Add video clip",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: media_path.to_string_lossy().into_owned(),
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: clip_id,
                media_id,
                timeline_start,
                source_start,
                source_duration: duration,
            },
        ],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipAdded {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

fn run_add_audio_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        media_flag,
        media_value,
        source_flag,
        source_value,
        timeline_flag,
        timeline_value,
        duration_flag,
        duration_value,
        probe_flag,
        probe_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-audio-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || media_flag != "--media"
        || source_flag != "--source-start"
        || timeline_flag != "--timeline-start"
        || duration_flag != "--duration"
        || probe_flag != "--ffprobe"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let media_path = PathBuf::from(media_value);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let source_start = parse_exact_time(source_value.clone(), "--source-start")?;
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let metadata = FfprobeInspector::new(probe_value).inspect(&media_path)?;
    if !metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Audio(_)))
    {
        return Err(CliError::NoAudioStream);
    }
    let requested_end = source_start.checked_add(duration)?;
    if !source_start.is_non_negative()
        || !duration.is_positive()
        || metadata
            .duration
            .is_some_and(|available| requested_end > available)
    {
        return Err(CliError::SourceRangeOutOfBounds);
    }
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence
                .tracks
                .iter()
                .any(|track| track.id == track_id && track.kind == TrackKind::Audio)
        })
        .ok_or_else(|| CliError::Command("audio track not found".to_owned()))?
        .id;
    let media_id = MediaAssetId::new();
    let clip_id = ClipId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Add audio clip",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: media_path.to_string_lossy().into_owned(),
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: clip_id,
                media_id,
                timeline_start,
                source_start,
                source_duration: duration,
            },
        ],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipAdded {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

fn run_create_video_sequence(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        media_flag,
        media_path,
        name_flag,
        name,
        start_flag,
        start,
        duration_flag,
        duration,
        probe_flag,
        probe,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "create-video-sequence"
        || project_flag != "--project"
        || media_flag != "--media"
        || name_flag != "--sequence-name"
        || start_flag != "--source-start"
        || duration_flag != "--duration"
        || probe_flag != "--ffprobe"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let media_path = PathBuf::from(media_path);
    let sequence_name = name
        .to_str()
        .ok_or(CliError::NonUtf8Value("--sequence-name"))?
        .to_owned();
    let source_start = parse_exact_time(start.clone(), "--source-start")?;
    let duration = parse_exact_time(duration.clone(), "--duration")?;
    let metadata = FfprobeInspector::new(probe).inspect(&media_path)?;
    if !metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
    {
        return Err(CliError::NoVideoStream);
    }
    let requested_end = source_start.checked_add(duration)?;
    if !source_start.is_non_negative()
        || !duration.is_positive()
        || metadata
            .duration
            .is_none_or(|available| requested_end > available)
    {
        return Err(CliError::SourceRangeOutOfBounds);
    }
    let mut project = load(&project_path)?;
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Create video sequence",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: media_path.to_string_lossy().into_owned(),
            },
            Command::CreateSequence {
                id: sequence_id,
                name: sequence_name,
            },
            Command::CreateTrack {
                sequence_id,
                id: track_id,
                kind: TrackKind::Video,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: ClipId::new(),
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start,
                source_duration: duration,
            },
        ],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::VideoSequenceCreated {
        path: project_path,
        sequence_id: sequence_id.as_uuid().to_string(),
        track_id: track_id.as_uuid().to_string(),
    })
}

fn run_create_project(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let parsed = parse_create_project(arguments)?;
    if parsed.path.exists() {
        return Err(CliError::ProjectExists(parsed.path));
    }
    let project = Project::new(parsed.name);
    project.validate().map_err(|error| CliError::InvalidValue {
        option: "--name",
        value: error.to_string(),
    })?;
    save_atomic(&project, &parsed.path)?;
    Ok(CliOutcome::ProjectCreated {
        path: parsed.path,
        project_id: project.id.as_uuid().to_string(),
    })
}

struct CreateProjectArgs {
    path: PathBuf,
    name: String,
}

fn parse_create_project(arguments: &[OsString]) -> Result<CreateProjectArgs, CliError> {
    let Some(command) = arguments.first() else {
        return Err(CliError::Usage);
    };
    if command != "create-project" || arguments.len() != 5 {
        return Err(CliError::Usage);
    }
    let mut path = None;
    let mut name = None;
    for pair in arguments[1..].chunks_exact(2) {
        let flag = pair[0].to_str().ok_or(CliError::NonUtf8Value("option"))?;
        match flag {
            "--project" if path.is_none() => path = Some(PathBuf::from(&pair[1])),
            "--name" if name.is_none() => {
                name = Some(
                    pair[1]
                        .clone()
                        .into_string()
                        .map_err(|_| CliError::NonUtf8Value("--name"))?,
                );
            }
            _ => return Err(CliError::InvalidOption(flag.to_owned())),
        }
    }
    Ok(CreateProjectArgs {
        path: path.ok_or(CliError::MissingOption("--project"))?,
        name: name.ok_or(CliError::MissingOption("--name"))?,
    })
}

fn run_export_track(arguments: Vec<OsString>) -> Result<CliOutcome, CliError> {
    let parsed = parse_export_track(arguments)?;
    let project = load(&parsed.project)?;
    Ok(CliOutcome::Exported(
        BasicMp4Exporter::new(&parsed.ffmpeg, &parsed.ffprobe).export_single_video_track(
            &project,
            parsed.sequence_id,
            parsed.track_id,
            &SingleTrackMp4ExportRequest {
                width: parsed.width,
                height: parsed.height,
                frame_rate: parsed.frame_rate,
                video_bitrate_kbps: 8_000,
                output: parsed.output,
            },
        )?,
    ))
}

fn run_export_preset_track(arguments: Vec<OsString>) -> Result<CliOutcome, CliError> {
    let parsed = parse_export_preset_track(arguments)?;
    let project = load(&parsed.project)?;
    let request = SingleTrackMp4ExportRequest::from_preset(parsed.preset, parsed.output);
    Ok(CliOutcome::Exported(
        BasicMp4Exporter::new(&parsed.ffmpeg, &parsed.ffprobe).export_single_video_track(
            &project,
            parsed.sequence_id,
            parsed.track_id,
            &request,
        )?,
    ))
}

fn run_export_video_audio(arguments: Vec<OsString>) -> Result<CliOutcome, CliError> {
    let parsed = parse_export_video_audio(arguments)?;
    let project = load(&parsed.project)?;
    Ok(CliOutcome::Exported(
        BasicMp4Exporter::new(&parsed.ffmpeg, &parsed.ffprobe).export_single_video_audio_track(
            &project,
            parsed.sequence_id,
            parsed.video_track_id,
            parsed.audio_track_id,
            &VideoAudioMp4ExportRequest {
                width: parsed.width,
                height: parsed.height,
                frame_rate: parsed.frame_rate,
                video_bitrate_kbps: 8_000,
                output: parsed.output,
            },
        )?,
    ))
}

fn run_inspect(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [command, flag, path] = arguments else {
        return Err(CliError::Usage);
    };
    if command != "inspect" || flag != "--project" {
        return Err(CliError::Usage);
    }
    let project = load(&PathBuf::from(path))?;
    Ok(CliOutcome::Inspected(serde_json::to_string_pretty(
        &ProjectInspection::from(&project),
    )?))
}

/// Version of the CLI inspection JSON contract.
pub const INSPECTION_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectInspection {
    format_version: u32,
    project_id: String,
    name: String,
    schema_version: u32,
    revision: u64,
    media: Vec<MediaInspection>,
    sequences: Vec<SequenceInspection>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaInspection {
    id: String,
    source_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SequenceInspection {
    id: String,
    name: String,
    markers: Vec<MarkerInspection>,
    transitions: Vec<TransitionInspection>,
    tracks: Vec<TrackInspection>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MarkerInspection {
    id: String,
    time: TimeInspection,
    label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TransitionInspection {
    id: String,
    outgoing_clip: String,
    incoming_clip: String,
    duration: TimeInspection,
    kind: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrackInspection {
    id: String,
    kind: &'static str,
    locked: bool,
    visible: bool,
    muted: bool,
    clips: Vec<ClipInspection>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClipInspection {
    id: String,
    media_id: String,
    timeline_start: TimeInspection,
    source_start: TimeInspection,
    source_duration: TimeInspection,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct TimeInspection {
    value: i64,
    timescale: u32,
}

impl From<RationalTime> for TimeInspection {
    fn from(time: RationalTime) -> Self {
        Self {
            value: time.value(),
            timescale: time.timescale(),
        }
    }
}

impl From<&Project> for ProjectInspection {
    fn from(project: &Project) -> Self {
        Self {
            format_version: INSPECTION_FORMAT_VERSION,
            project_id: project.id.as_uuid().to_string(),
            name: project.name.clone(),
            schema_version: project.schema_version,
            revision: project.revision(),
            media: project
                .media
                .values()
                .map(|media| MediaInspection {
                    id: media.id.as_uuid().to_string(),
                    source_path: media.source_path.clone(),
                })
                .collect(),
            sequences: project
                .sequences
                .values()
                .map(|sequence| SequenceInspection {
                    id: sequence.id.as_uuid().to_string(),
                    name: sequence.name.clone(),
                    markers: sequence
                        .markers
                        .iter()
                        .map(|marker| MarkerInspection {
                            id: marker.id.as_uuid().to_string(),
                            time: marker.time.into(),
                            label: marker.label.clone(),
                        })
                        .collect(),
                    transitions: sequence
                        .transitions
                        .iter()
                        .map(|transition| TransitionInspection {
                            id: transition.id.as_uuid().to_string(),
                            outgoing_clip: transition.outgoing_clip.as_uuid().to_string(),
                            incoming_clip: transition.incoming_clip.as_uuid().to_string(),
                            duration: transition.duration.into(),
                            kind: match transition.kind {
                                prism_project::TransitionKind::Dissolve => "dissolve",
                            },
                        })
                        .collect(),
                    tracks: sequence
                        .tracks
                        .iter()
                        .map(|track| TrackInspection {
                            id: track.id.as_uuid().to_string(),
                            kind: match track.kind {
                                TrackKind::Video => "video",
                                TrackKind::Audio => "audio",
                            },
                            locked: track.locked,
                            visible: track.visible,
                            muted: track.muted,
                            clips: track
                                .clips
                                .iter()
                                .map(|clip| ClipInspection {
                                    id: clip.id.as_uuid().to_string(),
                                    media_id: clip.media_id.as_uuid().to_string(),
                                    timeline_start: clip.timeline_start.into(),
                                    source_start: clip.source_start.into(),
                                    source_duration: clip.source_duration.into(),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// Parses the supported non-interactive export command.
///
/// # Errors
///
/// Returns [`CliError`] for missing, duplicate, unknown, or malformed options.
pub fn parse_export_track(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<ExportTrackArgs, CliError> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("export-track")) {
        return Err(CliError::Usage);
    }
    let mut values = ParsedValues::default();
    while let Some(flag) = arguments.next() {
        let flag_text = flag.to_str().ok_or(CliError::NonUtf8Value("option"))?;
        let value = arguments
            .next()
            .ok_or(CliError::MissingOption("option value"))?;
        values.set(flag_text, value)?;
    }
    values.finish()
}

/// Parses the preset-driven single-track export command.
///
/// # Errors
///
/// Returns [`CliError`] for missing, duplicate, unknown, or malformed options.
pub fn parse_export_preset_track(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<ExportPresetTrackArgs, CliError> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("export-preset-track")) {
        return Err(CliError::Usage);
    }
    let mut values = ParsedPresetValues::default();
    while let Some(flag) = arguments.next() {
        let flag_text = flag.to_str().ok_or(CliError::NonUtf8Value("option"))?;
        let value = arguments
            .next()
            .ok_or(CliError::MissingOption("option value"))?;
        values.set(flag_text, value)?;
    }
    values.finish()
}

/// Parses the synchronized video/audio export command.
///
/// # Errors
///
/// Returns [`CliError`] for missing, duplicate, unknown, or malformed options.
pub fn parse_export_video_audio(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<ExportVideoAudioArgs, CliError> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("export-video-audio")) {
        return Err(CliError::Usage);
    }
    let mut values = ParsedVideoAudioValues::default();
    while let Some(flag) = arguments.next() {
        let flag_text = flag.to_str().ok_or(CliError::NonUtf8Value("option"))?;
        let value = arguments
            .next()
            .ok_or(CliError::MissingOption("option value"))?;
        values.set(flag_text, value)?;
    }
    values.finish()
}

#[derive(Default)]
struct ParsedValues {
    project: Option<OsString>,
    sequence: Option<OsString>,
    track: Option<OsString>,
    output: Option<OsString>,
    width: Option<OsString>,
    height: Option<OsString>,
    fps: Option<OsString>,
    ffmpeg: Option<OsString>,
    ffprobe: Option<OsString>,
}

#[derive(Default)]
struct ParsedPresetValues {
    project: Option<OsString>,
    sequence: Option<OsString>,
    track: Option<OsString>,
    preset: Option<OsString>,
    output: Option<OsString>,
    ffmpeg: Option<OsString>,
    ffprobe: Option<OsString>,
}

impl ParsedPresetValues {
    fn set(&mut self, flag: &str, value: OsString) -> Result<(), CliError> {
        let destination = match flag {
            "--project" => &mut self.project,
            "--sequence" => &mut self.sequence,
            "--track" => &mut self.track,
            "--preset" => &mut self.preset,
            "--output" => &mut self.output,
            "--ffmpeg" => &mut self.ffmpeg,
            "--ffprobe" => &mut self.ffprobe,
            _ => return Err(CliError::InvalidOption(flag.to_owned())),
        };
        if destination.replace(value).is_some() {
            return Err(CliError::InvalidOption(flag.to_owned()));
        }
        Ok(())
    }

    fn finish(self) -> Result<ExportPresetTrackArgs, CliError> {
        let preset_name = required(self.preset, "--preset")?
            .into_string()
            .map_err(|_| CliError::NonUtf8Value("--preset"))?;
        let preset = ExportPreset::parse(&preset_name).map_err(|_| CliError::InvalidValue {
            option: "--preset",
            value: preset_name,
        })?;
        Ok(ExportPresetTrackArgs {
            project: PathBuf::from(required(self.project, "--project")?),
            sequence_id: SequenceId::from_uuid(parse_uuid(
                required(self.sequence, "--sequence")?,
                "--sequence",
            )?),
            track_id: TrackId::from_uuid(parse_uuid(required(self.track, "--track")?, "--track")?),
            preset,
            output: PathBuf::from(required(self.output, "--output")?),
            ffmpeg: PathBuf::from(required(self.ffmpeg, "--ffmpeg")?),
            ffprobe: PathBuf::from(required(self.ffprobe, "--ffprobe")?),
        })
    }
}

#[derive(Default)]
struct ParsedVideoAudioValues {
    project: Option<OsString>,
    sequence: Option<OsString>,
    video_track: Option<OsString>,
    audio_track: Option<OsString>,
    output: Option<OsString>,
    width: Option<OsString>,
    height: Option<OsString>,
    fps: Option<OsString>,
    ffmpeg: Option<OsString>,
    ffprobe: Option<OsString>,
}

impl ParsedVideoAudioValues {
    fn set(&mut self, flag: &str, value: OsString) -> Result<(), CliError> {
        let destination = match flag {
            "--project" => &mut self.project,
            "--sequence" => &mut self.sequence,
            "--video-track" => &mut self.video_track,
            "--audio-track" => &mut self.audio_track,
            "--output" => &mut self.output,
            "--width" => &mut self.width,
            "--height" => &mut self.height,
            "--fps" => &mut self.fps,
            "--ffmpeg" => &mut self.ffmpeg,
            "--ffprobe" => &mut self.ffprobe,
            _ => return Err(CliError::InvalidOption(flag.to_owned())),
        };
        if destination.replace(value).is_some() {
            return Err(CliError::InvalidOption(flag.to_owned()));
        }
        Ok(())
    }

    fn finish(self) -> Result<ExportVideoAudioArgs, CliError> {
        Ok(ExportVideoAudioArgs {
            project: PathBuf::from(required(self.project, "--project")?),
            sequence_id: SequenceId::from_uuid(parse_uuid(
                required(self.sequence, "--sequence")?,
                "--sequence",
            )?),
            video_track_id: TrackId::from_uuid(parse_uuid(
                required(self.video_track, "--video-track")?,
                "--video-track",
            )?),
            audio_track_id: TrackId::from_uuid(parse_uuid(
                required(self.audio_track, "--audio-track")?,
                "--audio-track",
            )?),
            output: PathBuf::from(required(self.output, "--output")?),
            width: parse_u32(required(self.width, "--width")?, "--width")?,
            height: parse_u32(required(self.height, "--height")?, "--height")?,
            frame_rate: parse_rate(required(self.fps, "--fps")?)?,
            ffmpeg: PathBuf::from(required(self.ffmpeg, "--ffmpeg")?),
            ffprobe: PathBuf::from(required(self.ffprobe, "--ffprobe")?),
        })
    }
}

impl ParsedValues {
    fn set(&mut self, flag: &str, value: OsString) -> Result<(), CliError> {
        let destination = match flag {
            "--project" => &mut self.project,
            "--sequence" => &mut self.sequence,
            "--track" => &mut self.track,
            "--output" => &mut self.output,
            "--width" => &mut self.width,
            "--height" => &mut self.height,
            "--fps" => &mut self.fps,
            "--ffmpeg" => &mut self.ffmpeg,
            "--ffprobe" => &mut self.ffprobe,
            _ => return Err(CliError::InvalidOption(flag.to_owned())),
        };
        if destination.replace(value).is_some() {
            return Err(CliError::InvalidOption(flag.to_owned()));
        }
        Ok(())
    }

    fn finish(self) -> Result<ExportTrackArgs, CliError> {
        Ok(ExportTrackArgs {
            project: PathBuf::from(required(self.project, "--project")?),
            sequence_id: SequenceId::from_uuid(parse_uuid(
                required(self.sequence, "--sequence")?,
                "--sequence",
            )?),
            track_id: TrackId::from_uuid(parse_uuid(required(self.track, "--track")?, "--track")?),
            output: PathBuf::from(required(self.output, "--output")?),
            width: parse_u32(required(self.width, "--width")?, "--width")?,
            height: parse_u32(required(self.height, "--height")?, "--height")?,
            frame_rate: parse_rate(required(self.fps, "--fps")?)?,
            ffmpeg: PathBuf::from(required(self.ffmpeg, "--ffmpeg")?),
            ffprobe: PathBuf::from(required(self.ffprobe, "--ffprobe")?),
        })
    }
}

fn required(value: Option<OsString>, option: &'static str) -> Result<OsString, CliError> {
    value.ok_or(CliError::MissingOption(option))
}

fn parse_uuid(value: OsString, option: &'static str) -> Result<Uuid, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value(option))?;
    Uuid::parse_str(&value).map_err(|_| CliError::InvalidValue { option, value })
}

fn parse_u32(value: OsString, option: &'static str) -> Result<u32, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value(option))?;
    value
        .parse()
        .map_err(|_| CliError::InvalidValue { option, value })
}

fn parse_usize(value: &OsString, option: &'static str) -> Result<usize, CliError> {
    let text = value.to_str().ok_or(CliError::NonUtf8Value(option))?;
    text.parse::<usize>().map_err(|_| CliError::InvalidValue {
        option,
        value: text.to_owned(),
    })
}

fn parse_bool(value: &OsString, option: &'static str) -> Result<bool, CliError> {
    let text = value.to_str().ok_or(CliError::NonUtf8Value(option))?;
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(CliError::InvalidValue {
            option,
            value: text.to_owned(),
        }),
    }
}

fn parse_rate(value: OsString) -> Result<RationalTime, CliError> {
    parse_exact_time(value, "--fps")
}

fn parse_exact_time(value: OsString, option: &'static str) -> Result<RationalTime, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value(option))?;
    let Some((numerator, denominator)) = value.split_once('/') else {
        return Err(CliError::InvalidValue { option, value });
    };
    let numerator = numerator
        .parse::<i64>()
        .map_err(|_| CliError::InvalidValue {
            option,
            value: value.clone(),
        })?;
    let denominator = denominator
        .parse::<u32>()
        .map_err(|_| CliError::InvalidValue {
            option,
            value: value.clone(),
        })?;
    RationalTime::new(numerator, denominator).map_err(CliError::from)
}

#[cfg(test)]
mod tests {
    use super::{
        CliError, CliOutcome, ProjectInspection, parse_export_preset_track, parse_export_track,
        parse_export_video_audio, run,
    };
    use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Clip, Marker, MediaAsset, Project, Sequence, Track, TrackKind};
    use prism_timeline::RationalTime;
    use std::ffi::OsString;
    use uuid::Uuid;

    fn valid_arguments() -> Vec<OsString> {
        [
            "export-track",
            "--project",
            "edit.prism",
            "--sequence",
            "11111111-1111-4111-8111-111111111111",
            "--track",
            "22222222-2222-4222-8222-222222222222",
            "--output",
            "finish.mp4",
            "--width",
            "1920",
            "--height",
            "1080",
            "--fps",
            "30000/1001",
            "--ffmpeg",
            "/tools/ffmpeg",
            "--ffprobe",
            "/tools/ffprobe",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    fn valid_video_audio_arguments() -> Vec<OsString> {
        [
            "export-video-audio",
            "--project",
            "edit.prism",
            "--sequence",
            "11111111-1111-4111-8111-111111111111",
            "--video-track",
            "22222222-2222-4222-8222-222222222222",
            "--audio-track",
            "33333333-3333-4333-8333-333333333333",
            "--output",
            "finish.mp4",
            "--width",
            "1920",
            "--height",
            "1080",
            "--fps",
            "30000/1001",
            "--ffmpeg",
            "/tools/ffmpeg",
            "--ffprobe",
            "/tools/ffprobe",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn parses_typed_export_track_command() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_export_track(valid_arguments())?;
        assert_eq!(parsed.width, 1920);
        assert_eq!(parsed.height, 1080);
        assert_eq!(parsed.frame_rate, RationalTime::new(30_000, 1_001)?);
        assert_eq!(
            parsed.sequence_id.as_uuid().to_string(),
            "11111111-1111-4111-8111-111111111111"
        );
        Ok(())
    }

    #[test]
    fn parses_preset_export_command() -> Result<(), Box<dyn std::error::Error>> {
        let arguments = [
            "export-preset-track",
            "--project",
            "edit.prism",
            "--sequence",
            "11111111-1111-4111-8111-111111111111",
            "--track",
            "22222222-2222-4222-8222-222222222222",
            "--preset",
            "youtube-1080p",
            "--output",
            "finish.mp4",
            "--ffmpeg",
            "/tools/ffmpeg",
            "--ffprobe",
            "/tools/ffprobe",
        ]
        .into_iter()
        .map(OsString::from);
        let parsed = parse_export_preset_track(arguments)?;
        assert_eq!(parsed.preset, prism_export::ExportPreset::Youtube1080p);
        assert_eq!(parsed.output, std::path::PathBuf::from("finish.mp4"));
        Ok(())
    }

    #[test]
    fn parses_typed_video_audio_export_command() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_export_video_audio(valid_video_audio_arguments())?;
        assert_eq!(parsed.width, 1920);
        assert_eq!(parsed.height, 1080);
        assert_eq!(parsed.frame_rate, RationalTime::new(30_000, 1_001)?);
        assert_eq!(
            parsed.audio_track_id.as_uuid().to_string(),
            "33333333-3333-4333-8333-333333333333"
        );
        Ok(())
    }

    #[test]
    fn rejects_duplicate_and_missing_options() {
        let mut duplicate = valid_arguments();
        duplicate.extend([OsString::from("--width"), OsString::from("640")]);
        assert!(matches!(
            parse_export_track(duplicate),
            Err(CliError::InvalidOption(option)) if option == "--width"
        ));
        assert!(matches!(
            parse_export_track([OsString::from("export-track")]),
            Err(CliError::MissingOption("--project"))
        ));
    }

    #[test]
    fn rejects_malformed_frame_rate() {
        let mut arguments = valid_arguments();
        let fps = arguments
            .iter_mut()
            .skip_while(|argument| argument.as_os_str() != "--fps")
            .nth(1);
        if let Some(fps) = fps {
            *fps = OsString::from("29.97");
        }
        assert!(matches!(
            parse_export_track(arguments),
            Err(CliError::InvalidValue {
                option: "--fps",
                ..
            })
        ));
    }

    #[test]
    fn lists_autosave_candidates_as_newest_first_json() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("prism-cli-autosaves-{}", Uuid::new_v4()));
        std::fs::create_dir(&directory)?;
        let project = directory.join("edit.prism");
        std::fs::write(directory.join("edit.prism.autosave.2"), b"candidate")?;
        std::fs::write(directory.join("edit.prism.autosave.5"), b"candidate")?;
        let outcome = run([
            OsString::from("list-autosaves"),
            OsString::from("--project"),
            project.as_os_str().to_owned(),
        ])?;
        let CliOutcome::AutosavesListed { json } = outcome else {
            return Err("unexpected CLI outcome".into());
        };
        let values: Vec<serde_json::Value> = serde_json::from_str(&json)?;
        assert_eq!(values[0]["revision"], 5);
        assert_eq!(values[1]["revision"], 2);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn inspection_json_preserves_ids_order_and_exact_times()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Inspection");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let clip_id = ClipId::new();
        let marker_id = MarkerId::new();
        project.media.insert(
            media_id,
            MediaAsset {
                id: media_id,
                source_path: "source.mp4".to_owned(),
            },
        );
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: vec![Marker {
                    id: marker_id,
                    time: RationalTime::new(2, 1)?,
                    label: "Beat".to_owned(),
                }],
                transitions: Vec::new(),
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Video,
                    locked: false,
                    visible: true,
                    muted: false,
                    clips: vec![Clip {
                        id: clip_id,
                        media_id,
                        timeline_start: RationalTime::new(1_001, 30_000)?,
                        source_start: RationalTime::new(1, 2)?,
                        source_duration: RationalTime::new(3, 4)?,
                    }],
                }],
            },
        );
        project.validate()?;
        let json = serde_json::to_value(ProjectInspection::from(&project))?;
        assert_eq!(json["formatVersion"], 1);
        assert_eq!(json["projectId"], project.id.as_uuid().to_string());
        assert_eq!(json["media"][0]["id"], media_id.as_uuid().to_string());
        assert_eq!(
            json["sequences"][0]["id"],
            sequence_id.as_uuid().to_string()
        );
        assert_eq!(json["sequences"][0]["tracks"][0]["kind"], "video");
        assert_eq!(
            json["sequences"][0]["markers"][0]["id"],
            marker_id.as_uuid().to_string()
        );
        assert_eq!(json["sequences"][0]["markers"][0]["label"], "Beat");
        let time = &json["sequences"][0]["tracks"][0]["clips"][0]["timelineStart"];
        assert_eq!(time["value"], 1_001);
        assert_eq!(time["timescale"], 30_000);
        Ok(())
    }
}
