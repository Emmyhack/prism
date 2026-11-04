//! Validated export adapters with temporary-file finalization.
//!
//! The initial adapter establishes a real single-source MP4 workflow using an
//! explicitly configured `FFmpeg` executable. Multi-clip timeline rendering,
//! effects, and production codec distribution remain separate milestones.

use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_media::{FfprobeInspector, MediaError, StreamKind};
use prism_project::{Project, TrackKind};
use prism_timeline::{RationalTime, RoundingMode, TimeError};
use std::{
    ffi::OsString,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use thiserror::Error;
use uuid::Uuid;

/// One exact source range exported to a new MP4 file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BasicMp4ExportRequest {
    /// Existing input media path.
    pub source: PathBuf,
    /// Nonnegative source timestamp.
    pub source_start: RationalTime,
    /// Strictly positive source duration.
    pub duration: RationalTime,
    /// New final output path. Existing files are never replaced.
    pub output: PathBuf,
}

/// Cooperative cancellation state shared with an in-flight export.
#[derive(Clone, Debug, Default)]
pub struct ExportCancellation {
    cancelled: Arc<AtomicBool>,
}

impl ExportCancellation {
    /// Creates a non-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation of an in-flight encoder.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

/// Output settings for rendering one planned video track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingleTrackMp4ExportRequest {
    /// Final encoded width in pixels.
    pub width: u32,
    /// Final encoded height in pixels.
    pub height: u32,
    /// Exact positive output frame rate.
    pub frame_rate: RationalTime,
    /// Target video bitrate in kilobits per second.
    pub video_bitrate_kbps: u32,
    /// New final output path. Existing files are never replaced.
    pub output: PathBuf,
}

/// Output settings for a synchronized video-plus-audio export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoAudioMp4ExportRequest {
    /// Final encoded width in pixels.
    pub width: u32,
    /// Final encoded height in pixels.
    pub height: u32,
    /// Exact positive output frame rate.
    pub frame_rate: RationalTime,
    /// Target video bitrate in kilobits per second.
    pub video_bitrate_kbps: u32,
    /// Target AAC audio bitrate in kilobits per second.
    pub audio_bitrate_kbps: u32,
    /// New final output path. Existing files are never replaced.
    pub output: PathBuf,
}

/// Validated initial export presets with explicit geometry, frame rate, and
/// video bitrate policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportPreset {
    /// 1920×1080 at 30000/1001 fps.
    Youtube1080p,
    /// 3840×2160 at 30000/1001 fps.
    Youtube4k,
}

impl ExportPreset {
    /// Parses a stable user-facing preset identifier.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError::UnknownPreset`] for unsupported identifiers.
    pub fn parse(name: &str) -> Result<Self, ExportError> {
        match name {
            "youtube-1080p" => Ok(Self::Youtube1080p),
            "youtube-4k" => Ok(Self::Youtube4k),
            _ => Err(ExportError::UnknownPreset(name.to_owned())),
        }
    }

    /// Returns exact dimensions, frame rate, and target bitrate for this preset.
    #[must_use]
    pub fn settings(self) -> (u32, u32, RationalTime, u32) {
        let frame_rate = RationalTime::new(30_000, 1_001)
            .unwrap_or_else(|_| unreachable!("built-in preset frame rate is valid"));
        match self {
            Self::Youtube1080p => (1920, 1080, frame_rate, 8_000),
            Self::Youtube4k => (3840, 2160, frame_rate, 35_000),
        }
    }

    /// Returns the target AAC audio bitrate for this preset.
    #[must_use]
    pub const fn audio_bitrate_kbps(self) -> u32 {
        192
    }
}

impl SingleTrackMp4ExportRequest {
    /// Creates a request from a validated preset and output path.
    #[must_use]
    pub fn from_preset(preset: ExportPreset, output: PathBuf) -> Self {
        let (width, height, frame_rate, video_bitrate_kbps) = preset.settings();
        Self {
            width,
            height,
            frame_rate,
            video_bitrate_kbps,
            output,
        }
    }
}

impl VideoAudioMp4ExportRequest {
    /// Creates a synchronized video/audio request from a preset and output.
    #[must_use]
    pub fn from_preset(preset: ExportPreset, output: PathBuf) -> Self {
        let (width, height, frame_rate, video_bitrate_kbps) = preset.settings();
        Self {
            width,
            height,
            frame_rate,
            video_bitrate_kbps,
            audio_bitrate_kbps: preset.audio_bitrate_kbps(),
            output,
        }
    }
}

/// Facts independently observed before finalizing an export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportReport {
    /// Finalized output path.
    pub output: PathBuf,
    /// Finalized file length.
    pub byte_length: u64,
    /// Probed output duration.
    pub duration: RationalTime,
    /// Whether the finalized output contains a video stream.
    pub has_video: bool,
    /// Whether the finalized output contains an audio stream.
    pub has_audio: bool,
}

/// One exact segment in a single-video-track export plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SequenceExportSegment {
    /// Transparent/black sequence time without a clip on the selected track.
    Gap {
        /// Exact start in sequence time.
        timeline_start: RationalTime,
        /// Exact positive gap duration.
        duration: RationalTime,
    },
    /// One nondestructive source range placed on the selected track.
    Clip {
        /// Authoritative clip identifier.
        clip_id: ClipId,
        /// Referenced source asset.
        media_id: MediaAssetId,
        /// Exact start in sequence time.
        timeline_start: RationalTime,
        /// Exact start in source media time.
        source_start: RationalTime,
        /// Exact source/sequence duration before retiming exists.
        duration: RationalTime,
    },
}

/// Deterministic export interpretation of one video track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingleTrackExportPlan {
    /// Source sequence.
    pub sequence_id: SequenceId,
    /// Selected video track.
    pub track_id: TrackId,
    /// Ordered, contiguous segments from zero through the final clip end.
    pub segments: Vec<SequenceExportSegment>,
    /// Exact plan duration, or zero for an empty track.
    pub duration: RationalTime,
}

