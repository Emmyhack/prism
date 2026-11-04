//! Dependency-light command-line workflows backed by Prism's core APIs.

use prism_core::{SequenceId, TrackId};
use prism_export::{BasicMp4Exporter, ExportError, ExportReport, SingleTrackMp4ExportRequest};
use prism_persistence::save_atomic;
use prism_persistence::{PersistenceError, load};
use prism_project::{Project, TrackKind};
use prism_timeline::{RationalTime, TimeError};
use serde::Serialize;
use std::{ffi::OsString, path::PathBuf};
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

/// Successful CLI operation.
#[derive(Debug)]
pub enum CliOutcome {
    /// A finalized media export.
    Exported(ExportReport),
    /// Versioned project inspection JSON.
    Inspected(String),
    /// A newly created, atomically saved project.
    ProjectCreated {
        /// Saved project path.
        path: PathBuf,
        /// Stable project UUID.
        project_id: String,
    },
}

/// CLI parse and workflow failures.
#[derive(Debug, Error)]
pub enum CliError {
    /// No supported subcommand was supplied.
    #[error(
        "usage: prism-cli create-project --project PATH --name NAME | prism-cli inspect --project PATH | prism-cli export-track --project PATH --sequence UUID --track UUID --output PATH --width PIXELS --height PIXELS --fps NUM/DEN --ffmpeg PATH --ffprobe PATH"
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
    /// Project creation never overwrites an existing path.
    #[error("project already exists: {0}")]
    ProjectExists(PathBuf),
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
        Some("inspect") => run_inspect(&arguments),
        Some("create-project") => run_create_project(&arguments),
        Some(_) | None => Err(CliError::Usage),
    }
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
    tracks: Vec<TrackInspection>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrackInspection {
    id: String,
    kind: &'static str,
    locked: bool,
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

fn parse_rate(value: OsString) -> Result<RationalTime, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value("--fps"))?;
    let Some((numerator, denominator)) = value.split_once('/') else {
        return Err(CliError::InvalidValue {
            option: "--fps",
            value,
        });
    };
    let numerator = numerator
        .parse::<i64>()
        .map_err(|_| CliError::InvalidValue {
            option: "--fps",
            value: value.clone(),
        })?;
    let denominator = denominator
        .parse::<u32>()
        .map_err(|_| CliError::InvalidValue {
            option: "--fps",
            value: value.clone(),
        })?;
    RationalTime::new(numerator, denominator).map_err(CliError::from)
}

#[cfg(test)]
mod tests {
    use super::{CliError, ProjectInspection, parse_export_track};
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind};
    use prism_timeline::RationalTime;
    use std::ffi::OsString;

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
    fn inspection_json_preserves_ids_order_and_exact_times()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Inspection");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let clip_id = ClipId::new();
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
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Video,
                    locked: false,
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
        let time = &json["sequences"][0]["tracks"][0]["clips"][0]["timelineStart"];
        assert_eq!(time["value"], 1_001);
        assert_eq!(time["timescale"], 30_000);
        Ok(())
    }
}
