use super::{
    BasicMp4ExportRequest, ExportCancellation, ExportError, ExportJobState, ExportPreset,
    ExportQueue, ExportQueueError, SequenceExportSegment, SequenceMp4ExportRequest,
    SingleTrackMp4ExportRequest, VideoAudioMp4ExportRequest, build_audio_concat_filter,
    build_video_audio_ffmpeg_arguments, format_timestamp, plan_single_audio_track,
    plan_single_video_track, validate_request,
};
use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_project::{
    Clip, MediaAsset, Project, Sequence, Track, TrackKind, Transition, TransitionKind,
};
use prism_timeline::RationalTime;
use std::{fs, path::PathBuf};

#[test]
fn export_queue_enforces_monotonic_real_progress() -> Result<(), ExportQueueError> {
    let mut queue = ExportQueue::new();
    let id = queue.enqueue(PathBuf::from("deliver.mp4"));
    assert_eq!(
        queue.get(id).map(|job| &job.state),
        Some(&ExportJobState::Queued)
    );
    queue.start(id)?;
    queue.report_progress(id, 40)?;
    assert_eq!(
        queue.report_progress(id, 39),
        Err(ExportQueueError::NonMonotonicProgress)
    );
    queue.report_progress(id, 100)?;
    queue.complete(id)?;
    assert_eq!(
        queue.get(id).map(|job| &job.state),
        Some(&ExportJobState::Completed)
    );
    assert_eq!(queue.cancel(id), Err(ExportQueueError::InvalidTransition));
    Ok(())
}

#[test]
fn export_queue_cancellation_and_failure_are_terminal() -> Result<(), ExportQueueError> {
    let mut queue = ExportQueue::new();
    let cancelled = queue.enqueue(PathBuf::from("cancelled.mp4"));
    queue.cancel(cancelled)?;
    assert_eq!(
        queue.start(cancelled),
        Err(ExportQueueError::InvalidTransition)
    );

    let failed = queue.enqueue(PathBuf::from("failed.mp4"));
    queue.start(failed)?;
    queue.fail(failed, "encoder unavailable")?;
    assert_eq!(
        queue.report_progress(failed, 1),
        Err(ExportQueueError::InvalidTransition)
    );
    assert!(matches!(
        queue.get(failed).map(|job| &job.state),
        Some(ExportJobState::Failed { message }) if message == "encoder unavailable"
    ));
    queue.retry(failed)?;
    assert_eq!(
        queue.get(failed).map(|job| &job.state),
        Some(&ExportJobState::Queued)
    );
    assert_eq!(
        queue.remove(failed),
        Err(ExportQueueError::InvalidTransition)
    );
    queue.cancel(failed)?;
    let removed = queue.remove(failed)?;
    assert_eq!(removed.id, failed);
    assert_eq!(queue.get(failed), None);
    Ok(())
}

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
    let audio_request =
        VideoAudioMp4ExportRequest::from_preset(ExportPreset::Youtube4k, PathBuf::from("4k.mp4"));
    assert_eq!((audio_request.width, audio_request.height), (3840, 2160));
    assert_eq!(audio_request.video_bitrate_kbps, 35_000);
    assert_eq!(
        ExportPreset::parse("vertical-1080x1920")?.settings().0,
        1080
    );
    assert_eq!(ExportPreset::parse("square-1080x1080")?.settings().1, 1080);
    let source_match = SequenceMp4ExportRequest::source_match(
        720,
        480,
        RationalTime::new(24_000, 1_001)?,
        5_000,
        PathBuf::from("source-match.mp4"),
    );
    assert_eq!((source_match.width, source_match.height), (720, 480));
    let custom = SequenceMp4ExportRequest::custom(
        640,
        360,
        RationalTime::new(25, 1)?,
        2_500,
        128,
        PathBuf::from("custom.mp4"),
    );
    assert_eq!(custom.audio_bitrate_kbps, 128);
    assert!(matches!(
        ExportPreset::parse("unknown"),
        Err(ExportError::UnknownPreset(name)) if name == "unknown"
    ));
    Ok(())
}

