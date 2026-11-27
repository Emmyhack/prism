use super::{
    BasicMp4Exporter, CliError, CliOutcome, DEFAULT_VIDEO_BITRATE_KBPS, ExportPreset,
    ExportPresetTrackArgs, ExportTrackArgs, ExportVideoAudioArgs, OsString, PathBuf, Project,
    RationalTime, SequenceId, Serialize, SingleTrackMp4ExportRequest, TrackId, TrackKind, Uuid,
    VideoAudioMp4ExportRequest, load,
};

pub(super) fn run_export_track(arguments: Vec<OsString>) -> Result<CliOutcome, CliError> {
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
                video_bitrate_kbps: parsed.video_bitrate_kbps,
                output: parsed.output,
            },
        )?,
    ))
}

pub(super) fn run_export_preset_track(arguments: Vec<OsString>) -> Result<CliOutcome, CliError> {
    let parsed = parse_export_preset_track(arguments)?;
    let project = load(&parsed.project)?;
    let mut request = SingleTrackMp4ExportRequest::from_preset(parsed.preset, parsed.output);
    if let Some(video_bitrate_kbps) = parsed.video_bitrate_kbps {
        request.video_bitrate_kbps = video_bitrate_kbps;
    }
    Ok(CliOutcome::Exported(
        BasicMp4Exporter::new(&parsed.ffmpeg, &parsed.ffprobe).export_single_video_track(
            &project,
            parsed.sequence_id,
            parsed.track_id,
            &request,
        )?,
    ))
}

pub(super) fn run_export_video_audio(arguments: Vec<OsString>) -> Result<CliOutcome, CliError> {
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
                video_bitrate_kbps: parsed.video_bitrate_kbps,
                audio_bitrate_kbps: parsed.audio_bitrate_kbps,
                output: parsed.output,
            },
        )?,
    ))
}

pub(super) fn run_inspect(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
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
pub(super) struct ProjectInspection {
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
    video_bitrate_kbps: Option<OsString>,
    ffmpeg: Option<OsString>,
    ffprobe: Option<OsString>,
}

#[derive(Default)]
struct ParsedPresetValues {
    project: Option<OsString>,
    sequence: Option<OsString>,
    track: Option<OsString>,
    preset: Option<OsString>,
    video_bitrate_kbps: Option<OsString>,
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
            "--video-bitrate-kbps" => &mut self.video_bitrate_kbps,
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
            video_bitrate_kbps: self
                .video_bitrate_kbps
                .map(|value| parse_positive_u32(value, "--video-bitrate-kbps"))
                .transpose()?,
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
    video_bitrate_kbps: Option<OsString>,
    audio_bitrate_kbps: Option<OsString>,
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
            "--video-bitrate-kbps" => &mut self.video_bitrate_kbps,
            "--audio-bitrate-kbps" => &mut self.audio_bitrate_kbps,
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
            video_bitrate_kbps: parse_positive_u32(
                required(self.video_bitrate_kbps, "--video-bitrate-kbps")?,
                "--video-bitrate-kbps",
            )?,
            audio_bitrate_kbps: parse_positive_u32(
                required(self.audio_bitrate_kbps, "--audio-bitrate-kbps")?,
                "--audio-bitrate-kbps",
            )?,
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
            "--video-bitrate-kbps" => &mut self.video_bitrate_kbps,
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
            video_bitrate_kbps: self
                .video_bitrate_kbps
                .map(|value| parse_positive_u32(value, "--video-bitrate-kbps"))
                .transpose()?
                .unwrap_or(DEFAULT_VIDEO_BITRATE_KBPS),
            ffmpeg: PathBuf::from(required(self.ffmpeg, "--ffmpeg")?),
            ffprobe: PathBuf::from(required(self.ffprobe, "--ffprobe")?),
        })
    }
}

pub(super) fn required(
    value: Option<OsString>,
    option: &'static str,
) -> Result<OsString, CliError> {
    value.ok_or(CliError::MissingOption(option))
}

pub(super) fn parse_uuid(value: OsString, option: &'static str) -> Result<Uuid, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value(option))?;
    Uuid::parse_str(&value).map_err(|_| CliError::InvalidValue { option, value })
}

pub(super) fn parse_u32(value: OsString, option: &'static str) -> Result<u32, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value(option))?;
    value
        .parse()
        .map_err(|_| CliError::InvalidValue { option, value })
}

pub(super) fn parse_positive_u32(value: OsString, option: &'static str) -> Result<u32, CliError> {
    let parsed = parse_u32(value, option)?;
    if parsed == 0 {
        return Err(CliError::InvalidValue {
            option,
            value: "0".to_owned(),
        });
    }
    Ok(parsed)
}

pub(super) fn parse_usize(value: &OsString, option: &'static str) -> Result<usize, CliError> {
    let text = value.to_str().ok_or(CliError::NonUtf8Value(option))?;
    text.parse::<usize>().map_err(|_| CliError::InvalidValue {
        option,
        value: text.to_owned(),
    })
}

pub(super) fn parse_bool(value: &OsString, option: &'static str) -> Result<bool, CliError> {
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

pub(super) fn parse_rate(value: OsString) -> Result<RationalTime, CliError> {
    parse_exact_time(value, "--fps")
}

pub(super) fn parse_exact_time(
    value: OsString,
    option: &'static str,
) -> Result<RationalTime, CliError> {
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
