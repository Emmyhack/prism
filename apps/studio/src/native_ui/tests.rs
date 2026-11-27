use super::{
    NativeExportStatus, PreviewWorker, ProcessCommand, TrackId, TrackKind, WaveformRequest,
    WaveformWorker, insert_media_into_project, insert_video_into_project,
};
use prism_project::Project;
use prism_studio::desktop::ExportController;
use prism_timeline::RationalTime;
use std::path::PathBuf;

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn real_video_frames_complete_under_repeated_playback_ticks()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PRISM_TEST_FFMPEG").is_none() {
        return Ok(());
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media/av-ntsc-48k.mp4");
    let worker = PreviewWorker::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut completed = 0;
    while std::time::Instant::now() < deadline {
        worker.request_playback(
            fixture.clone(),
            RationalTime::new(1, 2)?,
            RationalTime::from_integer(30),
        );
        if let Some(frame) = worker.poll_decoded() {
            let (frame, _, _, _) = frame?;
            assert!(frame.width > 0 && frame.height > 0);
            completed += 1;
            if completed == 2 {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(completed, 2, "Playback timer starved video decoding");
    Ok(())
}

#[test]
#[ignore = "requires PRISM_TASK0_LONG_MEDIA and installed FFmpeg tools"]
fn task0_measures_long_media_scrub_latency() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::var_os("PRISM_TASK0_LONG_MEDIA").map(PathBuf::from) else {
        return Ok(());
    };
    let worker = PreviewWorker::new();
    let mut samples = Vec::new();
    for seconds in (0..600).step_by(60) {
        let started = std::time::Instant::now();
        worker.request(path.clone(), RationalTime::from_integer(seconds));
        let result = loop {
            if let Some(result) = worker.poll_decoded() {
                break result;
            }
            if started.elapsed() > std::time::Duration::from_secs(5) {
                return Err(format!("scrub at {seconds}s exceeded five seconds").into());
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        result?;
        samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(f64::total_cmp);
    let median = samples[samples.len() / 2];
    let maximum = samples.last().copied().ok_or("no scrub samples")?;
    eprintln!("TASK0_SCRUB_MS samples={samples:?} median={median:.3} max={maximum:.3}");
    Ok(())
}

#[test]
fn playback_waits_for_decoder_but_scrubbing_supersedes_stale_frames() {
    use std::{
        cell::{Cell, RefCell},
        sync::mpsc,
    };
    let (sender, requests) = mpsc::sync_channel(1);
    let (replies, receiver) = mpsc::sync_channel(2);
    let worker = PreviewWorker {
        sender,
        receiver,
        pending: RefCell::new(None),
        generation: Cell::new(0),
        busy: Cell::new(false),
        requested_at: Cell::new(None),
        stream_control: super::StreamControl::default(),
    };
    let path = PathBuf::from("slow-video.mp4");
    worker.request_playback(
        path.clone(),
        RationalTime::ZERO,
        RationalTime::from_integer(30),
    );
    let first = requests.recv().unwrap();
    // Simulate a decode taking longer than multiple playback timer ticks.
    for tick in 1..10 {
        worker.request_playback(
            path.clone(),
            RationalTime::new(tick, 20).unwrap(),
            RationalTime::from_integer(30),
        );
        assert_eq!(worker.generation.get(), first.generation);
    }
    assert!(requests.try_recv().is_err());
    let frame = || {
        Ok((
            prism_media::DecodedVideoFrame {
                width: 1,
                height: 1,
                rgba: vec![0, 0, 0, 255],
            },
            RationalTime::new(10, 1).unwrap(),
            None,
            false,
        ))
    };
    worker.requested_at.set(Some(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(6))
            .unwrap(),
    ));
    assert!(worker.timed_out(std::time::Duration::from_secs(5)));
    replies.send((first.generation, frame())).unwrap();
    assert!(worker.poll_decoded().unwrap().is_ok());
    assert!(!worker.timed_out(std::time::Duration::ZERO));
    assert!(!worker.busy.get());
    worker.request_playback(
        path.clone(),
        RationalTime::new(1, 1).unwrap(),
        RationalTime::from_integer(30),
    );
    let second = requests.recv().unwrap();
    worker.request(path, RationalTime::new(2, 1).unwrap());
    let scrub = requests.recv().unwrap();
    assert_ne!(second.generation, scrub.generation);
    replies.send((second.generation, frame())).unwrap();
    assert!(worker.poll_decoded().is_none());
    assert!(worker.busy.get());
    replies.send((scrub.generation, frame())).unwrap();
    assert!(worker.poll_decoded().unwrap().is_ok());
    assert!(!worker.busy.get());
}

#[test]
fn source_insert_splits_shifts_and_preserves_source_offsets()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Insert edit");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("insert.mp4"),
        RationalTime::new(4, 1)?,
        true,
    )?;
    let media = *project.media.keys().next().unwrap();
    let append = super::append_source_commands(
        &project,
        media,
        RationalTime::ZERO,
        RationalTime::new(4, 1)?,
    )?;
    prism_commands::CommandEngine::execute_transaction(&mut project, "Append", append)?;
    let commands = super::source_edit_commands(
        &project,
        media,
        RationalTime::new(1, 2)?,
        RationalTime::new(1, 1)?,
        RationalTime::new(1, 1)?,
        false,
    )?;
    let result =
        prism_commands::CommandEngine::execute_transaction(&mut project, "Insert", commands)?;
    for track in &project.sequences.values().next().unwrap().tracks {
        assert_eq!(track.clips.len(), 4);
        let ranges = track
            .clips
            .iter()
            .map(|c| (c.timeline_start, c.source_start, c.source_duration))
            .collect::<Vec<_>>();
        assert_eq!(
            ranges,
            vec![
                (
                    RationalTime::ZERO,
                    RationalTime::ZERO,
                    RationalTime::new(1, 1)?
                ),
                (
                    RationalTime::new(1, 1)?,
                    RationalTime::new(1, 2)?,
                    RationalTime::new(1, 1)?
                ),
                (
                    RationalTime::new(2, 1)?,
                    RationalTime::new(1, 1)?,
                    RationalTime::new(3, 1)?
                ),
                (
                    RationalTime::new(5, 1)?,
                    RationalTime::ZERO,
                    RationalTime::new(4, 1)?
                )
            ]
        );
    }
    result.undo.apply(&mut project)?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(|t| t.clips.len() == 2)
    );
    project.sequences.values_mut().next().unwrap().tracks[1].locked = true;
    assert!(
        super::source_edit_commands(
            &project,
            media,
            RationalTime::ZERO,
            RationalTime::new(1, 1)?,
            RationalTime::new(1, 1)?,
            false
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn source_overwrite_preserves_tails_and_later_timing() -> Result<(), Box<dyn std::error::Error>> {
    for (position, duration, expected) in [
        (1, 2, vec![(0, 0, 1), (1, 0, 2), (3, 3, 1), (4, 0, 4)]),
        (3, 2, vec![(0, 0, 3), (3, 0, 2), (5, 1, 3)]),
        (0, 8, vec![(0, 0, 8)]),
        (10, 1, vec![(0, 0, 4), (4, 0, 4), (10, 0, 1)]),
    ] {
        let mut project = Project::new("Overwrite edit");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("overwrite.mp4"),
            RationalTime::new(4, 1)?,
            true,
        )?;
        let media = *project.media.keys().next().unwrap();
        let append = super::append_source_commands(
            &project,
            media,
            RationalTime::ZERO,
            RationalTime::new(4, 1)?,
        )?;
        prism_commands::CommandEngine::execute_transaction(&mut project, "Append", append)?;
        let commands = super::source_edit_commands(
            &project,
            media,
            RationalTime::ZERO,
            RationalTime::new(duration, 1)?,
            RationalTime::new(position, 1)?,
            true,
        )?;
        let result = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Overwrite",
            commands,
        )?;
        for track in &project.sequences.values().next().unwrap().tracks {
            let ranges = track
                .clips
                .iter()
                .map(|c| (c.timeline_start, c.source_start, c.source_duration))
                .collect::<Vec<_>>();
            assert_eq!(
                ranges,
                expected
                    .iter()
                    .map(|&(a, b, c)| (
                        RationalTime::new(a, 1).unwrap(),
                        RationalTime::new(b, 1).unwrap(),
                        RationalTime::new(c, 1).unwrap()
                    ))
                    .collect::<Vec<_>>()
            );
        }
        result.undo.apply(&mut project)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|t| t.clips.len() == 2)
        );
    }
    Ok(())
}

