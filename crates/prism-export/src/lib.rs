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
    fs,
    path::{Path, PathBuf},
    process::Command,
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

/// Basic export failures.
#[derive(Debug, Error)]
pub enum ExportError {
    /// Requested sequence was absent from the project.
    #[error("export sequence does not exist")]
    SequenceNotFound,
    /// Requested track was absent from the selected sequence.
    #[error("export track does not exist in the sequence")]
    TrackNotFound,
    /// The initial sequence exporter only accepts video tracks.
    #[error("export track is not a video track")]
    TrackIsNotVideo,
    /// Track clips were overlapping, out of order, or otherwise invalid.
    #[error("export track layout is invalid")]
    InvalidTrackLayout,
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
        validate_request(request)?;
        let temporary = temporary_output_path(&request.output)?;
        let guard = TemporaryExport::new(temporary);
        let start = format_timestamp(request.source_start)?;
        let duration = format_timestamp(request.duration)?;
        let output = Command::new(&self.ffmpeg)
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
            .arg(guard.path())
            .output()?;
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
}

fn validate_request(request: &BasicMp4ExportRequest) -> Result<(), ExportError> {
    if !request.source.is_file() {
        return Err(ExportError::InvalidSource(request.source.clone()));
    }
    if !request.source_start.is_non_negative() || !request.duration.is_positive() {
        return Err(ExportError::InvalidRange);
    }
    if request.output.exists() {
        return Err(ExportError::OutputExists(request.output.clone()));
    }
    let parent = request
        .output
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| ExportError::InvalidOutputParent(request.output.clone()))?;
    if parent.as_os_str().is_empty() {
        return Err(ExportError::InvalidOutputParent(request.output.clone()));
    }
    Ok(())
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
        BasicMp4ExportRequest, ExportError, SequenceExportSegment, format_timestamp,
        plan_single_video_track, validate_request,
    };
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind};
    use prism_timeline::RationalTime;
    use std::path::PathBuf;

    #[test]
    fn timestamp_format_uses_exact_named_rounding() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            format_timestamp(RationalTime::new(1_001, 30_000)?)?,
            "0.033366667"
        );
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
                tracks: vec![Track {
                    id: track_id,
                    kind: TrackKind::Audio,
                    clips: Vec::new(),
                    locked: false,
                }],
            },
        );
        assert!(matches!(
            plan_single_video_track(&project, sequence_id, track_id),
            Err(ExportError::TrackIsNotVideo)
        ));
    }
}