/// Deterministic export interpretation of one audio track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SingleAudioTrackExportPlan {
    /// Source sequence.
    pub sequence_id: SequenceId,
    /// Selected audio track.
    pub track_id: TrackId,
    /// Ordered, contiguous segments from zero through the final clip end.
    pub segments: Vec<SequenceExportSegment>,
    /// Exact plan duration, or zero for an empty track.
    pub duration: RationalTime,
}

/// Basic export failures.
#[derive(Debug, Error)]
pub enum ExportError {
    /// User supplied an unsupported export preset identifier.
    #[error("unknown export preset: {0}")]
    UnknownPreset(String),
    /// Requested sequence was absent from the project.
    #[error("export sequence does not exist")]
    SequenceNotFound,
    /// Requested track was absent from the selected sequence.
    #[error("export track does not exist in the sequence")]
    TrackNotFound,
    /// The initial sequence exporter only accepts video tracks.
    #[error("export track is not a video track")]
    TrackIsNotVideo,
    /// The audio planning path was given a video track.
    #[error("export track is not an audio track")]
    TrackIsNotAudio,
    /// Hidden tracks cannot be selected as visible export output.
    #[error("export track is hidden")]
    TrackHidden,
    /// Muted audio tracks cannot be selected as audible export output.
    #[error("export audio track is muted")]
    TrackMuted,
    /// Track clips were overlapping, out of order, or otherwise invalid.
    #[error("export track layout is invalid")]
    InvalidTrackLayout,
    /// A planned clip references media absent from the project.
    #[error("planned export clip references missing media")]
    MissingMedia,
    /// The selected track has no clips and therefore no duration.
    #[error("selected export track is empty")]
    EmptyPlan,
    /// Explicit sequence output dimensions or frame rate were invalid.
    #[error("sequence export video settings are invalid")]
    InvalidVideoSettings,
    /// Input source does not exist as a regular file.
    #[error("export source is not a regular file: {0}")]
    InvalidSource(PathBuf),
    /// Output already exists and will not be overwritten.
    #[error("export output already exists: {0}")]
    OutputExists(PathBuf),
    /// Output has no existing parent directory.
    #[error("export output parent is unavailable: {0}")]
    InvalidOutputParent(PathBuf),
    /// Source start was before zero or duration was not positive.
    #[error("export range must have a nonnegative start and positive duration")]
    InvalidRange,
    /// Exact timestamp conversion failed.
    #[error("export time conversion failed: {0}")]
    Time(#[from] TimeError),
    /// Video and audio plans do not cover the same exact duration.
    #[error("video and audio export plans have different durations")]
    MismatchedPlanDuration,
    /// The encoder process could not start or file finalization failed.
    #[error("export I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The encoder process reported failure.
    #[error("encoder failed with status {status}: {stderr}")]
    Encoder {
        /// Process exit status when available.
        status: i32,
        /// Diagnostic output with lossy UTF-8 conversion.
        stderr: String,
    },
    /// The caller cancelled the encoder before finalization.
    #[error("export cancelled")]
    Cancelled,
    /// Independent output inspection failed.
    #[error("export validation probe failed: {0}")]
    Probe(#[from] MediaError),
    /// The encoder output was empty or lacked required video/duration evidence.
    #[error("encoded output failed validation: {0}")]
    InvalidOutput(&'static str),
}

/// Builds contiguous export timing for one authoritative video track.
///
/// Gaps before or between clips are explicit plan segments. No trailing gap is
/// invented because the current sequence model has no independent duration.
///
/// # Errors
///
/// Returns [`ExportError`] for missing/wrong-kind objects or exact-time
/// arithmetic overflow.
pub fn plan_single_video_track(
    project: &Project,
    sequence_id: SequenceId,
    track_id: TrackId,
) -> Result<SingleTrackExportPlan, ExportError> {
    let sequence = project
        .sequences
        .get(&sequence_id)
        .ok_or(ExportError::SequenceNotFound)?;
    let track = sequence
        .tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or(ExportError::TrackNotFound)?;
    if track.kind != TrackKind::Video {
        return Err(ExportError::TrackIsNotVideo);
    }
    if !track.visible {
        return Err(ExportError::TrackHidden);
    }
    let mut cursor = RationalTime::ZERO;
    let mut segments = Vec::with_capacity(track.clips.len().saturating_mul(2));
    for clip in &track.clips {
        if clip.timeline_start < cursor || !clip.source_duration.is_positive() {
            return Err(ExportError::InvalidTrackLayout);
        }
        if clip.timeline_start > cursor {
            segments.push(SequenceExportSegment::Gap {
                timeline_start: cursor,
                duration: clip.timeline_start.checked_sub(cursor)?,
            });
        }
        segments.push(SequenceExportSegment::Clip {
            clip_id: clip.id,
            media_id: clip.media_id,
            timeline_start: clip.timeline_start,
            source_start: clip.source_start,
            duration: clip.source_duration,
        });
        cursor = clip.timeline_start.checked_add(clip.source_duration)?;
    }
    Ok(SingleTrackExportPlan {
        sequence_id,
        track_id,
        segments,
        duration: cursor,
    })
}

/// Builds contiguous export timing for one authoritative audio track.
///
/// Audio gaps are explicit silence segments. The plan does not mix samples or
/// launch an encoder; it is the validated timing contract consumed by the
/// future audio render/mix stage.
///
/// # Errors
///
/// Returns [`ExportError`] for missing/wrong-kind/muted objects or exact-time
/// arithmetic overflow.
pub fn plan_single_audio_track(
    project: &Project,
    sequence_id: SequenceId,
    track_id: TrackId,
) -> Result<SingleAudioTrackExportPlan, ExportError> {
    let sequence = project
        .sequences
        .get(&sequence_id)
        .ok_or(ExportError::SequenceNotFound)?;
    let track = sequence
        .tracks
        .iter()
        .find(|track| track.id == track_id)
        .ok_or(ExportError::TrackNotFound)?;
    if track.kind != TrackKind::Audio {
        return Err(ExportError::TrackIsNotAudio);
    }
    if track.muted {
        return Err(ExportError::TrackMuted);
    }
    let mut cursor = RationalTime::ZERO;
    let mut segments = Vec::with_capacity(track.clips.len().saturating_mul(2));
    for clip in &track.clips {
        if clip.timeline_start < cursor || !clip.source_duration.is_positive() {
            return Err(ExportError::InvalidTrackLayout);
        }
        if clip.timeline_start > cursor {
            segments.push(SequenceExportSegment::Gap {
                timeline_start: cursor,
                duration: clip.timeline_start.checked_sub(cursor)?,
            });
        }
        segments.push(SequenceExportSegment::Clip {
            clip_id: clip.id,
            media_id: clip.media_id,
            timeline_start: clip.timeline_start,
            source_start: clip.source_start,
            duration: clip.source_duration,
        });
        cursor = clip.timeline_start.checked_add(clip.source_duration)?;
    }
    Ok(SingleAudioTrackExportPlan {
        sequence_id,
        track_id,
        segments,
        duration: cursor,
    })
}

/// Builds the deterministic `FFmpeg` filter fragment for one audio plan.
///
/// The caller must provide one audio input for each plan segment in the same
/// order (silence inputs for [`SequenceExportSegment::Gap`] and trimmed media
/// inputs for clips). `input_offset` is the first `FFmpeg` input index reserved
/// for these segments. The returned `[outa]` label is stable for composition
/// with the video graph.
#[must_use]
pub fn build_audio_concat_filter(plan: &SingleAudioTrackExportPlan, input_offset: usize) -> String {
    if plan.segments.is_empty() {
        return "anullsrc=r=48000:cl=stereo,atrim=duration=0[outa]".to_owned();
    }
    let mut filter = String::new();
    for (segment_index, _) in plan.segments.iter().enumerate() {
        let input_index = input_offset + segment_index;
        let _ = write!(
            filter,
            "[{input_index}:a:0]aresample=async=1:first_pts=0,asetpts=PTS-STARTPTS[a{segment_index}];"
        );
    }
    for segment_index in 0..plan.segments.len() {
        let _ = write!(filter, "[a{segment_index}]");
    }
    let _ = write!(filter, "concat=n={}:v=0:a=1[outa]", plan.segments.len());
    filter
}

/// Explicit `FFmpeg`/ffprobe adapter for the initial validated MP4 workflow.
#[derive(Clone, Debug)]
pub struct BasicMp4Exporter {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl BasicMp4Exporter {
    /// Creates an exporter using explicit executable paths.
    #[must_use]
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        }
    }

    /// Encodes, independently validates, and finalizes a basic MP4 export.
    ///
    /// The temporary file is created beside the destination so no-clobber link
    /// finalization remains on the same filesystem. Existing destinations are
    /// refused, including if one appears during encoding.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] for invalid paths/ranges, encoder failure,
    /// validation failure, or atomic finalization failure.
    pub fn export(&self, request: &BasicMp4ExportRequest) -> Result<ExportReport, ExportError> {
        self.export_with_cancellation(request, None)
    }

    /// Encodes an export while polling a cooperative cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError::Cancelled`] when cancellation is requested, or
    /// the same errors as [`Self::export`] for all other failures.
    pub fn export_with_cancellation(
        &self,
        request: &BasicMp4ExportRequest,
        cancellation: Option<&ExportCancellation>,
    ) -> Result<ExportReport, ExportError> {
        validate_request(request)?;
        if cancellation.is_some_and(ExportCancellation::is_cancelled) {
            return Err(ExportError::Cancelled);
        }
        let temporary = temporary_output_path(&request.output)?;
        let guard = TemporaryExport::new(temporary);
        let start = format_timestamp(request.source_start)?;
        let duration = format_timestamp(request.duration)?;
        let mut command = Command::new(&self.ffmpeg);
        command
            .args(["-v", "error", "-ss", &start, "-t", &duration, "-i"])
            .arg(&request.source)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-c:v",
                "mpeg4",
                "-q:v",
                "4",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
                "-f",
                "mp4",
                "-y",
            ])
            .arg(guard.path());
        let output = run_encoder(command, cancellation)?;
        if !output.status.success() {
            return Err(ExportError::Encoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let metadata = FfprobeInspector::new(&self.ffprobe).inspect(guard.path())?;
        let byte_length = fs::metadata(guard.path())?.len();
        if byte_length == 0 {
            return Err(ExportError::InvalidOutput("file is empty"));
        }
        let duration = metadata
            .duration
            .ok_or(ExportError::InvalidOutput("duration is missing"))?;
        if !duration.is_positive() {
            return Err(ExportError::InvalidOutput("duration is not positive"));
        }
        let has_video = metadata
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Video(_)));
        if !has_video {
            return Err(ExportError::InvalidOutput("video stream is missing"));
        }
        let has_audio = metadata
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Audio(_)));
        guard.finalize(&request.output)?;
        Ok(ExportReport {
            output: request.output.clone(),
            byte_length,
            duration,
            has_video,
            has_audio,
        })
    }

    /// Renders one planned video track, including deterministic black gaps.
    ///
    /// All clips are scaled to the explicitly requested output dimensions and
    /// concatenated in plan order. This initial path emits video only.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] for invalid project/layout/settings, missing
    /// media, encoder/probe failure, or finalization failure.
    pub fn export_single_video_track(
        &self,
        project: &Project,
        sequence_id: SequenceId,
        track_id: TrackId,
        request: &SingleTrackMp4ExportRequest,
    ) -> Result<ExportReport, ExportError> {
        self.export_single_video_track_with_cancellation(
            project,
            sequence_id,
            track_id,
            request,
            None,
        )
    }

    /// Renders a video track while polling a cooperative cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError::Cancelled`] before encoder launch or while the
    /// encoder is running when cancellation is requested.
    pub fn export_single_video_track_with_cancellation(
        &self,
        project: &Project,
        sequence_id: SequenceId,
        track_id: TrackId,
        request: &SingleTrackMp4ExportRequest,
        cancellation: Option<&ExportCancellation>,
    ) -> Result<ExportReport, ExportError> {
        let plan = plan_single_video_track(project, sequence_id, track_id)?;
        if plan.segments.is_empty() {
            return Err(ExportError::EmptyPlan);
        }
        if request.width == 0
            || request.height == 0
            || !request.frame_rate.is_positive()
            || request.video_bitrate_kbps == 0
            || u64::from(request.width)
                .checked_mul(u64::from(request.height))
                .is_none_or(|pixels| pixels > 134_217_728)
        {
            return Err(ExportError::InvalidVideoSettings);
        }
        validate_output_path(&request.output)?;
        if cancellation.is_some_and(ExportCancellation::is_cancelled) {
            return Err(ExportError::Cancelled);
        }
        let temporary = temporary_output_path(&request.output)?;
        let guard = TemporaryExport::new(temporary);
        let mut arguments = build_sequence_ffmpeg_arguments(project, &plan, request)?;
        arguments.extend([
            OsString::from("-r"),
            OsString::from(format!(
                "{}/{}",
                request.frame_rate.value(),
                request.frame_rate.timescale()
            )),
            OsString::from("-c:v"),
            OsString::from("mpeg4"),
            OsString::from("-b:v"),
            OsString::from(format!("{}k", request.video_bitrate_kbps)),
            OsString::from("-movflags"),
            OsString::from("+faststart"),
            OsString::from("-f"),
            OsString::from("mp4"),
            OsString::from("-y"),
            guard.path().as_os_str().to_owned(),
        ]);
        let mut command = Command::new(&self.ffmpeg);
        command.args(arguments);
        let output = run_encoder(command, cancellation)?;
        if !output.status.success() {
            return Err(ExportError::Encoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let report = validate_temporary_output(&self.ffprobe, guard.path(), &request.output)?;
        guard.finalize(&request.output)?;
        Ok(report)
    }

    /// Renders one visible video track and one unmuted audio track into MP4.
    ///
    /// Both plans must cover the same exact duration. Audio gaps become
    /// explicit silence inputs and source ranges remain nondestructive.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] for invalid plans/settings, encoder/probe
    /// failure, cancellation, or finalization failure.
    pub fn export_single_video_audio_track(
        &self,
        project: &Project,
        sequence_id: SequenceId,
        video_track_id: TrackId,
        audio_track_id: TrackId,
        request: &VideoAudioMp4ExportRequest,
    ) -> Result<ExportReport, ExportError> {
        self.export_single_video_audio_track_with_cancellation(
            project,
            sequence_id,
            video_track_id,
            audio_track_id,
            request,
            None,
        )
    }

    /// Combined video/audio export with cooperative cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] for invalid plans/settings, encoder/probe
    /// failure, cancellation, or finalization failure.
    pub fn export_single_video_audio_track_with_cancellation(
        &self,
        project: &Project,
        sequence_id: SequenceId,
        video_track_id: TrackId,
        audio_track_id: TrackId,
        request: &VideoAudioMp4ExportRequest,
        cancellation: Option<&ExportCancellation>,
    ) -> Result<ExportReport, ExportError> {
        let video_plan = plan_single_video_track(project, sequence_id, video_track_id)?;
        let audio_plan = plan_single_audio_track(project, sequence_id, audio_track_id)?;
        if video_plan.segments.is_empty() || audio_plan.segments.is_empty() {
            return Err(ExportError::EmptyPlan);
        }
        if video_plan.duration != audio_plan.duration {
            return Err(ExportError::MismatchedPlanDuration);
        }
        if request.width == 0
            || request.height == 0
            || !request.frame_rate.is_positive()
            || request.video_bitrate_kbps == 0
            || request.audio_bitrate_kbps == 0
            || u64::from(request.width)
                .checked_mul(u64::from(request.height))
                .is_none_or(|pixels| pixels > 134_217_728)
        {
            return Err(ExportError::InvalidVideoSettings);
        }
        validate_output_path(&request.output)?;
        if cancellation.is_some_and(ExportCancellation::is_cancelled) {
            return Err(ExportError::Cancelled);
        }
        let temporary = temporary_output_path(&request.output)?;
        let guard = TemporaryExport::new(temporary);
        let arguments = build_video_audio_ffmpeg_arguments(
            project,
            &video_plan,
            &audio_plan,
            request,
            guard.path(),
        )?;
        let mut command = Command::new(&self.ffmpeg);
        command.args(arguments);
        let output = run_encoder(command, cancellation)?;
        if !output.status.success() {
            return Err(ExportError::Encoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let report =
            validate_temporary_output_with_audio(&self.ffprobe, guard.path(), &request.output)?;
        guard.finalize(&request.output)?;
        Ok(report)
    }
}

fn run_encoder(
    mut command: Command,
    cancellation: Option<&ExportCancellation>,
) -> Result<Output, ExportError> {
    let Some(cancellation) = cancellation else {
        return Ok(command.output()?);
    };
    command.stderr(Stdio::piped());
    let mut child: Child = command.spawn()?;
    loop {
        if cancellation.is_cancelled() {
            child.kill()?;
            let _ = child.wait();
            return Err(ExportError::Cancelled);
        }
        if child.try_wait()?.is_some() {
            return Ok(child.wait_with_output()?);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn build_sequence_ffmpeg_arguments(
    project: &Project,
    plan: &SingleTrackExportPlan,
    request: &SingleTrackMp4ExportRequest,
) -> Result<Vec<OsString>, ExportError> {
    let mut arguments = vec![OsString::from("-v"), OsString::from("error")];
    for segment in &plan.segments {
        match segment {
            SequenceExportSegment::Gap { duration, .. } => arguments.extend([
                OsString::from("-f"),
                OsString::from("lavfi"),
                OsString::from("-t"),
                OsString::from(format_timestamp(*duration)?),
                OsString::from("-i"),
                OsString::from(format!(
                    "color=c=black:s={}x{}:r={}/{}",
                    request.width,
                    request.height,
                    request.frame_rate.value(),
                    request.frame_rate.timescale()
                )),
            ]),
            SequenceExportSegment::Clip {
                media_id,
                source_start,
                duration,
                ..
            } => {
                let source = project
                    .media
                    .get(media_id)
                    .ok_or(ExportError::MissingMedia)?;
                let source_path = PathBuf::from(&source.source_path);
                if !source_path.is_file() {
                    return Err(ExportError::InvalidSource(source_path));
                }
                arguments.extend([
                    OsString::from("-ss"),
                    OsString::from(format_timestamp(*source_start)?),
                    OsString::from("-t"),
                    OsString::from(format_timestamp(*duration)?),
                    OsString::from("-i"),
                    source_path.into_os_string(),
                ]);
            }
        }
    }
    arguments.extend([
        OsString::from("-filter_complex"),
        OsString::from(build_concat_filter(plan, request)),
        OsString::from("-map"),
        OsString::from("[outv]"),
    ]);
    Ok(arguments)
}

fn build_video_audio_ffmpeg_arguments(
    project: &Project,
    video_plan: &SingleTrackExportPlan,
    audio_plan: &SingleAudioTrackExportPlan,
    request: &VideoAudioMp4ExportRequest,
    output: &Path,
) -> Result<Vec<OsString>, ExportError> {
    let mut arguments = vec![OsString::from("-v"), OsString::from("error")];
    append_video_inputs(&mut arguments, project, video_plan, request)?;
    append_audio_inputs(&mut arguments, project, audio_plan)?;
    let mut filter = build_concat_filter(
        video_plan,
        &SingleTrackMp4ExportRequest {
            width: request.width,
            height: request.height,
            frame_rate: request.frame_rate,
            video_bitrate_kbps: request.video_bitrate_kbps,
            output: output.to_path_buf(),
        },
    );
    filter.push(';');
    filter.push_str(&build_audio_concat_filter(
        audio_plan,
        video_plan.segments.len(),
    ));
    arguments.extend([
        OsString::from("-filter_complex"),
        OsString::from(filter),
        OsString::from("-map"),
        OsString::from("[outv]"),
        OsString::from("-map"),
        OsString::from("[outa]"),
        OsString::from("-r"),
        OsString::from(format!(
            "{}/{}",
            request.frame_rate.value(),
            request.frame_rate.timescale()
        )),
        OsString::from("-c:v"),
        OsString::from("mpeg4"),
        OsString::from("-b:v"),
        OsString::from(format!("{}k", request.video_bitrate_kbps)),
        OsString::from("-c:a"),
        OsString::from("aac"),
        OsString::from("-b:a"),
        OsString::from(format!("{}k", request.audio_bitrate_kbps)),
        OsString::from("-movflags"),
        OsString::from("+faststart"),
        OsString::from("-f"),
        OsString::from("mp4"),
        OsString::from("-y"),
        output.as_os_str().to_owned(),
    ]);
    Ok(arguments)
}

fn append_video_inputs(
    arguments: &mut Vec<OsString>,
    project: &Project,
    plan: &SingleTrackExportPlan,
    request: &VideoAudioMp4ExportRequest,
) -> Result<(), ExportError> {
    for segment in &plan.segments {
        match segment {
            SequenceExportSegment::Gap { duration, .. } => arguments.extend([
                OsString::from("-f"),
                OsString::from("lavfi"),
                OsString::from("-t"),
                OsString::from(format_timestamp(*duration)?),
                OsString::from("-i"),
                OsString::from(format!(
                    "color=c=black:s={}x{}:r={}/{}",
                    request.width,
                    request.height,
                    request.frame_rate.value(),
                    request.frame_rate.timescale()
                )),
            ]),
            SequenceExportSegment::Clip { .. } => append_source_input(arguments, project, segment)?,
        }
    }
    Ok(())
}

fn append_audio_inputs(
    arguments: &mut Vec<OsString>,
    project: &Project,
    plan: &SingleAudioTrackExportPlan,
) -> Result<(), ExportError> {
    for segment in &plan.segments {
        match segment {
            SequenceExportSegment::Gap { duration, .. } => arguments.extend([
                OsString::from("-f"),
                OsString::from("lavfi"),
                OsString::from("-t"),
                OsString::from(format_timestamp(*duration)?),
                OsString::from("-i"),
                OsString::from("anullsrc=r=48000:cl=stereo"),
            ]),
            SequenceExportSegment::Clip { .. } => append_source_input(arguments, project, segment)?,
        }
    }
    Ok(())
}

fn append_source_input(
    arguments: &mut Vec<OsString>,
    project: &Project,
    segment: &SequenceExportSegment,
) -> Result<(), ExportError> {
    let SequenceExportSegment::Clip {
        media_id,
        source_start,
        duration,
        ..
    } = segment
    else {
        return Ok(());
    };
    let source = project
        .media
        .get(media_id)
        .ok_or(ExportError::MissingMedia)?;
    let source_path = PathBuf::from(&source.source_path);
    if !source_path.is_file() {
        return Err(ExportError::InvalidSource(source_path));
    }
    arguments.extend([
        OsString::from("-ss"),
        OsString::from(format_timestamp(*source_start)?),
        OsString::from("-t"),
        OsString::from(format_timestamp(*duration)?),
        OsString::from("-i"),
        source_path.into_os_string(),
    ]);
    Ok(())
}

fn build_concat_filter(
    plan: &SingleTrackExportPlan,
    request: &SingleTrackMp4ExportRequest,
) -> String {
    let mut filter = String::new();
    for index in 0..plan.segments.len() {
        let _ = write!(
            filter,
            "[{index}:v:0]scale={}:{},setsar=1,setpts=PTS-STARTPTS[v{index}];",
            request.width, request.height
        );
    }
    for index in 0..plan.segments.len() {
        let _ = write!(filter, "[v{index}]");
    }
    let _ = write!(filter, "concat=n={}:v=1:a=0[outv]", plan.segments.len());
    filter
}

fn validate_request(request: &BasicMp4ExportRequest) -> Result<(), ExportError> {
    if !request.source.is_file() {
        return Err(ExportError::InvalidSource(request.source.clone()));
    }
    if !request.source_start.is_non_negative() || !request.duration.is_positive() {
        return Err(ExportError::InvalidRange);
    }
    validate_output_path(&request.output)
}

fn validate_output_path(output: &Path) -> Result<(), ExportError> {
    if output.exists() {
        return Err(ExportError::OutputExists(output.to_path_buf()));
    }
    let parent = output
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| ExportError::InvalidOutputParent(output.to_path_buf()))?;
    if parent.as_os_str().is_empty() {
        return Err(ExportError::InvalidOutputParent(output.to_path_buf()));
    }
    Ok(())
}

fn validate_temporary_output(
    ffprobe: &Path,
    temporary: &Path,
    final_output: &Path,
) -> Result<ExportReport, ExportError> {
    let metadata = FfprobeInspector::new(ffprobe).inspect(temporary)?;
    let byte_length = fs::metadata(temporary)?.len();
    if byte_length == 0 {
        return Err(ExportError::InvalidOutput("file is empty"));
    }
    let duration = metadata
        .duration
        .ok_or(ExportError::InvalidOutput("duration is missing"))?;
    if !duration.is_positive() {
        return Err(ExportError::InvalidOutput("duration is not positive"));
    }
    let has_video = metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)));
    if !has_video {
        return Err(ExportError::InvalidOutput("video stream is missing"));
    }
    let has_audio = metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Audio(_)));
    Ok(ExportReport {
        output: final_output.to_path_buf(),
        byte_length,
        duration,
        has_video,
        has_audio,
    })
}