#[test]
fn all_track_cuts_skip_locks_and_undo_atomically() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("All track cut");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("all-tracks.mp4"),
        RationalTime::new(2, 1)?,
        true,
    )?;
    let cut = RationalTime::new(1, 1)?;
    assert!(super::split_all_commands(&project, RationalTime::ZERO).is_err());
    let commands = super::split_all_commands(&project, cut)?;
    assert_eq!(commands.len(), 2);
    let original = project.clone();
    let result = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "All track cut",
        commands,
    )?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(|track| track.clips.len() == 2)
    );
    result.undo.apply(&mut project)?;
    let clips = |project: &Project| {
        project
            .sequences
            .values()
            .flat_map(|sequence| &sequence.tracks)
            .flat_map(|track| {
                track.clips.iter().map(move |clip| {
                    (
                        track.id,
                        clip.id,
                        clip.media_id,
                        clip.timeline_start,
                        clip.source_start,
                        clip.source_duration,
                    )
                })
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(clips(&project), clips(&original));
    let sequence = project.sequences.values_mut().next().unwrap();
    sequence.tracks[0].locked = true;
    sequence.tracks[1].muted = true;
    assert_eq!(super::split_all_commands(&project, cut)?.len(), 1);
    project.sequences.values_mut().next().unwrap().tracks[1].locked = true;
    assert!(super::split_all_commands(&project, cut).is_err());
    Ok(())
}

#[test]
fn program_match_uses_visible_picture_and_rejects_gaps() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Match Program");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("match-program.mp4"),
        RationalTime::new(2, 1)?,
        true,
    )?;
    assert_eq!(
        super::program_clip_index(&project, RationalTime::new(1, 1)?),
        Some(0)
    );
    assert_eq!(
        super::program_clip_index(&project, RationalTime::new(2, 1)?),
        None
    );
    project.sequences.values_mut().next().unwrap().tracks[0].visible = false;
    assert_eq!(
        super::program_clip_index(&project, RationalTime::ZERO),
        None
    );
    Ok(())
}

