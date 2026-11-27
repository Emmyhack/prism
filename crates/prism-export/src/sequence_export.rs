//! Complete multitrack sequence planning and feature-gated `FFmpeg` execution.

use super::{
    BasicMp4Exporter, ExportCancellation, ExportError, ExportPreset, ExportReport,
    SequenceExportSegment, SingleAudioTrackExportPlan, TemporaryExport, format_timestamp,
    plan_single_audio_track, run_encoder, temporary_output_path, validate_output_path,
    validate_temporary_output, validate_temporary_output_with_audio,
};
use prism_audio::StereoGainPan;
use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_project::{Project, Sequence, Track, TrackKind, Transition};
use prism_timeline::RationalTime;
use std::{ffi::OsString, fmt::Write as _, path::PathBuf, process::Command};

/// Output settings for complete multitrack sequence export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceMp4ExportRequest {
    /// Final encoded width in pixels.
    pub width: u32,
    /// Final encoded height in pixels.
    pub height: u32,
    /// Exact output frame rate. Frames are sampled on this grid with nearest
    /// timestamp selection after each source is decoded.
    pub frame_rate: RationalTime,
    /// Target H.264 bitrate in kilobits per second.
    pub video_bitrate_kbps: u32,
    /// Target AAC bitrate in kilobits per second.
    pub audio_bitrate_kbps: u32,
    /// New final output path.
    pub output: PathBuf,
}

impl SequenceMp4ExportRequest {
    /// Creates a complete-sequence request from a fixed built-in preset.
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

    /// Creates a source-match request after media inspection supplies exact
    /// source geometry and frame rate.
    #[must_use]
    pub fn source_match(
        width: u32,
        height: u32,
        frame_rate: RationalTime,
        video_bitrate_kbps: u32,
        output: PathBuf,
    ) -> Self {
        Self::custom(width, height, frame_rate, video_bitrate_kbps, 192, output)
    }

    /// Creates a custom request with caller-selected settings.
    #[must_use]
    pub fn custom(
        width: u32,
        height: u32,
        frame_rate: RationalTime,
        video_bitrate_kbps: u32,
        audio_bitrate_kbps: u32,
        output: PathBuf,
    ) -> Self {
        Self {
            width,
            height,
            frame_rate,
            video_bitrate_kbps,
            audio_bitrate_kbps,
            output,
        }
    }
}

/// One contiguous visual segment in a composed video track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SequenceVideoSegment {
    /// Transparent track time with no active clip.
    Gap {
        /// Exact sequence start.
        timeline_start: RationalTime,
        /// Exact positive duration.
        duration: RationalTime,
    },
    /// One visible source range.
    Clip {
        /// Authoritative clip identifier.
        clip_id: ClipId,
        /// Referenced media.
        media_id: MediaAssetId,
        /// Exact sequence start.
        timeline_start: RationalTime,
        /// Exact source start.
        source_start: RationalTime,
        /// Exact positive duration.
        duration: RationalTime,
    },
    /// Linear dissolve rendered from two source ranges and their handles.
    Dissolve {
        /// Outgoing clip.
        outgoing_clip: ClipId,
        /// Incoming clip.
        incoming_clip: ClipId,
        /// Outgoing media.
        outgoing_media: MediaAssetId,
        /// Incoming media.
        incoming_media: MediaAssetId,
        /// Exact sequence start of the transition window.
        timeline_start: RationalTime,
        /// Exact outgoing source start including post-roll.
        outgoing_source_start: RationalTime,
        /// Exact incoming source start including pre-roll.
        incoming_source_start: RationalTime,
        /// Exact transition duration.
        duration: RationalTime,
        /// Required outgoing media after its visible range.
        outgoing_postroll: RationalTime,
    },
}

/// Deterministic visual plan for one visible video track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceVideoTrackPlan {
    /// Authoritative track identifier.
    pub track_id: TrackId,
    /// Contiguous segments covering the complete sequence duration.
    pub segments: Vec<SequenceVideoSegment>,
}

/// Deterministic audible plan for one unmuted audio track.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceAudioTrackPlan {
    /// Authoritative track identifier.
    pub track_id: TrackId,
    /// Existing exact clip/gap plan; the filter pads its tail to sequence end.
    pub plan: SingleAudioTrackExportPlan,
}