fn validate_temporary_output_with_audio(
    ffprobe: &Path,
    temporary: &Path,
    final_output: &Path,
) -> Result<ExportReport, ExportError> {
    let report = validate_temporary_output(ffprobe, temporary, final_output)?;
    if !report.has_audio {
        return Err(ExportError::InvalidOutput("audio stream is missing"));
    }
    Ok(report)
}

fn format_timestamp(time: RationalTime) -> Result<String, ExportError> {
    let nanoseconds = time.to_ticks(1_000_000_000, RoundingMode::NearestTiesAway)?;
    let seconds = nanoseconds / 1_000_000_000;
    let subsecond = nanoseconds % 1_000_000_000;
    Ok(format!("{seconds}.{subsecond:09}"))
}

fn temporary_output_path(output: &Path) -> Result<PathBuf, ExportError> {
    let parent = output
        .parent()
        .ok_or_else(|| ExportError::InvalidOutputParent(output.to_path_buf()))?;
    let name = output
        .file_name()
        .ok_or_else(|| ExportError::InvalidOutputParent(output.to_path_buf()))?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.{}.tmp.mp4", Uuid::new_v4())))
}

struct TemporaryExport {
    path: PathBuf,
    finalized: bool,
}

impl TemporaryExport {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            finalized: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn finalize(mut self, output: &Path) -> Result<(), std::io::Error> {
        fs::hard_link(&self.path, output)?;
        fs::remove_file(&self.path)?;
        self.finalized = true;
        Ok(())
    }
}