#[test]
fn aligned_av_ranges_validate_bounds_undo_and_overlap() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Paired range");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("source.mp4"),
        RationalTime::new(2, 1)?,
        true,
    )?;
    let range = |project: &Project, start, duration, available| {
        let timeline_start = project
            .sequences
            .values()
            .next()
            .and_then(|sequence| sequence.tracks.first()?.clips.first())
            .map_or(RationalTime::ZERO, |clip| clip.timeline_start);
        super::aligned_av_range_commands(project, 0, timeline_start, start, duration, available)
    };
    let commands = range(
        &project,
        RationalTime::new(1, 4)?,
        RationalTime::new(3, 2)?,
        RationalTime::new(2, 1)?,
    )?;
    let outcome =
        prism_commands::CommandEngine::execute_transaction(&mut project, "Paired range", commands)?;
    for track in &project.sequences.values().next().unwrap().tracks {
        assert_eq!(track.clips[0].timeline_start, RationalTime::ZERO);
        assert_eq!(track.clips[0].source_start, RationalTime::new(1, 4)?);
        assert_eq!(track.clips[0].source_duration, RationalTime::new(3, 2)?);
    }
    outcome.undo.apply(&mut project)?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(|track| track.clips[0].source_start == RationalTime::ZERO
                && track.clips[0].source_duration == RationalTime::new(2, 1).unwrap())
    );
    assert!(
        range(
            &project,
            RationalTime::new(1, 1)?,
            RationalTime::new(2, 1)?,
            RationalTime::new(2, 1)?
        )
        .is_err()
    );
    assert!(
        range(
            &project,
            RationalTime::ZERO,
            RationalTime::ZERO,
            RationalTime::new(2, 1)?
        )
        .is_err()
    );
    let sequence = project.sequences.values_mut().next().unwrap();
    let mut blocker = sequence.tracks[1].clips[0].clone();
    blocker.id = prism_core::ClipId::new();
    blocker.timeline_start = RationalTime::new(3, 1)?;
    sequence.tracks[1].clips.push(blocker);
    let revision = project.revision();
    let commands = range(
        &project,
        RationalTime::ZERO,
        RationalTime::new(4, 1)?,
        RationalTime::new(10, 1)?,
    )?;
    assert!(
        prism_commands::CommandEngine::execute_transaction(&mut project, "Blocked range", commands)
            .is_err()
    );
    assert_eq!(project.revision(), revision);
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(|track| track.clips[0].source_duration == RationalTime::new(2, 1).unwrap())
    );
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn aligned_av_edits_are_atomic_undoable_and_reject_unsafe_matches()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Paired edits");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("camera.mp4"),
        RationalTime::new(2, 1)?,
        true,
    )?;
    assert_eq!(super::aligned_av_companion_index(&project, 0), Some(1));
    assert_eq!(super::aligned_av_companion_index(&project, 1), Some(0));
    let commands =
        super::aligned_av_commands(&project, 0, "timeline.split_av", RationalTime::new(1, 1)?)?;
    assert_eq!(commands.len(), 2);
    let split = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Split aligned A/V",
        commands,
    )?;
    for track in &project.sequences.values().next().unwrap().tracks {
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[1].timeline_start, RationalTime::new(1, 1)?);
        assert_eq!(track.clips[1].source_start, RationalTime::new(1, 1)?);
    }
    split.undo.apply(&mut project)?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(|track| track.clips.len() == 1)
    );
    let revision = project.revision();
    let invalid = super::aligned_av_commands(&project, 0, "timeline.split_av", RationalTime::ZERO)?;
    assert!(
        prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Invalid pair cut",
            invalid
        )
        .is_err()
    );
    assert_eq!(project.revision(), revision);
    let commands =
        super::aligned_av_commands(&project, 1, "timeline.delete_av", RationalTime::ZERO)?;
    let deleted = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Delete aligned A/V",
        commands,
    )?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(|track| track.clips.is_empty())
    );
    deleted.undo.apply(&mut project)?;
    let commands =
        super::aligned_av_commands(&project, 0, "timeline.move_av", RationalTime::new(3, 1)?)?;
    let moved = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Move aligned A/V",
        commands,
    )?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(
                |track| track.clips[0].timeline_start == RationalTime::new(3, 1).unwrap()
                    && track.clips[0].source_start == RationalTime::ZERO
            )
    );
    moved.undo.apply(&mut project)?;
    let sequence = project.sequences.values_mut().next().unwrap();
    let mut blocker = sequence.tracks[1].clips[0].clone();
    blocker.id = prism_core::ClipId::new();
    blocker.timeline_start = RationalTime::new(3, 1)?;
    sequence.tracks[1].clips.push(blocker);
    let revision = project.revision();
    let commands =
        super::aligned_av_commands(&project, 0, "timeline.move_av", RationalTime::new(3, 1)?)?;
    assert!(
        prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Blocked pair move",
            commands
        )
        .is_err()
    );
    assert_eq!(project.revision(), revision);
    assert!(
        project
            .sequences
            .values()
            .next()
            .unwrap()
            .tracks
            .iter()
            .all(|track| track.clips[0].timeline_start == RationalTime::ZERO)
    );
    let sequence = project.sequences.values_mut().next().unwrap();
    sequence.tracks[1].clips.pop();
    sequence.tracks[1].locked = true;
    assert!(
        super::aligned_av_commands(&project, 0, "timeline.delete_av", RationalTime::ZERO)
            .unwrap_err()
            .contains("Unlock both")
    );
    let sequence = project.sequences.values_mut().next().unwrap();
    sequence.tracks[1].locked = false;
    let mut extra = sequence.tracks[1].clone();
    extra.id = TrackId::new();
    extra.clips[0].id = prism_core::ClipId::new();
    sequence.tracks.push(extra);
    assert!(
        super::aligned_av_commands(&project, 0, "timeline.delete_av", RationalTime::ZERO)
            .unwrap_err()
            .contains("Multiple")
    );
    let sequence = project.sequences.values_mut().next().unwrap();
    sequence.tracks.pop();
    sequence.tracks[1].clips[0].source_start = RationalTime::new(1, 30)?;
    assert!(
        super::aligned_av_commands(&project, 0, "timeline.delete_av", RationalTime::ZERO)
            .unwrap_err()
            .contains("No audio/video")
    );
    Ok(())
}

