//! Opt-in basic MP4 export verification against the generated fixture.

use prism_audio::{AudioAnalysis, measure_integrated_loudness};
use prism_commands::{Command, CommandEngine};
use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_export::{
    BasicMp4ExportRequest, BasicMp4Exporter, SequenceMp4ExportRequest, SingleTrackMp4ExportRequest,
    VideoAudioMp4ExportRequest, plan_single_video_track,
};
use prism_media::{FfmpegReferenceDecoder, FfprobeInspector, StreamKind};
use prism_persistence::{load, save_atomic};
use prism_project::{
    Clip, MediaAsset, Project, Sequence, Track, TrackKind, Transition, TransitionAlignment,
    TransitionKind,
};
use prism_timeline::RationalTime;
use std::{fs, path::PathBuf};
use uuid::Uuid;

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn exports_validated_source_range_and_decodes_result() -> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = PathBuf::from(std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?);
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory = std::env::temp_dir().join(format!("prism-export-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let output = test_directory.join("range.mp4");
    let request = BasicMp4ExportRequest {
        source: fixture,
        source_start: RationalTime::new(1, 4)?,
        duration: RationalTime::new(1, 2)?,
        output: output.clone(),
    };
    let report = BasicMp4Exporter::new(&ffmpeg, &ffprobe).export(&request)?;
    assert_eq!(report.output, output);
    assert!(report.byte_length > 0);
    assert!(report.has_video);
    assert!(report.has_audio);

    let independently_probed = FfprobeInspector::new(&ffprobe).inspect(&output)?;
    assert!(
        independently_probed
            .duration
            .is_some_and(RationalTime::is_positive)
    );
    let video = independently_probed
        .streams
        .iter()
        .find_map(|stream| match &stream.kind {
            StreamKind::Video(video) => Some(video),
            StreamKind::Audio(_) | StreamKind::Other { .. } => None,
        })
        .ok_or("exported video stream was missing")?;
    assert_eq!(video.width, Some(320));
    assert_eq!(video.height, Some(180));
    let decoded = FfmpegReferenceDecoder::new(&ffmpeg).decode_video_frame_rgba(
        &output,
        0,
        RationalTime::new(1, 4)?,
        320,
        180,
    )?;
    assert_eq!(decoded.rgba.len(), 320 * 180 * 4);

    fs::remove_file(&output)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn renders_single_track_clips_and_black_gaps() -> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = PathBuf::from(std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?);
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory =
        std::env::temp_dir().join(format!("prism-sequence-export-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let output = test_directory.join("sequence.mp4");
    let mut project = Project::new("Sequence export");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    project.media.insert(
        media_id,
        MediaAsset {
            id: media_id,
            source_path: fixture.to_string_lossy().into_owned(),
            proxy_path: None,
            proxy_source_fingerprint: None,
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
                        id: ClipId::new(),
                        media_id,
                        timeline_start: RationalTime::new(1, 5)?,
                        source_start: RationalTime::ZERO,
                        source_duration: RationalTime::new(1, 5)?,
                        transform: prism_project::ClipTransform::default(),
                    },
                    Clip {
                        id: ClipId::new(),
                        media_id,
                        timeline_start: RationalTime::new(3, 5)?,
                        source_start: RationalTime::new(1, 2)?,
                        source_duration: RationalTime::new(1, 5)?,
                        transform: prism_project::ClipTransform::default(),
                    },
                ],
                locked: false,
                visible: true,
                muted: false,
            }],
        },
    );
    project.validate()?;
    let report = BasicMp4Exporter::new(&ffmpeg, &ffprobe).export_single_video_track(
        &project,
        sequence_id,
        track_id,
        &SingleTrackMp4ExportRequest {
            width: 320,
            height: 180,
            frame_rate: RationalTime::new(30, 1)?,
            video_bitrate_kbps: 1_000,
            output: output.clone(),
        },
    )?;
    assert!(report.has_video);
    assert!(!report.has_audio);
    assert!(report.duration >= RationalTime::new(4, 5)?);
    assert!(report.duration <= RationalTime::new(5, 6)?);

    let decoder = FfmpegReferenceDecoder::new(&ffmpeg);
    let gap = decoder.decode_video_frame_rgba(&output, 0, RationalTime::new(1, 10)?, 320, 180)?;
    let clip = decoder.decode_video_frame_rgba(&output, 0, RationalTime::new(3, 10)?, 320, 180)?;
    assert_ne!(gap.sha256(), clip.sha256());

    fs::remove_file(&output)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn muxes_synchronized_video_and_audio_tracks() -> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = PathBuf::from(std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?);
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory =
        std::env::temp_dir().join(format!("prism-av-export-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let output = test_directory.join("combined.mp4");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let video_track_id = TrackId::new();
    let audio_track_id = TrackId::new();
    let source_start = RationalTime::new(1, 10)?;
    let source_duration = RationalTime::new(1, 2)?;
    let clip = |kind, track_id| Track {
        id: track_id,
        kind,
        clips: vec![Clip {
            id: ClipId::new(),
            media_id,
            timeline_start: RationalTime::ZERO,
            source_start,
            source_duration,
            transform: prism_project::ClipTransform::default(),
        }],
        locked: false,
        visible: true,
        muted: false,
    };
    let mut project = Project::new("Combined export");
    project.media.insert(
        media_id,
        MediaAsset {
            id: media_id,
            source_path: fixture.to_string_lossy().into_owned(),
            proxy_path: None,
            proxy_source_fingerprint: None,
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
                clip(TrackKind::Video, video_track_id),
                clip(TrackKind::Audio, audio_track_id),
            ],
        },
    );
    project.validate()?;
    let report = BasicMp4Exporter::new(ffmpeg, ffprobe.clone()).export_single_video_audio_track(
        &project,
        sequence_id,
        video_track_id,
        audio_track_id,
        &VideoAudioMp4ExportRequest {
            width: 320,
            height: 180,
            frame_rate: RationalTime::new(30, 1)?,
            video_bitrate_kbps: 1_000,
            audio_bitrate_kbps: 192,
            output: output.clone(),
        },
    )?;
    assert!(report.has_video);
    assert!(report.has_audio);
    assert!(report.byte_length > 0);
    let inspected = FfprobeInspector::new(ffprobe).inspect(&output)?;
    assert!(
        inspected
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Audio(_)))
    );
    fs::remove_file(&output)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn saved_project_reopens_with_identical_export_plan_and_timing()
-> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = PathBuf::from(std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?);
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory =
        std::env::temp_dir().join(format!("prism-reopen-export-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("workflow.prism");
    let output = test_directory.join("reopened.mp4");
    let mut project = Project::new("Saved export workflow");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Build export timeline",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: fixture.to_string_lossy().into_owned(),
            },
            Command::CreateSequence {
                id: sequence_id,
                name: "Main".to_owned(),
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
                source_start: RationalTime::ZERO,
                source_duration: RationalTime::new(1, 5)?,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: ClipId::new(),
                media_id,
                timeline_start: RationalTime::new(1, 5)?,
                source_start: RationalTime::new(1, 2)?,
                source_duration: RationalTime::new(1, 5)?,
            },
        ],
    )?;
    let original_plan = plan_single_video_track(&project, sequence_id, track_id)?;
    save_atomic(&project, &project_path)?;
    let reopened = load(&project_path)?;
    let reopened_plan = plan_single_video_track(&reopened, sequence_id, track_id)?;
    assert_eq!(reopened_plan, original_plan);
    assert_eq!(reopened.revision(), project.revision());

    let report = BasicMp4Exporter::new(ffmpeg, ffprobe).export_single_video_track(
        &reopened,
        sequence_id,
        track_id,
        &SingleTrackMp4ExportRequest {
            width: 320,
            height: 180,
            frame_rate: RationalTime::new(30, 1)?,
            video_bitrate_kbps: 1_000,
            output: output.clone(),
        },
    )?;
    assert!(report.duration >= original_plan.duration);
    assert!(
        report.duration
            <= original_plan
                .duration
                .checked_add(RationalTime::new(1, 30)?)?
    );

    fs::remove_file(&output)?;
    fs::remove_file(&project_path)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