impl Drop for TemporaryExport {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BasicMp4ExportRequest, ExportCancellation, ExportError, ExportPreset,
        SequenceExportSegment, SingleTrackMp4ExportRequest, VideoAudioMp4ExportRequest,
        build_audio_concat_filter, build_video_audio_ffmpeg_arguments, format_timestamp,
        plan_single_audio_track, plan_single_video_track, validate_request,
    };
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind};
    use prism_timeline::RationalTime;
    use std::{fs, path::PathBuf};

    #[test]
    fn timestamp_format_uses_exact_named_rounding() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            format_timestamp(RationalTime::new(1_001, 30_000)?)?,
            "0.033366667"
        );
        Ok(())
    }

    #[test]
    fn export_presets_produce_exact_settings_and_reject_unknown_names()
    -> Result<(), Box<dyn std::error::Error>> {
        let preset = ExportPreset::parse("youtube-1080p")?;
        let (width, height, frame_rate, bitrate) = preset.settings();
        assert_eq!((width, height), (1920, 1080));
        assert_eq!(frame_rate, RationalTime::new(30_000, 1_001)?);
        assert_eq!(bitrate, 8_000);
        let request = SingleTrackMp4ExportRequest::from_preset(preset, PathBuf::from("out.mp4"));
        assert_eq!(request.width, 1920);
        let audio_request = VideoAudioMp4ExportRequest::from_preset(
            ExportPreset::Youtube4k,
            PathBuf::from("4k.mp4"),
        );
        assert_eq!((audio_request.width, audio_request.height), (3840, 2160));
        assert_eq!(audio_request.video_bitrate_kbps, 35_000);
        assert!(matches!(
            ExportPreset::parse("unknown"),
            Err(ExportError::UnknownPreset(name)) if name == "unknown"
        ));
        Ok(())
    }

    #[test]
    fn invalid_source_is_rejected_before_encoder_launch() -> Result<(), Box<dyn std::error::Error>>
    {
        let request = BasicMp4ExportRequest {
            source: PathBuf::from("source-that-does-not-exist.mp4"),
            source_start: RationalTime::ZERO,
            duration: RationalTime::new(1, 1)?,
            output: PathBuf::from("output.mp4"),
        };
        assert!(matches!(
            validate_request(&request),
            Err(ExportError::InvalidSource(_))
        ));
        Ok(())
    }

    #[test]
    fn pre_cancelled_export_does_not_launch_encoder() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("prism-cancel-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let source = directory.join("source.mp4");
        let output = directory.join("output.mp4");
        fs::write(&source, b"not decoded because cancellation is preflight")?;
        let cancellation = ExportCancellation::new();
        cancellation.cancel();
        let exporter = super::BasicMp4Exporter::new("ffmpeg", "ffprobe");
        let result = exporter.export_with_cancellation(
            &BasicMp4ExportRequest {
                source,
                source_start: RationalTime::ZERO,
                duration: RationalTime::new(1, 1)?,
                output,
            },
            Some(&cancellation),
        );
        assert!(matches!(result, Err(ExportError::Cancelled)));
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn pre_cancelled_track_export_does_not_launch_encoder() -> Result<(), Box<dyn std::error::Error>>
    {
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let mut project = Project::new("Cancelled track");
        project.media.insert(
            media_id,
            MediaAsset {
                id: media_id,
                source_path: "missing.mp4".to_owned(),
            },
        );
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Video,
                    clips: vec![Clip {
                        id: ClipId::new(),
                        media_id,
                        timeline_start: RationalTime::ZERO,
                        source_start: RationalTime::ZERO,
                        source_duration: RationalTime::new(1, 1)?,
                    }],
                    locked: false,
                    visible: true,
                    muted: false,
                }],
            },
        );
        let directory =
            std::env::temp_dir().join(format!("prism-track-cancel-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let cancellation = ExportCancellation::new();
        cancellation.cancel();
        let result = super::BasicMp4Exporter::new("ffmpeg", "ffprobe")
            .export_single_video_track_with_cancellation(
                &project,
                sequence_id,
                track_id,
                &super::SingleTrackMp4ExportRequest {
                    width: 320,
                    height: 180,
                    frame_rate: RationalTime::new(30, 1)?,
                    video_bitrate_kbps: 1_000,
                    output: directory.join("output.mp4"),
                },
                Some(&cancellation),
            );
        assert!(matches!(result, Err(ExportError::Cancelled)));
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn running_export_is_terminated_and_temp_output_is_removed_on_cancel()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{os::unix::fs::PermissionsExt, sync::mpsc, time::Duration};

        let directory =
            std::env::temp_dir().join(format!("prism-running-cancel-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let source = directory.join("source.mp4");
        let fake_ffmpeg = directory.join("fake-ffmpeg.sh");
        let output = directory.join("output.mp4");
        fs::write(&source, b"source")?;
        fs::write(&fake_ffmpeg, b"#!/bin/sh\nwhile true; do :; done\n")?;
        let mut permissions = fs::metadata(&fake_ffmpeg)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_ffmpeg, permissions)?;
        let cancellation = ExportCancellation::new();
        let worker_token = cancellation.clone();
        let (started, ready) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(()).ok();
            let duration = RationalTime::new(1, 1)?;
            super::BasicMp4Exporter::new(fake_ffmpeg, "unused-ffprobe").export_with_cancellation(
                &BasicMp4ExportRequest {
                    source,
                    source_start: RationalTime::ZERO,
                    duration,
                    output,
                },
                Some(&worker_token),
            )
        });
        ready.recv()?;
        std::thread::sleep(Duration::from_millis(50));
        cancellation.cancel();
        let Ok(result) = worker.join() else {
            return Err("export worker panicked".into());
        };
        assert!(matches!(result, Err(ExportError::Cancelled)));
        assert!(!directory.join("output.mp4").exists());
        assert_eq!(fs::read_dir(&directory)?.count(), 2);
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn sequence_plan_preserves_trims_and_makes_gaps_explicit()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Export plan");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let first_clip_id = ClipId::new();
        let second_clip_id = ClipId::new();
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
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Video,
                    clips: vec![
                        Clip {
                            id: first_clip_id,
                            media_id,
                            timeline_start: RationalTime::new(1, 1)?,
                            source_start: RationalTime::new(10, 1)?,
                            source_duration: RationalTime::new(2, 1)?,
                        },
                        Clip {
                            id: second_clip_id,
                            media_id,
                            timeline_start: RationalTime::new(4, 1)?,
                            source_start: RationalTime::new(20, 1)?,
                            source_duration: RationalTime::new(1, 1)?,
                        },
                    ],
                    locked: false,
                    visible: true,
                    muted: false,
                }],
            },
        );
        project.validate()?;
        let plan = plan_single_video_track(&project, sequence_id, track_id)?;
        assert_eq!(plan.duration, RationalTime::new(5, 1)?);
        assert_eq!(plan.segments.len(), 4);
        assert_eq!(
            plan.segments[0],
            SequenceExportSegment::Gap {
                timeline_start: RationalTime::ZERO,
                duration: RationalTime::new(1, 1)?,
            }
        );
        assert_eq!(
            plan.segments[1],
            SequenceExportSegment::Clip {
                clip_id: first_clip_id,
                media_id,
                timeline_start: RationalTime::new(1, 1)?,
                source_start: RationalTime::new(10, 1)?,
                duration: RationalTime::new(2, 1)?,
            }
        );
        assert_eq!(
            plan.segments[2],
            SequenceExportSegment::Gap {
                timeline_start: RationalTime::new(3, 1)?,
                duration: RationalTime::new(1, 1)?,
            }
        );
        assert_eq!(
            plan.segments[3],
            SequenceExportSegment::Clip {
                clip_id: second_clip_id,
                media_id,
                timeline_start: RationalTime::new(4, 1)?,
                source_start: RationalTime::new(20, 1)?,
                duration: RationalTime::new(1, 1)?,
            }
        );
        Ok(())
    }

    #[test]
    fn sequence_plan_rejects_audio_track() {
        let mut project = Project::new("Audio only");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Audio,
                    clips: Vec::new(),
                    locked: false,
                    visible: true,
                    muted: false,
                }],
            },
        );
        assert!(matches!(
            plan_single_video_track(&project, sequence_id, track_id),
            Err(ExportError::TrackIsNotVideo)
        ));
    }

    #[test]
    fn audio_sequence_plan_preserves_trims_and_silence_gaps()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Audio plan");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let clip_id = ClipId::new();
        project.media.insert(
            media_id,
            MediaAsset {
                id: media_id,
                source_path: "speech.wav".to_owned(),
            },
        );
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Audio,
                    clips: vec![Clip {
                        id: clip_id,
                        media_id,
                        timeline_start: RationalTime::new(2, 1)?,
                        source_start: RationalTime::new(7, 1)?,
                        source_duration: RationalTime::new(3, 2)?,
                    }],
                    locked: false,
                    visible: true,
                    muted: false,
                }],
            },
        );
        project.validate()?;
        let plan = plan_single_audio_track(&project, sequence_id, track_id)?;
        assert_eq!(plan.duration, RationalTime::new(7, 2)?);
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(
            plan.segments[0],
            SequenceExportSegment::Gap {
                timeline_start: RationalTime::ZERO,
                duration: RationalTime::new(2, 1)?,
            }
        );
        assert_eq!(
            plan.segments[1],
            SequenceExportSegment::Clip {
                clip_id,
                media_id,
                timeline_start: RationalTime::new(2, 1)?,
                source_start: RationalTime::new(7, 1)?,
                duration: RationalTime::new(3, 2)?,
            }
        );
        Ok(())
    }

    #[test]
    fn audio_sequence_plan_rejects_muted_track() {
        let mut project = Project::new("Muted audio");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Audio,
                    clips: Vec::new(),
                    locked: false,
                    visible: true,
                    muted: true,
                }],
            },
        );
        assert!(matches!(
            plan_single_audio_track(&project, sequence_id, track_id),
            Err(ExportError::TrackMuted)
        ));
    }

    #[test]
    fn audio_filter_labels_inputs_and_concatenates_segments()
    -> Result<(), Box<dyn std::error::Error>> {
        let plan = super::SingleAudioTrackExportPlan {
            sequence_id: SequenceId::new(),
            track_id: TrackId::new(),
            segments: vec![
                SequenceExportSegment::Gap {
                    timeline_start: RationalTime::ZERO,
                    duration: RationalTime::new(1, 1)?,
                },
                SequenceExportSegment::Clip {
                    clip_id: ClipId::new(),
                    media_id: MediaAssetId::new(),
                    timeline_start: RationalTime::new(1, 1)?,
                    source_start: RationalTime::ZERO,
                    duration: RationalTime::new(2, 1)?,
                },
            ],
            duration: RationalTime::new(3, 1)?,
        };
        assert_eq!(
            build_audio_concat_filter(&plan, 4),
            "[4:a:0]aresample=async=1:first_pts=0,asetpts=PTS-STARTPTS[a0];[5:a:0]aresample=async=1:first_pts=0,asetpts=PTS-STARTPTS[a1];[a0][a1]concat=n=2:v=0:a=1[outa]"
        );
        let empty = super::SingleAudioTrackExportPlan {
            sequence_id: SequenceId::new(),
            track_id: TrackId::new(),
            segments: Vec::new(),
            duration: RationalTime::ZERO,
        };
        assert_eq!(
            build_audio_concat_filter(&empty, 0),
            "anullsrc=r=48000:cl=stereo,atrim=duration=0[outa]"
        );
        Ok(())
    }

    #[test]
    fn combined_export_rejects_mismatched_video_audio_duration()
    -> Result<(), Box<dyn std::error::Error>> {
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let video_track_id = TrackId::new();
        let audio_track_id = TrackId::new();
        let mut project = Project::new("Mismatched A/V");
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
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![
                    Track {
                        id: video_track_id,
                        kind: TrackKind::Video,
                        clips: vec![Clip {
                            id: ClipId::new(),
                            media_id,
                            timeline_start: RationalTime::ZERO,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(2, 1)?,
                        }],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                    Track {
                        id: audio_track_id,
                        kind: TrackKind::Audio,
                        clips: vec![Clip {
                            id: ClipId::new(),
                            media_id,
                            timeline_start: RationalTime::ZERO,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(1, 1)?,
                        }],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                ],
            },
        );
        let directory =
            std::env::temp_dir().join(format!("prism-av-plan-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let result = super::BasicMp4Exporter::new("unused-ffmpeg", "unused-ffprobe")
            .export_single_video_audio_track(
                &project,
                sequence_id,
                video_track_id,
                audio_track_id,
                &super::VideoAudioMp4ExportRequest {
                    width: 320,
                    height: 180,
                    frame_rate: RationalTime::new(30, 1)?,
                    video_bitrate_kbps: 1_000,
                    audio_bitrate_kbps: 192,
                    output: directory.join("out.mp4"),
                },
            );
        assert!(matches!(result, Err(ExportError::MismatchedPlanDuration)));
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn combined_export_arguments_map_video_and_audio_filters()
    -> Result<(), Box<dyn std::error::Error>> {
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let video_track_id = TrackId::new();
        let audio_track_id = TrackId::new();
        let directory =
            std::env::temp_dir().join(format!("prism-av-args-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let source = directory.join("source.mp4");
        fs::write(&source, b"fixture")?;
        let mut project = Project::new("A/V arguments");
        project.media.insert(
            media_id,
            MediaAsset {
                id: media_id,
                source_path: source.display().to_string(),
            },
        );
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![
                    Track {
                        id: video_track_id,
                        kind: TrackKind::Video,
                        clips: vec![Clip {
                            id: ClipId::new(),
                            media_id,
                            timeline_start: RationalTime::ZERO,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(1, 1)?,
                        }],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                    Track {
                        id: audio_track_id,
                        kind: TrackKind::Audio,
                        clips: vec![Clip {
                            id: ClipId::new(),
                            media_id,
                            timeline_start: RationalTime::ZERO,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(1, 1)?,
                        }],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                ],
            },
        );
        let video_plan = plan_single_video_track(&project, sequence_id, video_track_id)?;
        let audio_plan = plan_single_audio_track(&project, sequence_id, audio_track_id)?;
        let output = directory.join("out.mp4");
        let arguments = build_video_audio_ffmpeg_arguments(
            &project,
            &video_plan,
            &audio_plan,
            &VideoAudioMp4ExportRequest {
                width: 320,
                height: 180,
                frame_rate: RationalTime::new(30, 1)?,
                video_bitrate_kbps: 1_000,
                audio_bitrate_kbps: 192,
                output: output.clone(),
            },
            &output,
        )?;
        let joined = arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(joined.contains("[outv]"));
        assert!(joined.contains("[outa]"));
        assert!(joined.contains("-map [outv]"));
        assert!(joined.contains("-map [outa]"));
        assert!(joined.contains("-c:a aac"));
        assert!(joined.contains("-b:v 1000k"));
        assert!(joined.contains("-b:a 192k"));
        fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn sequence_plan_rejects_hidden_video_track() {
        let mut project = Project::new("Hidden");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Video,
                    clips: Vec::new(),
                    locked: false,
                    visible: false,
                    muted: false,
                }],
            },
        );
        assert!(matches!(
            plan_single_video_track(&project, sequence_id, track_id),
            Err(ExportError::TrackHidden)
        ));
    }
}