#[test]
fn aligned_av_ripple_delete_closes_both_tracks_atomically() -> Result<(), Box<dyn std::error::Error>>
{
    let mut project = Project::new("Paired ripple delete");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("camera.mp4"),
        RationalTime::new(2, 1)?,
        true,
    )?;
    let split_commands =
        super::aligned_av_commands(&project, 0, "timeline.split_av", RationalTime::new(1, 1)?)?;
    prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Split aligned A/V",
        split_commands,
    )?;

    let ripple_commands =
        super::aligned_av_commands(&project, 0, "timeline.ripple_delete_av", RationalTime::ZERO)?;
    assert_eq!(ripple_commands.len(), 2);
    let deleted = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Ripple delete aligned A/V",
        ripple_commands,
    )?;
    for track in &project.sequences.values().next().unwrap().tracks {
        assert_eq!(track.clips.len(), 1);
        assert_eq!(track.clips[0].timeline_start, RationalTime::ZERO);
        assert_eq!(track.clips[0].source_start, RationalTime::new(1, 1)?);
    }

    deleted.undo.apply(&mut project)?;
    for track in &project.sequences.values().next().unwrap().tracks {
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].timeline_start, RationalTime::ZERO);
        assert_eq!(track.clips[1].timeline_start, RationalTime::new(1, 1)?);
    }
    Ok(())
}

