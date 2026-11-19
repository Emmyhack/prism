//! Continuous single-track audio preview with explicit trims and finite silence gaps.
use prism_export::{SequenceExportSegment, plan_single_audio_track};
use prism_project::{Project, TrackKind};
use prism_timeline::RationalTime;
#[cfg(test)]
use std::io::Write as _;
use std::{
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AudioInput {
    Silence(RationalTime),
    Source {
        path: PathBuf,
        start: RationalTime,
        duration: RationalTime,
    },
}
impl AudioInput {
    fn duration(&self) -> RationalTime {
        match self {
            Self::Silence(duration) | Self::Source { duration, .. } => *duration,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AudioPreviewPlan {
    pub inputs: Vec<AudioInput>,
    pub duration: RationalTime,
}

pub(crate) fn plan(
    project: &Project,
    position: RationalTime,
) -> Result<Option<AudioPreviewPlan>, String> {
    if position.is_negative() {
        return Err("Audio preview position must be nonnegative".into());
    }
    let Some(sequence) = project.sequences.values().next() else {
        return Ok(None);
    };
    let tracks = sequence
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio && !track.muted && !track.clips.is_empty())
        .collect::<Vec<_>>();
    if tracks.len() > 1 {
        return Err("Audio preview currently supports one active audio track. Mute extra tracks before playing.".into());
    }
    let Some(track) = tracks.first() else {
        return Ok(None);
    };
    let end = sequence.duration().map_err(|error| error.to_string())?;
    if position >= end {
        return Ok(None);
    }
    let track_plan = plan_single_audio_track(project, sequence.id, track.id)
        .map_err(|error| error.to_string())?;
    let mut inputs = Vec::new();
    for segment in &track_plan.segments {
        let (start, duration) = match segment {
            SequenceExportSegment::Gap {
                timeline_start,
                duration,
            }
            | SequenceExportSegment::Clip {
                timeline_start,
                duration,
                ..
            } => (*timeline_start, *duration),
        };
        let segment_end = start
            .checked_add(duration)
            .map_err(|error| error.to_string())?;
        let from = start.max(position);
        if from >= segment_end {
            continue;
        }
        let duration = segment_end
            .checked_sub(from)
            .map_err(|error| error.to_string())?;
        inputs.push(match segment {
            SequenceExportSegment::Gap { .. } => AudioInput::Silence(duration),
            SequenceExportSegment::Clip {
                media_id,
                source_start,
                ..
            } => AudioInput::Source {
                path: PathBuf::from(
                    &project
                        .media
                        .get(media_id)
                        .ok_or("Audio source unavailable")?
                        .source_path,
                ),
                start: source_start
                    .checked_add(from.checked_sub(start).map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?,
                duration,
            },
        });
    }
    let tail_start = track_plan.duration.max(position);
    if end > tail_start {
        inputs.push(AudioInput::Silence(
            end.checked_sub(tail_start)
                .map_err(|error| error.to_string())?,
        ));
    }
    if inputs.len() > 128 {
        return Err("This preview exceeds the current 128-segment audio session limit".into());
    }
    Ok(Some(AudioPreviewPlan {
        inputs,
        duration: end
            .checked_sub(position)
            .map_err(|error| error.to_string())?,
    }))
}

pub(crate) fn decoder_arguments(plan: &AudioPreviewPlan) -> Result<Vec<OsString>, String> {
    if plan.inputs.is_empty() || plan.inputs.len() > 128 {
        return Err("Invalid audio preview segment count".into());
    }
    let total = plan
        .inputs
        .iter()
        .try_fold(RationalTime::ZERO, |total, input| {
            total.checked_add(input.duration())
        })
        .map_err(|error| error.to_string())?;
    if total != plan.duration {
        return Err("Audio preview duration does not match its inputs".into());
    }
    let mut arguments: Vec<OsString> = ["-nostdin", "-v", "error"]
        .into_iter()
        .map(Into::into)
        .collect();
    let mut filter = String::new();
    for (index, input) in plan.inputs.iter().enumerate() {
        let duration = input.duration();
        if !duration.is_positive() {
            return Err("Audio preview ranges must be positive".into());
        }
        let seconds = format!("{:.9}", duration.as_seconds_f64());
        match input {
            AudioInput::Silence(_) => arguments.extend([
                "-f".into(),
                "lavfi".into(),
                "-t".into(),
                seconds.clone().into(),
                "-i".into(),
                "anullsrc=r=48000:cl=stereo".into(),
            ]),
            AudioInput::Source { path, start, .. } => {
                if start.is_negative() {
                    return Err("Audio source time must be nonnegative".into());
                }
                arguments.extend([
                    "-ss".into(),
                    format!("{:.9}", start.as_seconds_f64()).into(),
                    "-i".into(),
                    path.as_os_str().to_owned(),
                ]);
            }
        }
        write!(filter, "[{index}:a:0]aresample=48000,aformat=sample_fmts=flt:channel_layouts=stereo,asetpts=PTS-STARTPTS,apad=whole_dur={seconds},atrim=duration={seconds},asetpts=PTS-STARTPTS[a{index}];").map_err(|error| error.to_string())?;
    }
    for index in 0..plan.inputs.len() {
        write!(filter, "[a{index}]").map_err(|error| error.to_string())?;
    }
    write!(filter, "concat=n={}:v=0:a=1[outa]", plan.inputs.len())
        .map_err(|error| error.to_string())?;
    arguments.extend([
        "-filter_complex_threads".into(),
        "1".into(),
        "-filter_complex".into(),
        filter.into(),
        "-map".into(),
        "[outa]".into(),
        "-threads".into(),
        "1".into(),
        "-c:a".into(),
        "pcm_f32le".into(),
        "-ar".into(),
        "48000".into(),
        "-ac".into(),
        "2".into(),
        "-f".into(),
        "wav".into(),
        "pipe:1".into(),
    ]);
    Ok(arguments)
}

pub(crate) fn start(
    plan: &AudioPreviewPlan,
    ffmpeg: &Path,
    ffplay: &Path,
) -> Result<(Child, Child, crate::audio_clock::AudioClock), String> {
    start_with_driver(plan, ffmpeg, ffplay, None)
}

fn start_with_driver(
    plan: &AudioPreviewPlan,
    ffmpeg: &Path,
    ffplay: &Path,
    driver: Option<&str>,
) -> Result<(Child, Child, crate::audio_clock::AudioClock), String> {
    let mut decoder = Command::new(ffmpeg)
        .args(decoder_arguments(plan)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("Audio decoder unavailable: {error}"))?;
    let Some(pipe) = decoder.stdout.take() else {
        let _ = decoder.kill();
        let _ = decoder.wait();
        return Err("Audio decoder output unavailable".into());
    };
    let mut command = Command::new(ffplay);
    command
        .args([
            "-nodisp",
            "-autoexit",
            "-hide_banner",
            "-loglevel",
            "info",
            "-stats",
            "-i",
            "pipe:0",
        ])
        .stdin(Stdio::from(pipe))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(driver) = driver {
        command.env("SDL_AUDIODRIVER", driver);
    }
    match command.spawn() {
        Ok(mut player) => {
            let clock = crate::audio_clock::AudioClock::default();
            let Some(stderr) = player.stderr.take() else {
                let _ = player.kill();
                let _ = player.wait();
                let _ = decoder.kill();
                let _ = decoder.wait();
                return Err("Audio playback clock unavailable".into());
            };
            let observed = clock.clone();
            std::thread::spawn(move || crate::audio_clock::read_status(stderr, &observed));
            Ok((decoder, player, clock))
        }
        Err(error) => {
            let _ = decoder.kill();
            let _ = decoder.wait();
            Err(format!("Audio player unavailable: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plans_resume_from_playhead_and_preserve_silence_and_mute()
    -> Result<(), Box<dyn std::error::Error>> {
        use prism_commands::{Command as Edit, CommandEngine};
        use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
        let mut project = Project::new("Audio session");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        CommandEngine::execute_transaction(
            &mut project,
            "Audio fixture",
            vec![
                Edit::ImportMedia {
                    id: media_id,
                    source_path: "take.wav".into(),
                },
                Edit::CreateSequence {
                    id: sequence_id,
                    name: "Edit".into(),
                },
                Edit::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Audio,
                },
                Edit::InsertClip {
                    sequence_id,
                    track_id,
                    id: ClipId::new(),
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: RationalTime::new(1, 1)?,
                },
                Edit::InsertClip {
                    sequence_id,
                    track_id,
                    id: ClipId::new(),
                    media_id,
                    timeline_start: RationalTime::new(2, 1)?,
                    source_start: RationalTime::new(1, 4)?,
                    source_duration: RationalTime::new(1, 1)?,
                },
            ],
        )?;
        let preview = plan(&project, RationalTime::new(1, 2)?)?.ok_or("Missing plan")?;
        assert_eq!(preview.duration, RationalTime::new(5, 2)?);
        assert_eq!(preview.inputs.len(), 3);
        assert_eq!(
            preview.inputs[0],
            AudioInput::Source {
                path: PathBuf::from("take.wav"),
                start: RationalTime::new(1, 2)?,
                duration: RationalTime::new(1, 2)?
            }
        );
        assert_eq!(
            preview.inputs[1],
            AudioInput::Silence(RationalTime::new(1, 1)?)
        );
        assert_eq!(
            preview.inputs[2],
            AudioInput::Source {
                path: PathBuf::from("take.wav"),
                start: RationalTime::new(1, 4)?,
                duration: RationalTime::new(1, 1)?
            }
        );
        let gap = plan(&project, RationalTime::new(3, 2)?)?.ok_or("Missing gap plan")?;
        assert_eq!(gap.inputs[0], AudioInput::Silence(RationalTime::new(1, 2)?));
        assert!(plan(&project, RationalTime::new(3, 1)?)?.is_none());
        let mut extra = project
            .sequences
            .get(&sequence_id)
            .ok_or("Missing sequence")?
            .tracks[0]
            .clone();
        extra.id = TrackId::new();
        for clip in &mut extra.clips {
            clip.id = ClipId::new();
        }
        project
            .sequences
            .get_mut(&sequence_id)
            .ok_or("Missing sequence")?
            .tracks
            .push(extra);
        assert!(plan(&project, RationalTime::ZERO).is_err());
        for track in &mut project
            .sequences
            .get_mut(&sequence_id)
            .ok_or("Missing sequence")?
            .tracks
        {
            track.muted = true;
        }
        assert!(plan(&project, RationalTime::ZERO)?.is_none());
        Ok(())
    }

    #[test]
    fn decoder_arguments_reject_inconsistent_or_unbounded_sessions()
    -> Result<(), Box<dyn std::error::Error>> {
        let second = RationalTime::new(1, 1)?;
        assert!(
            decoder_arguments(&AudioPreviewPlan {
                inputs: vec![AudioInput::Silence(second)],
                duration: RationalTime::new(2, 1)?
            })
            .is_err()
        );
        assert!(
            decoder_arguments(&AudioPreviewPlan {
                inputs: vec![AudioInput::Silence(second); 129],
                duration: RationalTime::new(129, 1)?
            })
            .is_err()
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires PRISM_FFMPEG, PRISM_FFPLAY, and an SDL dummy audio driver"]
    fn real_player_reports_monotonic_audio_master_with_dummy_device()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("PRISM_TEST_FFMPEG").is_none() {
            return Ok(());
        }
        let plan = AudioPreviewPlan {
            inputs: vec![AudioInput::Silence(RationalTime::new(1, 1)?)],
            duration: RationalTime::new(1, 1)?,
        };
        let ffmpeg =
            std::env::var_os("PRISM_FFMPEG").map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from);
        let ffplay =
            std::env::var_os("PRISM_FFPLAY").map_or_else(|| PathBuf::from("ffplay"), PathBuf::from);
        let (mut decoder, mut player, clock) =
            start_with_driver(&plan, &ffmpeg, &ffplay, Some("dummy"))?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut observations = Vec::new();
        let mut exited = None;
        while std::time::Instant::now() < deadline {
            if let Some(time) = clock.position() {
                observations.push(time);
            }
            if let Some(status) = player.try_wait()? {
                exited = Some(status);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if exited.is_none() {
            let _ = player.kill();
            let _ = player.wait();
        }
        let _ = decoder.kill();
        let _ = decoder.wait();
        assert!(
            exited.is_some_and(|status| status.success()),
            "Dummy audio session did not finish"
        );
        assert!(observations.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            observations
                .last()
                .is_some_and(|time| time.as_seconds_f64() >= 0.5 && time.as_seconds_f64() <= 1.1),
            "Audio clock did not report a usable position"
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires PRISM_FFMPEG and the generated media fixture"]
    fn continuous_audio_renders_exact_silence_between_trimmed_clips()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("PRISM_TEST_FFMPEG").is_none() {
            return Ok(());
        }
        let started = std::time::Instant::now();
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
        let quarter = RationalTime::new(1, 4)?;
        let plan = AudioPreviewPlan {
            inputs: vec![
                AudioInput::Source {
                    path: source.clone(),
                    start: RationalTime::ZERO,
                    duration: quarter,
                },
                AudioInput::Silence(quarter),
                AudioInput::Source {
                    path: source,
                    start: RationalTime::new(1, 2)?,
                    duration: quarter,
                },
            ],
            duration: RationalTime::new(3, 4)?,
        };
        let ffmpeg = std::env::var_os("PRISM_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
        let output = Command::new(&ffmpeg)
            .args(decoder_arguments(&plan)?)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // Decode the streaming WAV to mono raw PCM for a signal-based gap check.
        let mut child = Command::new(ffmpeg)
            .args([
                "-v", "error", "-i", "pipe:0", "-f", "f32le", "-ac", "1", "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or("Missing input pipe")?;
        let writer = std::thread::spawn(move || stdin.write_all(&output.stdout));
        let result = child.wait_with_output()?;
        writer.join().map_err(|_| "Audio fixture writer failed")??;
        assert!(result.status.success());
        let samples = result
            .stdout
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect::<Vec<_>>();
        assert_eq!(samples.len(), 36_000);
        assert!(
            samples[13_000..23_000]
                .iter()
                .all(|sample| sample.abs() < 0.00001)
        );
        assert!(samples[..11_000].iter().any(|sample| sample.abs() > 0.01));
        assert!(samples[25_000..].iter().any(|sample| sample.abs() > 0.01));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(15),
            "Continuous audio fixture exceeded the 15-second acceptance budget"
        );
        Ok(())
    }
}