/// Complete active-track interpretation of a sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SequenceExportPlan {
    /// Authoritative sequence identifier.
    pub sequence_id: SequenceId,
    /// Full sequence duration, including gaps and inactive-track tail time.
    pub duration: RationalTime,
    /// Visible video tracks in authoritative track order.
    pub video_tracks: Vec<SequenceVideoTrackPlan>,
    /// Unmuted audio tracks in authoritative track order.
    pub audio_tracks: Vec<SequenceAudioTrackPlan>,
}

/// Plans all visible video and unmuted audio tracks without launching tools.
///
/// # Errors
///
/// Returns [`ExportError`] for missing sequences, empty output, invalid layout,
/// unavailable incoming preroll, or exact-time overflow.
pub fn plan_sequence_export(
    project: &Project,
    sequence_id: SequenceId,
) -> Result<SequenceExportPlan, ExportError> {
    let sequence = project
        .sequences
        .get(&sequence_id)
        .ok_or(ExportError::SequenceNotFound)?;
    let duration = sequence.duration()?;
    if !duration.is_positive() {
        return Err(ExportError::EmptyPlan);
    }
    let mut video_tracks = Vec::new();
    let mut audio_tracks = Vec::new();
    for track in &sequence.tracks {
        match track.kind {
            TrackKind::Video if track.visible && !track.clips.is_empty() => {
                video_tracks.push(plan_video_track(sequence, track, duration)?);
            }
            TrackKind::Audio if !track.muted && !track.clips.is_empty() => {
                audio_tracks.push(SequenceAudioTrackPlan {
                    track_id: track.id,
                    plan: plan_single_audio_track(project, sequence_id, track.id)?,
                });
            }
            TrackKind::Video | TrackKind::Audio => {}
        }
    }
    if video_tracks.is_empty() {
        return Err(ExportError::EmptyPlan);
    }
    Ok(SequenceExportPlan {
        sequence_id,
        duration,
        video_tracks,
        audio_tracks,
    })
}

fn plan_video_track(
    sequence: &Sequence,
    track: &Track,
    sequence_duration: RationalTime,
) -> Result<SequenceVideoTrackPlan, ExportError> {
    let mut segments = Vec::new();
    let mut cursor = RationalTime::ZERO;
    for (index, clip) in track.clips.iter().enumerate() {
        let incoming = index
            .checked_sub(1)
            .and_then(|previous| transition_between(sequence, track.clips[previous].id, clip.id));
        let outgoing = track
            .clips
            .get(index.saturating_add(1))
            .and_then(|next| transition_between(sequence, clip.id, next.id));
        let incoming_after = incoming
            .map(Transition::handles)
            .transpose()?
            .map_or(RationalTime::ZERO, |handles| handles.after_cut);
        let outgoing_before = outgoing
            .map(Transition::handles)
            .transpose()?
            .map_or(RationalTime::ZERO, |handles| handles.before_cut);
        let normal_start = clip.timeline_start.checked_add(incoming_after)?;
        if cursor < normal_start {
            push_gap(&mut segments, cursor, normal_start.checked_sub(cursor)?)?;
        }
        let clip_end = clip.timeline_start.checked_add(clip.source_duration)?;
        let normal_end = clip_end.checked_sub(outgoing_before)?;
        if normal_end > normal_start {
            let source_start = clip.source_start.checked_add(incoming_after)?;
            segments.push(SequenceVideoSegment::Clip {
                clip_id: clip.id,
                media_id: clip.media_id,
                timeline_start: normal_start,
                source_start,
                duration: normal_end.checked_sub(normal_start)?,
            });
        }
        if let Some(transition) = outgoing {
            let next = track
                .clips
                .get(index.saturating_add(1))
                .ok_or(ExportError::InvalidTrackLayout)?;
            let handles = transition.handles()?;
            let incoming_source_start = next.source_start.checked_sub(handles.incoming_preroll)?;
            if incoming_source_start.is_negative() {
                return Err(ExportError::SourceHandleUnavailable(next.id));
            }
            segments.push(SequenceVideoSegment::Dissolve {
                outgoing_clip: clip.id,
                incoming_clip: next.id,
                outgoing_media: clip.media_id,
                incoming_media: next.media_id,
                timeline_start: normal_end,
                outgoing_source_start: clip
                    .source_start
                    .checked_add(clip.source_duration)?
                    .checked_sub(handles.before_cut)?,
                incoming_source_start,
                duration: transition.duration,
                outgoing_postroll: handles.outgoing_postroll,
            });
            cursor = normal_end.checked_add(transition.duration)?;
        } else {
            cursor = clip_end;
        }
    }
    if cursor < sequence_duration {
        push_gap(
            &mut segments,
            cursor,
            sequence_duration.checked_sub(cursor)?,
        )?;
    }
    Ok(SequenceVideoTrackPlan {
        track_id: track.id,
        segments,
    })
}