#[test]
fn audio_preview_respects_source_trims_cuts_gaps_and_mute() -> Result<(), Box<dyn std::error::Error>>
{
    let mut project = Project::new("Audio edit");
    let path = std::path::Path::new("take.mp4");
    insert_media_into_project(&mut project, path, RationalTime::new(1, 1)?, true)?;
    insert_media_into_project(
        &mut project,
        std::path::Path::new("next-take.mp4"),
        RationalTime::new(1, 1)?,
        true,
    )?;
    let audio = project
        .sequences
        .values_mut()
        .next()
        .unwrap()
        .tracks
        .iter_mut()
        .find(|track| track.kind == TrackKind::Audio)
        .unwrap();
    audio.clips[1].timeline_start = RationalTime::new(3, 1)?;
    audio.clips[1].source_start = RationalTime::new(1, 4)?;
    let second_id = audio.clips[1].id;
    let first = super::audio_preview_segment(&project, RationalTime::new(1, 2)?).unwrap();
    assert_eq!(first.source_time, RationalTime::new(1, 2)?);
    assert_eq!(first.remaining, RationalTime::new(1, 2)?);
    assert!(super::audio_preview_segment(&project, RationalTime::new(1, 1)?).is_none());
    assert!(super::audio_preview_segment(&project, RationalTime::new(2, 1)?).is_none());
    let second = super::audio_preview_segment(&project, RationalTime::new(7, 2)?).unwrap();
    assert_eq!(second.clip_id, second_id);
    assert_eq!(second.source_time, RationalTime::new(3, 4)?);
    assert_eq!(second.remaining, RationalTime::new(1, 2)?);
    assert!(super::audio_preview_segment(&project, RationalTime::new(4, 1)?).is_none());
    project
        .sequences
        .values_mut()
        .next()
        .unwrap()
        .tracks
        .iter_mut()
        .find(|track| track.kind == TrackKind::Audio)
        .unwrap()
        .muted = true;
    assert!(super::audio_preview_segment(&project, RationalTime::new(7, 2)?).is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn dropping_runtime_terminates_audio_process() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = super::NativeExportRuntime::new(None, None);
    let child = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
    let pid = child.id();
    *runtime.audio_process.borrow_mut() = Some(child);
    let decoder = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
    let decoder_pid = decoder.id();
    *runtime.audio_decoder.borrow_mut() = Some(decoder);
    drop(runtime);
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &decoder_pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()?
            .success()
    );
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()?
            .success(),
        "dropping Studio left its audio process running"
    );
    Ok(())
}

#[test]
fn matched_frame_maps_timeline_time_to_trimmed_source() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Match frame");
    insert_video_into_project(
        &mut project,
        std::path::Path::new("trimmed.mp4"),
        RationalTime::new(2, 1)?,
    )?;
    let clip = &mut project
        .sequences
        .values_mut()
        .next()
        .ok_or("Missing sequence")?
        .tracks[0]
        .clips[0];
    clip.timeline_start = RationalTime::new(3, 1)?;
    clip.source_start = RationalTime::new(7, 2)?;
    assert_eq!(
        super::matched_source(&project, 0, RationalTime::new(4, 1)?),
        Some((0, RationalTime::new(9, 2)?))
    );
    // Clip end is exclusive, as in program-frame decoding.
    assert_eq!(
        super::matched_source(&project, 0, RationalTime::new(5, 1)?),
        Some((0, RationalTime::new(7, 2)?))
    );
    assert_eq!(
        super::matched_source(&project, 0, RationalTime::ZERO),
        Some((0, RationalTime::new(7, 2)?))
    );
    assert_eq!(
        super::matched_source(&project, -1, RationalTime::ZERO),
        None
    );
    assert_eq!(super::matched_source(&project, 5, RationalTime::ZERO), None);
    Ok(())
}

