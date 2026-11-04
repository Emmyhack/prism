//! Opt-in evaluation-to-decode verification against the generated fixture.

use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_media::FfmpegReferenceDecoder;
use prism_playback::evaluate_video_frame;
use prism_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind};
use prism_timeline::RationalTime;
use std::path::PathBuf;

#[test]
#[ignore = "requires PRISM_FFMPEG and the generated media fixture"]
fn timeline_trim_and_placement_select_expected_source_frame()
-> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?;
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let mut project = Project::new("Preview integration");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    project.media.insert(
        media_id,
        MediaAsset {
            id: media_id,
            source_path: fixture.to_string_lossy().into_owned(),
        },
    );
    project.sequences.insert(
        sequence_id,
        Sequence {
            id: sequence_id,
            name: "Main".to_owned(),
            tracks: vec![Track {
                id: TrackId::new(),
                kind: TrackKind::Video,
                clips: vec![Clip {
                    id: ClipId::new(),
                    media_id,
                    timeline_start: RationalTime::new(1, 4)?,
                    source_start: RationalTime::new(1, 4)?,
                    source_duration: RationalTime::new(3, 4)?,
                }],
                locked: false,
            }],
        },
    );
    project.validate()?;

    let plan = evaluate_video_frame(&project, sequence_id, RationalTime::new(1, 2)?)?;
    assert_eq!(plan.sources.len(), 1);
    assert_eq!(plan.sources[0].source_time, RationalTime::new(1, 2)?);
    let source_path = PathBuf::from(&project.media[&plan.sources[0].media_id].source_path);
    let frame = FfmpegReferenceDecoder::new(ffmpeg).decode_video_frame_rgba(
        &source_path,
        0,
        plan.sources[0].source_time,
        320,
        180,
    )?;
    assert_eq!(
        frame.sha256(),
        "425a3107ba67e8586bdb38737176a7a916f9994dbb4966497c2311abf24f6001"
    );
    Ok(())
}