fn transition_between(
    sequence: &Sequence,
    outgoing: ClipId,
    incoming: ClipId,
) -> Option<&Transition> {
    sequence.transitions.iter().find(|transition| {
        transition.outgoing_clip == outgoing && transition.incoming_clip == incoming
    })
}

fn push_gap(
    segments: &mut Vec<SequenceVideoSegment>,
    timeline_start: RationalTime,
    duration: RationalTime,
) -> Result<(), ExportError> {
    if !duration.is_positive() {
        return Err(ExportError::InvalidTrackLayout);
    }
    segments.push(SequenceVideoSegment::Gap {
        timeline_start,
        duration,
    });
    Ok(())
}

impl BasicMp4Exporter {
    /// Renders all active sequence tracks with composition, dissolves and audio mixing.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] for invalid plans/settings/handles, tool failure,
    /// cancellation, output validation, or atomic finalization failure.
    pub fn export_sequence(
        &self,
        project: &Project,
        sequence_id: SequenceId,
        request: &SequenceMp4ExportRequest,
    ) -> Result<ExportReport, ExportError> {
        self.export_sequence_with_control(project, sequence_id, request, None, |_| {})
    }

    /// Complete sequence export with cancellation and monotonic stage progress.
    ///
    /// Progress reports planning (0), validated inputs (10), encoded output
    /// (90), and finalized output (100). Encoder-level progress remains a future
    /// refinement; values never move backwards.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::export_sequence`].
    pub fn export_sequence_with_control<F>(
        &self,
        project: &Project,
        sequence_id: SequenceId,
        request: &SequenceMp4ExportRequest,
        cancellation: Option<&ExportCancellation>,
        progress: F,
    ) -> Result<ExportReport, ExportError>
    where
        F: Fn(u8),
    {
        progress(0);
        validate_sequence_request(request)?;
        validate_output_path(&request.output)?;
        if cancellation.is_some_and(ExportCancellation::is_cancelled) {
            return Err(ExportError::Cancelled);
        }
        let plan = plan_sequence_export(project, sequence_id)?;
        validate_outgoing_handles(self, project, &plan)?;
        progress(10);
        let temporary = temporary_output_path(&request.output)?;
        let guard = TemporaryExport::new(temporary);
        let arguments = build_arguments(project, &plan, request, guard.path())?;
        let mut command = Command::new(&self.ffmpeg);
        command.args(arguments);
        let output = run_encoder(command, cancellation)?;
        if !output.status.success() {
            return Err(ExportError::Encoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        progress(90);
        let report = if plan.audio_tracks.is_empty() {
            validate_temporary_output(&self.ffprobe, guard.path(), &request.output)?
        } else {
            validate_temporary_output_with_audio(&self.ffprobe, guard.path(), &request.output)?
        };
        guard.finalize(&request.output)?;
        progress(100);
        Ok(report)
    }
}

fn validate_sequence_request(request: &SequenceMp4ExportRequest) -> Result<(), ExportError> {
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
    Ok(())
}

fn validate_outgoing_handles(
    exporter: &BasicMp4Exporter,
    project: &Project,
    plan: &SequenceExportPlan,
) -> Result<(), ExportError> {
    for track in &plan.video_tracks {
        for segment in &track.segments {
            let SequenceVideoSegment::Dissolve {
                outgoing_clip,
                outgoing_media,
                outgoing_source_start,
                duration,
                ..
            } = segment
            else {
                continue;
            };
            let source = project
                .media
                .get(outgoing_media)
                .ok_or(ExportError::MissingMedia)?;
            let path = PathBuf::from(&source.source_path);
            let available = prism_media::FfprobeInspector::new(&exporter.ffprobe)
                .inspect(&path)?
                .duration
                .ok_or(ExportError::SourceHandleUnavailable(*outgoing_clip))?;
            if outgoing_source_start.checked_add(*duration)? > available {
                return Err(ExportError::SourceHandleUnavailable(*outgoing_clip));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn build_arguments(
    project: &Project,
    plan: &SequenceExportPlan,
    request: &SequenceMp4ExportRequest,
    output: &std::path::Path,
) -> Result<Vec<OsString>, ExportError> {
    let mut arguments = vec![OsString::from("-v"), OsString::from("error")];
    let mut next_input = 0_usize;
    let mut video_inputs = Vec::new();
    for track in &plan.video_tracks {
        let mut track_inputs = Vec::new();
        for segment in &track.segments {
            match segment {
                SequenceVideoSegment::Gap { .. } => track_inputs.push(Vec::new()),
                SequenceVideoSegment::Clip {
                    media_id,
                    source_start,
                    duration,
                    ..
                } => {
                    append_media_input(
                        &mut arguments,
                        project,
                        *media_id,
                        *source_start,
                        *duration,
                    )?;
                    track_inputs.push(vec![next_input]);
                    next_input = next_input.saturating_add(1);
                }
                SequenceVideoSegment::Dissolve {
                    outgoing_media,
                    incoming_media,
                    outgoing_source_start,
                    incoming_source_start,
                    duration,
                    ..
                } => {
                    append_media_input(
                        &mut arguments,
                        project,
                        *outgoing_media,
                        *outgoing_source_start,
                        *duration,
                    )?;
                    let outgoing_input = next_input;
                    next_input = next_input.saturating_add(1);
                    append_media_input(
                        &mut arguments,
                        project,
                        *incoming_media,
                        *incoming_source_start,
                        *duration,
                    )?;
                    track_inputs.push(vec![outgoing_input, next_input]);
                    next_input = next_input.saturating_add(1);
                }
            }
        }
        video_inputs.push(track_inputs);
    }
    let mut audio_inputs = Vec::new();
    for audio in &plan.audio_tracks {
        let mut inputs = Vec::new();
        for segment in &audio.plan.segments {
            match segment {
                SequenceExportSegment::Gap { .. } => inputs.push(None),
                SequenceExportSegment::Clip {
                    media_id,
                    source_start,
                    duration,
                    ..
                } => {
                    append_media_input(
                        &mut arguments,
                        project,
                        *media_id,
                        *source_start,
                        *duration,
                    )?;
                    inputs.push(Some(next_input));
                    next_input = next_input.saturating_add(1);
                }
            }
        }
        audio_inputs.push(inputs);
    }
    let filter = build_filter(plan, request, &video_inputs, &audio_inputs)?;
    arguments.extend([
        OsString::from("-filter_complex"),
        OsString::from(filter),
        OsString::from("-map"),
        OsString::from("[outv]"),
    ]);
    if !plan.audio_tracks.is_empty() {
        arguments.extend([OsString::from("-map"), OsString::from("[outa]")]);
    }
    arguments.extend([
        OsString::from("-r"),
        OsString::from(rate(request.frame_rate)),
        OsString::from("-c:v"),
        OsString::from("libx264"),
        OsString::from("-profile:v"),
        OsString::from("high"),
        OsString::from("-pix_fmt"),
        OsString::from("yuv420p"),
        OsString::from("-b:v"),
        OsString::from(format!("{}k", request.video_bitrate_kbps)),
    ]);
    if !plan.audio_tracks.is_empty() {
        arguments.extend([
            OsString::from("-c:a"),
            OsString::from("aac"),
            OsString::from("-b:a"),
            OsString::from(format!("{}k", request.audio_bitrate_kbps)),
        ]);
    }
    arguments.extend([
        OsString::from("-movflags"),
        OsString::from("+faststart"),
        OsString::from("-f"),
        OsString::from("mp4"),
        OsString::from("-y"),
        output.as_os_str().to_owned(),
    ]);
    Ok(arguments)
}

fn append_media_input(
    arguments: &mut Vec<OsString>,
    project: &Project,
    media_id: MediaAssetId,
    source_start: RationalTime,
    duration: RationalTime,
) -> Result<(), ExportError> {
    let source = project
        .media
        .get(&media_id)
        .ok_or(ExportError::MissingMedia)?;
    let path = PathBuf::from(&source.source_path);
    if !path.is_file() {
        return Err(ExportError::InvalidSource(path));
    }
    arguments.extend([
        OsString::from("-ss"),
        OsString::from(format_timestamp(source_start)?),
        OsString::from("-t"),
        OsString::from(format_timestamp(duration)?),
        OsString::from("-i"),
        path.into_os_string(),
    ]);
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn build_filter(
    plan: &SequenceExportPlan,
    request: &SequenceMp4ExportRequest,
    video_inputs: &[Vec<Vec<usize>>],
    audio_inputs: &[Vec<Option<usize>>],
) -> Result<String, ExportError> {
    let mut filter = String::new();
    let dimensions = format!("{}:{}", request.width, request.height);
    let color_dimensions = format!("{}x{}", request.width, request.height);
    let frame_rate = rate(request.frame_rate);
    for (track_index, track) in plan.video_tracks.iter().enumerate() {
        for (segment_index, segment) in track.segments.iter().enumerate() {
            let label = format!("v{track_index}_{segment_index}");
            match segment {
                SequenceVideoSegment::Gap { duration, .. } => {
                    let _ = write!(
                        filter,
                        "color=c=black@0.0:s={color_dimensions}:r={frame_rate}:d={},format=rgba[{label}];",
                        format_timestamp(*duration)?
                    );
                }
                SequenceVideoSegment::Clip { .. } => {
                    let input = video_inputs[track_index][segment_index][0];
                    let _ = write!(
                        filter,
                        "[{input}:v:0]fps={frame_rate}:round=near,scale={dimensions}:force_original_aspect_ratio=decrease,pad={dimensions}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=rgba,setpts=PTS-STARTPTS[{label}];"
                    );
                }
                SequenceVideoSegment::Dissolve { duration, .. } => {
                    let outgoing = video_inputs[track_index][segment_index][0];
                    let incoming = video_inputs[track_index][segment_index][1];
                    let _ = write!(
                        filter,
                        "[{outgoing}:v:0]fps={frame_rate}:round=near,scale={dimensions}:force_original_aspect_ratio=decrease,pad={dimensions}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,setpts=PTS-STARTPTS[vo{track_index}_{segment_index}];[{incoming}:v:0]fps={frame_rate}:round=near,scale={dimensions}:force_original_aspect_ratio=decrease,pad={dimensions}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,setpts=PTS-STARTPTS[vi{track_index}_{segment_index}];[vo{track_index}_{segment_index}][vi{track_index}_{segment_index}]xfade=transition=fade:duration={}:offset=0,format=rgba[{label}];",
                        format_timestamp(*duration)?
                    );
                }
            }
        }
        for segment_index in 0..track.segments.len() {
            let _ = write!(filter, "[v{track_index}_{segment_index}]");
        }
        let _ = write!(
            filter,
            "concat=n={}:v=1:a=0[vt{track_index}];",
            track.segments.len()
        );
    }
    let _ = write!(
        filter,
        "color=c=black:s={color_dimensions}:r={frame_rate}:d={}[base];",
        format_timestamp(plan.duration)?
    );
    let mut composed = "base".to_owned();
    for track_index in 0..plan.video_tracks.len() {
        let next = format!("comp{track_index}");
        let _ = write!(
            filter,
            "[{composed}][vt{track_index}]overlay=shortest=1:format=auto[{next}];"
        );
        composed = next;
    }
    let _ = write!(filter, "[{composed}]format=yuv420p[outv]");

    if !plan.audio_tracks.is_empty() {
        filter.push(';');
        for (track_index, audio) in plan.audio_tracks.iter().enumerate() {
            for (segment_index, segment) in audio.plan.segments.iter().enumerate() {
                match (segment, audio_inputs[track_index][segment_index]) {
                    (SequenceExportSegment::Gap { duration, .. }, None) => {
                        let _ = write!(
                            filter,
                            "anullsrc=r=48000:cl=stereo:d={}[a{track_index}_{segment_index}];",
                            format_timestamp(*duration)?
                        );
                    }
                    (SequenceExportSegment::Clip { .. }, Some(input)) => {
                        let _ = write!(
                            filter,
                            "[{input}:a:0]aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo,asetpts=PTS-STARTPTS[a{track_index}_{segment_index}];"
                        );
                    }
                    _ => return Err(ExportError::InvalidTrackLayout),
                }
            }
            for segment_index in 0..audio.plan.segments.len() {
                let _ = write!(filter, "[a{track_index}_{segment_index}]");
            }
            let _ = write!(
                filter,
                "concat=n={}:v=0:a=1,apad=whole_dur={},atrim=duration={}[at{track_index}];",
                audio.plan.segments.len(),
                format_timestamp(plan.duration)?,
                format_timestamp(plan.duration)?
            );
        }
        let (left, right) = StereoGainPan::new().channel_gains();
        if (left - right).abs() > f32::EPSILON {
            return Err(ExportError::InvalidTrackLayout);
        }
        for track_index in 0..plan.audio_tracks.len() {
            let _ = write!(
                filter,
                "[at{track_index}]volume={left:.9}[am{track_index}];"
            );
        }
        for track_index in 0..plan.audio_tracks.len() {
            let _ = write!(filter, "[am{track_index}]");
        }
        let _ = write!(
            filter,
            "amix=inputs={}:normalize=0:dropout_transition=0[outa]",
            plan.audio_tracks.len()
        );
    }
    Ok(filter)
}

fn rate(frame_rate: RationalTime) -> String {
    format!("{}/{}", frame_rate.value(), frame_rate.timescale())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prism_core::{MediaAssetId, SequenceId, TrackId, TransitionId};
    use prism_project::{
        Clip, MediaAsset, Sequence, Track, Transition, TransitionAlignment, TransitionKind,
    };

    #[test]
    fn plans_centered_dissolve_handles_and_multitrack_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let lower_id = TrackId::new();
        let upper_id = TrackId::new();
        let outgoing = ClipId::new();
        let incoming = ClipId::new();
        let mut project = Project::new("Sequence");
        project.media.insert(
            media_id,
            MediaAsset {
                id: media_id,
                source_path: "/fixture.mp4".to_owned(),
                proxy_path: None,
                proxy_source_fingerprint: None,
            },
        );
        let clip = |id, start, source_start, duration| Clip {
            id,
            media_id,
            timeline_start: RationalTime::from_integer(start),
            source_start: RationalTime::from_integer(source_start),
            source_duration: RationalTime::from_integer(duration),
            transform: prism_project::ClipTransform::default(),
        };
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: vec![Transition {
                    id: TransitionId::new(),
                    outgoing_clip: outgoing,
                    incoming_clip: incoming,
                    duration: RationalTime::new(2, 1)?,
                    alignment: TransitionAlignment::Centered,
                    kind: TransitionKind::Dissolve,
                }],
                tracks: vec![
                    Track {
                        id: lower_id,
                        kind: TrackKind::Video,
                        clips: vec![clip(outgoing, 0, 0, 4), clip(incoming, 4, 5, 4)],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                    Track {
                        id: upper_id,
                        kind: TrackKind::Video,
                        clips: vec![clip(ClipId::new(), 7, 0, 2)],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                ],
            },
        );
        project.validate()?;
        let plan = plan_sequence_export(&project, sequence_id)?;
        assert_eq!(plan.duration, RationalTime::new(9, 1)?);
        assert_eq!(plan.video_tracks.len(), 2);
        assert!(matches!(
            plan.video_tracks[0].segments[1],
            SequenceVideoSegment::Dissolve {
                outgoing_source_start,
                incoming_source_start,
                duration,
                ..
            } if outgoing_source_start == RationalTime::new(3, 1)?
                && incoming_source_start == RationalTime::new(4, 1)?
                && duration == RationalTime::new(2, 1)?
        ));
        Ok(())
    }
}