#[test]
fn matched_audio_source_preserves_time_without_video_classification()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Match audio");
    super::insert_stream_media_into_project(
        &mut project,
        std::path::Path::new("dialogue.wav"),
        RationalTime::new(3, 1)?,
        false,
        true,
        None,
    )?;
    let media_id = *project.media.keys().next().ok_or("missing media")?;
    assert_eq!(
        super::media_stream_summary(&project, media_id),
        (false, true, Some(RationalTime::new(3, 1)?))
    );
    assert_eq!(
        super::matched_source(&project, 0, RationalTime::new(3, 2)?),
        Some((0, RationalTime::new(3, 2)?))
    );
    Ok(())
}

#[test]
fn edit_navigation_handles_gaps_edges_and_hidden_tracks() -> Result<(), Box<dyn std::error::Error>>
{
    let mut project = Project::new("Navigation");
    insert_video_into_project(
        &mut project,
        std::path::Path::new("first.mp4"),
        RationalTime::new(1, 1)?,
    )?;
    insert_video_into_project(
        &mut project,
        std::path::Path::new("second.mp4"),
        RationalTime::new(1, 1)?,
    )?;
    let sequence = project
        .sequences
        .values_mut()
        .next()
        .ok_or("Missing sequence")?;
    sequence.tracks[0].clips[1].timeline_start = RationalTime::new(3, 1)?;
    let time = |seconds| RationalTime::new(seconds, 1).unwrap();
    let target = |at, command| super::navigation_target(&project, time(at), command);
    assert_eq!(target(0, "playback.next_edit"), Some(time(1)));
    assert_eq!(target(1, "playback.next_edit"), Some(time(3)));
    assert_eq!(target(2, "playback.previous_edit"), Some(time(1)));
    assert_eq!(target(3, "playback.previous_edit"), Some(time(1)));
    assert_eq!(target(4, "playback.next_edit"), Some(time(4)));
    assert_eq!(target(0, "playback.previous_edit"), Some(time(0)));
    assert_eq!(target(2, "playback.start"), Some(time(0)));
    assert_eq!(target(2, "playback.end"), Some(time(4)));
    project.sequences.values_mut().next().unwrap().tracks[0].visible = false;
    assert_eq!(
        super::navigation_target(&project, time(0), "playback.next_edit"),
        Some(time(4))
    );
    assert_eq!(
        super::navigation_target(&Project::new("Empty"), time(0), "playback.next_edit"),
        None
    );
    Ok(())
}

#[test]
fn export_status_maps_to_graphical_task_properties() -> Result<(), Box<dyn std::error::Error>> {
    let mut controller = ExportController::new();
    controller.queue(PathBuf::from("deliver.mp4"));
    controller.start()?;
    controller.report_progress(42)?;
    let running = controller
        .status()
        .ok_or_else(|| std::io::Error::other("missing export status"))?;
    let running = NativeExportStatus::from(&running);
    assert_eq!(running.label, "EXPORTING");
    assert_eq!(running.output, "deliver.mp4");
    assert_eq!(running.progress, 42);
    assert!(running.can_cancel);
    assert!(!running.can_retry);

    controller.fail("encoder unavailable")?;
    let failed = controller
        .status()
        .ok_or_else(|| std::io::Error::other("missing export status"))?;
    let failed = NativeExportStatus::from(&failed);
    assert_eq!(failed.label, "FAILED");
    assert_eq!(failed.error, "encoder unavailable");
    assert!(failed.can_retry);
    assert!(failed.can_remove);
    Ok(())
}

#[test]
fn imported_videos_create_and_extend_a_timeline() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Edit");
    insert_video_into_project(
        &mut project,
        std::path::Path::new("first.mp4"),
        RationalTime::new(5, 1)?,
    )?;
    insert_video_into_project(
        &mut project,
        std::path::Path::new("second.mp4"),
        RationalTime::new(3, 1)?,
    )?;
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or_else(|| std::io::Error::other("missing sequence"))?;
    let track = sequence
        .tracks
        .first()
        .ok_or_else(|| std::io::Error::other("missing track"))?;
    assert_eq!(project.media.len(), 2);
    assert_eq!(track.clips.len(), 2);
    assert_eq!(track.clips[1].timeline_start, RationalTime::new(5, 1)?);
    Ok(())
}

