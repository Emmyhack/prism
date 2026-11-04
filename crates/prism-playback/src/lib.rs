//! UI-independent timeline evaluation for preview and export.
//!
//! This crate resolves authoritative project edits into source-frame requests.
//! Decoding and compositing consume this plan but do not redefine edit timing.

use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_project::{Project, TrackKind};
use prism_timeline::{RationalTime, TimeError};
use thiserror::Error;

/// A visible source selected for one sequence timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoSourceRequest {
    /// Track containing the selected clip.
    pub track_id: TrackId,
    /// Selected clip.
    pub clip_id: ClipId,
    /// Referenced media asset.
    pub media_id: MediaAssetId,
    /// Exact timestamp to request from the source decoder.
    pub source_time: RationalTime,
}

/// Evaluation result in the sequence's stored video-track order.
///
/// Layer ordering and blend semantics will be finalized by the compositor. The
/// planner deliberately preserves authoritative track order without reversing
/// it or assigning an undocumented top/bottom interpretation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoFramePlan {
    /// Requested sequence timestamp.
    pub sequence_time: RationalTime,
    /// At most one visible source per video track, in stored track order.
    pub sources: Vec<VideoSourceRequest>,
}

/// Timeline evaluation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum EvaluationError {
    /// The requested sequence does not exist in the project.
    #[error("sequence does not exist")]
    SequenceNotFound,
    /// Sequence evaluation does not accept timestamps before zero.
    #[error("sequence timestamp must not be negative")]
    NegativeTimestamp,
    /// Exact range or source-time arithmetic failed.
    #[error("timeline arithmetic failed: {0}")]
    Time(#[from] TimeError),
}

/// Resolves active video clips at an exact sequence timestamp.
///
/// Clip ranges are start-inclusive and end-exclusive. Empty gaps produce no
/// request for that track. Audio tracks are ignored by this video planner.
///
/// # Errors
///
/// Returns [`EvaluationError`] for a missing sequence, negative timestamp, or
/// unrepresentable exact-time arithmetic.
pub fn evaluate_video_frame(
    project: &Project,
    sequence_id: SequenceId,
    sequence_time: RationalTime,
) -> Result<VideoFramePlan, EvaluationError> {
    if !sequence_time.is_non_negative() {
        return Err(EvaluationError::NegativeTimestamp);
    }
    let sequence = project
        .sequences
        .get(&sequence_id)
        .ok_or(EvaluationError::SequenceNotFound)?;
    let mut sources = Vec::new();
    for track in &sequence.tracks {
        if track.kind != TrackKind::Video {
            continue;
        }
        for clip in &track.clips {
            if sequence_time < clip.timeline_start {
                break;
            }
            let timeline_end = clip.timeline_start.checked_add(clip.source_duration)?;
            if sequence_time >= timeline_end {
                continue;
            }
            let offset = sequence_time.checked_sub(clip.timeline_start)?;
            sources.push(VideoSourceRequest {
                track_id: track.id,
                clip_id: clip.id,
                media_id: clip.media_id,
                source_time: clip.source_start.checked_add(offset)?,
            });
            break;
        }
    }
    Ok(VideoFramePlan {
        sequence_time,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::{EvaluationError, evaluate_video_frame};
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind};
    use prism_timeline::RationalTime;

    struct Fixture {
        project: Project,
        sequence_id: SequenceId,
        video_clip_id: ClipId,
        second_video_clip_id: ClipId,
        video_media_id: MediaAssetId,
    }

    fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
        let mut project = Project::new("Evaluation");
        let sequence_id = SequenceId::new();
        let video_media_id = MediaAssetId::new();
        let audio_media_id = MediaAssetId::new();
        project.media.insert(
            video_media_id,
            MediaAsset {
                id: video_media_id,
                source_path: "video.mp4".to_owned(),
            },
        );
        project.media.insert(
            audio_media_id,
            MediaAsset {
                id: audio_media_id,
                source_path: "audio.wav".to_owned(),
            },
        );
        let video_clip_id = ClipId::new();
        let second_video_clip_id = ClipId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                tracks: vec![
                    Track {
                        id: TrackId::new(),
                        kind: TrackKind::Video,
                        clips: vec![
                            Clip {
                                id: video_clip_id,
                                media_id: video_media_id,
                                timeline_start: RationalTime::new(1, 1)?,
                                source_start: RationalTime::new(10, 1)?,
                                source_duration: RationalTime::new(2, 1)?,
                            },
                            Clip {
                                id: second_video_clip_id,
                                media_id: video_media_id,
                                timeline_start: RationalTime::new(3, 1)?,
                                source_start: RationalTime::new(20, 1)?,
                                source_duration: RationalTime::new(1, 1)?,
                            },
                        ],
                        locked: false,
                    },
                    Track {
                        id: TrackId::new(),
                        kind: TrackKind::Audio,
                        clips: vec![Clip {
                            id: ClipId::new(),
                            media_id: audio_media_id,
                            timeline_start: RationalTime::ZERO,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(30, 1)?,
                        }],
                        locked: false,
                    },
                ],
            },
        );
        project.validate()?;
        Ok(Fixture {
            project,
            sequence_id,
            video_clip_id,
            second_video_clip_id,
            video_media_id,
        })
    }

    #[test]
    fn maps_sequence_time_to_exact_source_time() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture()?;
        let plan = evaluate_video_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(3, 2)?,
        )?;
        assert_eq!(plan.sources.len(), 1);
        assert_eq!(plan.sources[0].clip_id, fixture.video_clip_id);
        assert_eq!(plan.sources[0].media_id, fixture.video_media_id);
        assert_eq!(plan.sources[0].source_time, RationalTime::new(21, 2)?);
        Ok(())
    }

    #[test]
    fn clip_end_is_exclusive_and_next_start_is_inclusive() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture()?;
        let plan = evaluate_video_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(3, 1)?,
        )?;
        assert_eq!(plan.sources.len(), 1);
        assert_eq!(plan.sources[0].clip_id, fixture.second_video_clip_id);
        assert_eq!(plan.sources[0].source_time, RationalTime::new(20, 1)?);
        Ok(())
    }

    #[test]
    fn gaps_and_audio_tracks_do_not_produce_video_sources() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture()?;
        let plan = evaluate_video_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(1, 2)?,
        )?;
        assert!(plan.sources.is_empty());
        Ok(())
    }

    #[test]
    fn rejects_negative_time_and_missing_sequence() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture()?;
        assert_eq!(
            evaluate_video_frame(
                &fixture.project,
                fixture.sequence_id,
                RationalTime::new(-1, 1)?,
            ),
            Err(EvaluationError::NegativeTimestamp)
        );
        assert_eq!(
            evaluate_video_frame(&fixture.project, SequenceId::new(), RationalTime::ZERO),
            Err(EvaluationError::SequenceNotFound)
        );
        Ok(())
    }
}