#[test]
fn invalid_source_is_rejected_before_encoder_launch() -> Result<(), Box<dyn std::error::Error>> {
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
fn pre_cancelled_track_export_does_not_launch_encoder() -> Result<(), Box<dyn std::error::Error>> {
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let mut project = Project::new("Cancelled track");
    project.media.insert(
        media_id,
        MediaAsset {
            id: media_id,
            source_path: "missing.mp4".to_owned(),
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
                clips: vec![Clip {
                    id: ClipId::new(),
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: RationalTime::new(1, 1)?,
                    transform: prism_project::ClipTransform::default(),
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
fn sequence_plan_preserves_trims_and_makes_gaps_explicit() -> Result<(), Box<dyn std::error::Error>>
{
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
                        id: first_clip_id,
                        media_id,
                        timeline_start: RationalTime::new(1, 1)?,
                        source_start: RationalTime::new(10, 1)?,
                        source_duration: RationalTime::new(2, 1)?,
                        transform: prism_project::ClipTransform::default(),
                    },
                    Clip {
                        id: second_clip_id,
                        media_id,
                        timeline_start: RationalTime::new(4, 1)?,
                        source_start: RationalTime::new(20, 1)?,
                        source_duration: RationalTime::new(1, 1)?,
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
fn sequence_plan_refuses_to_silently_drop_transitions() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Transition export");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let outgoing = ClipId::new();
    let incoming = ClipId::new();
    project.media.insert(
        media_id,
        MediaAsset {
            id: media_id,
            source_path: "source.mp4".to_owned(),
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
            transitions: vec![Transition {
                id: TransitionId::new(),
                outgoing_clip: outgoing,
                incoming_clip: incoming,
                duration: RationalTime::new(1, 2)?,
                alignment: prism_project::TransitionAlignment::Centered,
                kind: TransitionKind::Dissolve,
            }],
            tracks: vec![Track {
                id: track_id,
                kind: TrackKind::Video,
                clips: vec![
                    Clip {
                        id: outgoing,
                        media_id,
                        timeline_start: RationalTime::ZERO,
                        source_start: RationalTime::new(1, 1)?,
                        source_duration: RationalTime::new(2, 1)?,
                        transform: prism_project::ClipTransform::default(),
                    },
                    Clip {
                        id: incoming,
                        media_id,
                        timeline_start: RationalTime::new(2, 1)?,
                        source_start: RationalTime::new(4, 1)?,
                        source_duration: RationalTime::new(2, 1)?,
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
    assert!(matches!(
        plan_single_video_track(&project, sequence_id, track_id),
        Err(ExportError::TransitionSourceHandlesRequired)
    ));
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
fn audio_sequence_plan_preserves_trims_and_silence_gaps() -> Result<(), Box<dyn std::error::Error>>
{
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
                kind: TrackKind::Audio,
                clips: vec![Clip {
                    id: clip_id,
                    media_id,
                    timeline_start: RationalTime::new(2, 1)?,
                    source_start: RationalTime::new(7, 1)?,
                    source_duration: RationalTime::new(3, 2)?,
                    transform: prism_project::ClipTransform::default(),
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
fn audio_filter_labels_inputs_and_concatenates_segments() -> Result<(), Box<dyn std::error::Error>>
{
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
                Track {
                    id: video_track_id,
                    kind: TrackKind::Video,
                    clips: vec![Clip {
                        id: ClipId::new(),
                        media_id,
                        timeline_start: RationalTime::ZERO,
                        source_start: RationalTime::ZERO,
                        source_duration: RationalTime::new(2, 1)?,
                        transform: prism_project::ClipTransform::default(),
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
                        transform: prism_project::ClipTransform::default(),
                    }],
                    locked: false,
                    visible: true,
                    muted: false,
                },
            ],
        },
    );
    let directory = std::env::temp_dir().join(format!("prism-av-plan-{}", uuid::Uuid::new_v4()));
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
fn combined_export_arguments_map_video_and_audio_filters() -> Result<(), Box<dyn std::error::Error>>
{
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let video_track_id = TrackId::new();
    let audio_track_id = TrackId::new();
    let directory = std::env::temp_dir().join(format!("prism-av-args-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    let source = directory.join("source.mp4");
    fs::write(&source, b"fixture")?;
    let mut project = Project::new("A/V arguments");
    project.media.insert(
        media_id,
        MediaAsset {
            id: media_id,
            source_path: source.display().to_string(),
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
                Track {
                    id: video_track_id,
                    kind: TrackKind::Video,
                    clips: vec![Clip {
                        id: ClipId::new(),
                        media_id,
                        timeline_start: RationalTime::ZERO,
                        source_start: RationalTime::ZERO,
                        source_duration: RationalTime::new(1, 1)?,
                        transform: prism_project::ClipTransform::default(),
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
                        transform: prism_project::ClipTransform::default(),
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
    assert!(joined.contains("-c:v libx264"));
    assert!(joined.contains("-profile:v high"));
    assert!(joined.contains("-pix_fmt yuv420p"));
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