#[test]
fn imported_av_media_creates_synchronized_video_and_audio_tracks()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Edit");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("interview.mp4"),
        RationalTime::new(5, 1)?,
        true,
    )?;
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?;
    let video = sequence
        .tracks
        .iter()
        .find(|track| track.kind == prism_project::TrackKind::Video)
        .ok_or("missing video track")?;
    let audio = sequence
        .tracks
        .iter()
        .find(|track| track.kind == prism_project::TrackKind::Audio)
        .ok_or("missing audio track")?;
    assert_eq!(video.clips.len(), 1);
    assert_eq!(audio.clips.len(), 1);
    assert_eq!(video.clips[0].timeline_start, audio.clips[0].timeline_start);
    assert_eq!(
        video.clips[0].source_duration,
        audio.clips[0].source_duration
    );
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFPROBE and the generated media fixture"]
fn real_media_import_probes_and_creates_editable_synchronized_tracks()
-> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PRISM_FFPROBE").is_none() {
        return Ok(());
    }
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media/av-ntsc-48k.mp4");
    let mut project = Project::new("Real import");
    super::import_media_into_project(&mut project, &fixture)?;
    project.validate()?;
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?;
    let video = sequence
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .ok_or("missing video track")?;
    let audio = sequence
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .ok_or("missing audio track")?;
    assert_eq!(project.media.len(), 1);
    assert_eq!(video.clips.len(), 1);
    assert_eq!(audio.clips.len(), 1);
    assert_eq!(video.clips[0].timeline_start, RationalTime::ZERO);
    assert_eq!(
        video.clips[0].source_duration,
        audio.clips[0].source_duration
    );
    assert!(video.clips[0].source_duration.is_positive());
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn real_audio_only_import_reopens_and_generates_waveform() -> Result<(), Box<dyn std::error::Error>>
{
    if std::env::var_os("PRISM_FFMPEG").is_none() || std::env::var_os("PRISM_FFPROBE").is_none() {
        return Ok(());
    }
    let root = std::env::temp_dir().join(format!(
        "prism-studio-real-audio-import-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media/av-ntsc-48k.mp4");
    let audio = root.join("narration.wav");
    let output = ProcessCommand::new(super::media_tool("ffmpeg"))
        .args(["-v", "error", "-i"])
        .arg(&fixture)
        .args(["-t", "1", "-vn", "-c:a", "pcm_s16le", "-y"])
        .arg(&audio)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }

    let mut project = Project::new("Audio import");
    super::import_media_into_project(&mut project, &audio)?;
    let project_path = root.join("audio.prism");
    prism_persistence::save_atomic(&project, &project_path)?;
    let reopened = prism_persistence::load(&project_path)?;
    let sequence = reopened
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?;
    assert_eq!(sequence.tracks.len(), 1);
    assert_eq!(sequence.tracks[0].kind, TrackKind::Audio);

    let worker = WaveformWorker::spawn(super::media_tool("ffmpeg"), 1);
    worker.try_submit(WaveformRequest {
        path: audio,
        cache_root: root.join("cache"),
        sample_rate: 48_000,
        max_samples: 48_000,
        samples_per_point: 1_024,
    })?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let waveform = loop {
        match worker.try_result() {
            Ok(result) => break result?,
            Err(std::sync::mpsc::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    };
    assert!(!waveform.points.is_empty());
    assert!(
        waveform
            .points
            .iter()
            .any(|point| point.max > 0.0 || point.min < 0.0)
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn audio_only_import_creates_only_an_audio_track() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Music edit");
    let imported = super::insert_stream_media_into_project(
        &mut project,
        std::path::Path::new("score.wav"),
        RationalTime::new(3, 1)?,
        false,
        true,
        None,
    )?;
    project.validate()?;
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?;
    assert_eq!(sequence.tracks.len(), 1);
    assert_eq!(sequence.tracks[0].kind, TrackKind::Audio);
    assert_eq!(sequence.tracks[0].clips.len(), 1);
    assert_eq!(
        sequence.tracks[0].clips[0].source_duration,
        RationalTime::new(3, 1)?
    );
    imported.apply(&mut project)?;
    assert!(project.media.is_empty());
    assert!(project.sequences.is_empty());
    Ok(())
}

#[test]
fn source_append_keeps_av_streams_aligned_and_undoes_atomically()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Source append");
    super::insert_stream_media_into_project(
        &mut project,
        std::path::Path::new("interview.mp4"),
        RationalTime::new(3, 1)?,
        true,
        true,
        None,
    )?;
    let media_id = *project.media.keys().next().ok_or("missing media")?;
    let commands = super::append_source_commands(
        &project,
        media_id,
        RationalTime::new(1, 2)?,
        RationalTime::new(1, 1)?,
    )?;
    assert_eq!(commands.len(), 2);
    let outcome = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Append source range",
        commands,
    )?;
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?;
    for track in &sequence.tracks {
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[1].timeline_start, RationalTime::new(3, 1)?);
        assert_eq!(track.clips[1].source_start, RationalTime::new(1, 2)?);
        assert_eq!(track.clips[1].source_duration, RationalTime::new(1, 1)?);
    }
    outcome.undo.apply(&mut project)?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?
            .tracks
            .iter()
            .all(|track| track.clips.len() == 1)
    );
    Ok(())
}