#[allow(clippy::too_many_lines)]
fn exports_multitrack_dissolves_and_mixed_audio_with_golden_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = PathBuf::from(std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?);
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory =
        std::env::temp_dir().join(format!("prism-complete-export-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let output = test_directory.join("complete.mp4");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let video_track_id = TrackId::new();
    let overlay_track_id = TrackId::new();
    let audio_track_a = TrackId::new();
    let audio_track_b = TrackId::new();
    let clip_a = ClipId::new();
    let clip_b = ClipId::new();
    let clip_c = ClipId::new();
    let video_clip = |id, timeline_start, source_start| Clip {
        id,
        media_id,
        timeline_start,
        source_start,
        source_duration: RationalTime::new(1, 5).unwrap_or(RationalTime::ZERO),
        transform: prism_project::ClipTransform::default(),
    };
    let audio_clip = || Clip {
        id: ClipId::new(),
        media_id,
        timeline_start: RationalTime::ZERO,
        source_start: RationalTime::new(1, 10).unwrap_or(RationalTime::ZERO),
        source_duration: RationalTime::new(3, 5).unwrap_or(RationalTime::ZERO),
        transform: prism_project::ClipTransform::default(),
    };
    let mut project = Project::new("Complete sequence export");
    project.media.insert(
        media_id,
        MediaAsset {
            id: media_id,
            source_path: fixture.to_string_lossy().into_owned(),
            proxy_path: None,
            proxy_source_fingerprint: None,
        },
    );
    project.sequences.insert(
        sequence_id,
        Sequence {
            id: sequence_id,
            name: "Main".to_owned(),
            markers: Vec::new(),
            transitions: vec![
                Transition {
                    id: TransitionId::new(),
                    outgoing_clip: clip_a,
                    incoming_clip: clip_b,
                    duration: RationalTime::new(1, 10)?,
                    alignment: TransitionAlignment::Centered,
                    kind: TransitionKind::Dissolve,
                },
                Transition {
                    id: TransitionId::new(),
                    outgoing_clip: clip_b,
                    incoming_clip: clip_c,
                    duration: RationalTime::new(1, 10)?,
                    alignment: TransitionAlignment::Centered,
                    kind: TransitionKind::Dissolve,
                },
            ],
            tracks: vec![
                Track {
                    id: video_track_id,
                    kind: TrackKind::Video,
                    clips: vec![
                        video_clip(clip_a, RationalTime::ZERO, RationalTime::new(1, 10)?),
                        video_clip(clip_b, RationalTime::new(1, 5)?, RationalTime::new(2, 5)?),
                        video_clip(clip_c, RationalTime::new(2, 5)?, RationalTime::new(7, 10)?),
                    ],
                    locked: false,
                    visible: true,
                    muted: false,
                },
                Track {
                    id: overlay_track_id,
                    kind: TrackKind::Video,
                    clips: vec![video_clip(
                        ClipId::new(),
                        RationalTime::new(1, 5)?,
                        RationalTime::ZERO,
                    )],
                    locked: false,
                    visible: true,
                    muted: false,
                },
                Track {
                    id: audio_track_a,
                    kind: TrackKind::Audio,
                    clips: vec![audio_clip()],
                    locked: false,
                    visible: true,
                    muted: false,
                },
                Track {
                    id: audio_track_b,
                    kind: TrackKind::Audio,
                    clips: vec![audio_clip()],
                    locked: false,
                    visible: true,
                    muted: false,
                },
            ],
        },
    );
    project.validate()?;
    let report = BasicMp4Exporter::new(&ffmpeg, &ffprobe).export_sequence(
        &project,
        sequence_id,
        &SequenceMp4ExportRequest {
            width: 320,
            height: 180,
            frame_rate: RationalTime::new(30, 1)?,
            video_bitrate_kbps: 1_000,
            audio_bitrate_kbps: 192,
            output: output.clone(),
        },
    )?;
    assert!(report.has_video);
    assert!(report.has_audio);
    let decoder = FfmpegReferenceDecoder::new(&ffmpeg);
    let mut hashes = Vec::new();
    for time in [1_i64, 2, 3, 4, 5] {
        hashes.push(
            decoder
                .decode_video_frame_rgba(&output, 0, RationalTime::new(time, 10)?, 320, 180)?
                .sha256(),
        );
    }
    let mono = decoder.decode_audio_mono_f32(&output, 0, 48_000, 28_800)?;
    let analysis = AudioAnalysis::analyze(&mono);
    let mut stereo = Vec::with_capacity(mono.len().saturating_mul(2));
    for sample in mono {
        stereo.extend([sample, sample]);
    }
    let loudness = measure_integrated_loudness(&stereo, 48_000)?;
    assert_eq!(
        hashes,
        [
            "1f074eedf2368d896e5fd91ca01323e41aaa885b1aa0f49cad59d3cd613c0caa",
            "b0c544047ee931f6b0d1ee20960d5954aee158bd2417903c47a619dae872bf36",
            "ff20fb4bc1aef07a15f8dd4d4fc6f5c4f5680396f1a2ce970b613bb479e30b2d",
            "1c339bf57c4e7f2f34ba5b607922d98a4edc131274f26ce6c1535f64f60d01c2",
            "e6124509df1f35e8cc19c12c691f8cc83dadb92364d236cb2b67506713c9b56a",
        ]
    );
    assert!((analysis.peak - 0.184_210).abs() <= 0.000_010);
    assert!((loudness.lufs - -15.039).abs() <= 0.010);
    fs::remove_file(&output)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}