#[test]
fn duplicate_selected_av_pair_uses_one_aligned_transaction()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Duplicate pair");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("take.mp4"),
        RationalTime::new(2, 1)?,
        true,
    )?;
    let commands = super::duplicate_selected_commands(&project, 0)?;
    assert_eq!(commands.len(), 2);
    let outcome = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Duplicate aligned A/V",
        commands,
    )?;
    for track in &project
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?
        .tracks
    {
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[1].timeline_start, RationalTime::new(2, 1)?);
    }
    outcome.undo.apply(&mut project)?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?
            .tracks
            .iter()
            .all(|track| track.clips.len() == 1)
    );
    Ok(())
}

#[test]
fn slip_selected_av_pair_changes_both_sources_and_undoes_atomically()
-> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Slip pair");
    insert_media_into_project(
        &mut project,
        std::path::Path::new("take.mp4"),
        RationalTime::new(2, 1)?,
        true,
    )?;
    let commands = super::slip_selected_commands(&project, 0, RationalTime::new(1, 2)?)?;
    assert_eq!(commands.len(), 2);
    let outcome = prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Slip aligned A/V",
        commands,
    )?;
    let slipped = RationalTime::new(1, 2)?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?
            .tracks
            .iter()
            .all(|track| track.clips[0].source_start == slipped)
    );
    outcome.undo.apply(&mut project)?;
    assert!(
        project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?
            .tracks
            .iter()
            .all(|track| track.clips[0].source_start == RationalTime::ZERO)
    );
    Ok(())
}

#[test]
fn audio_source_append_targets_only_audio_at_sequence_end() -> Result<(), Box<dyn std::error::Error>>
{
    let mut project = Project::new("Audio append");
    super::insert_stream_media_into_project(
        &mut project,
        std::path::Path::new("score.wav"),
        RationalTime::new(3, 1)?,
        false,
        true,
        None,
    )?;
    let media_id = *project.media.keys().next().ok_or("missing media")?;
    let commands = super::append_source_commands(
        &project,
        media_id,
        RationalTime::new(1, 1)?,
        RationalTime::new(1, 2)?,
    )?;
    assert_eq!(commands.len(), 1);
    prism_commands::CommandEngine::execute_transaction(
        &mut project,
        "Append audio source range",
        commands,
    )?;
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?;
    assert_eq!(sequence.tracks.len(), 1);
    assert_eq!(sequence.tracks[0].kind, TrackKind::Audio);
    assert_eq!(sequence.tracks[0].clips.len(), 2);
    assert_eq!(
        sequence.tracks[0].clips[1].timeline_start,
        RationalTime::new(3, 1)?
    );
    assert_eq!(
        sequence.tracks[0].clips[1].source_start,
        RationalTime::new(1, 1)?
    );
    Ok(())
}

#[test]
fn mixed_media_imports_append_after_the_complete_sequence() -> Result<(), Box<dyn std::error::Error>>
{
    let mut project = Project::new("Mixed edit");
    super::insert_stream_media_into_project(
        &mut project,
        std::path::Path::new("score.wav"),
        RationalTime::new(3, 1)?,
        false,
        true,
        None,
    )?;
    super::insert_stream_media_into_project(
        &mut project,
        std::path::Path::new("scene.mp4"),
        RationalTime::new(2, 1)?,
        true,
        true,
        None,
    )?;
    project.validate()?;
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?;
    let video = sequence
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .ok_or("missing video track")?;
    let audio = sequence
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .ok_or("missing audio track")?;
    assert_eq!(video.clips[0].timeline_start, RationalTime::new(3, 1)?);
    assert_eq!(audio.clips[1].timeline_start, RationalTime::new(3, 1)?);
    assert_eq!(sequence.duration()?, RationalTime::new(5, 1)?);
    Ok(())
}
