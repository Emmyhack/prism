//! Opt-in executable-level export workflow.

use prism_commands::{Command, CommandEngine};
use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_media::FfprobeInspector;
use prism_persistence::save_atomic;
use prism_project::{Project, TrackKind};
use prism_timeline::RationalTime;
use std::{fs, path::PathBuf, process::Command as ProcessCommand};
use uuid::Uuid;

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn cli_loads_saved_project_and_exports_selected_track() -> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = PathBuf::from(std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?);
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory = std::env::temp_dir().join(format!("prism-cli-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("workflow.prism");
    let output_path = test_directory.join("cli.mp4");
    let mut project = Project::new("CLI workflow");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Build CLI timeline",
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
                timeline_start: RationalTime::new(1, 5)?,
                source_start: RationalTime::new(1, 4)?,
                source_duration: RationalTime::new(2, 5)?,
            },
        ],
    )?;
    save_atomic(&project, &project_path)?;

    let inspection = ProcessCommand::new(env!("CARGO_BIN_EXE_prism-cli"))
        .arg("inspect")
        .arg("--project")
        .arg(&project_path)
        .output()?;
    if !inspection.status.success() {
        return Err(format!(
            "CLI inspection failed: {}",
            String::from_utf8_lossy(&inspection.stderr)
        )
        .into());
    }
    let inspection: serde_json::Value = serde_json::from_slice(&inspection.stdout)?;
    assert_eq!(inspection["formatVersion"], 1);
    assert_eq!(
        inspection["sequences"][0]["id"],
        sequence_id.as_uuid().to_string()
    );
    assert_eq!(
        inspection["sequences"][0]["tracks"][0]["id"],
        track_id.as_uuid().to_string()
    );

    let process = ProcessCommand::new(env!("CARGO_BIN_EXE_prism-cli"))
        .arg("export-track")
        .arg("--project")
        .arg(&project_path)
        .arg("--sequence")
        .arg(sequence_id.as_uuid().to_string())
        .arg("--track")
        .arg(track_id.as_uuid().to_string())
        .arg("--output")
        .arg(&output_path)
        .arg("--width")
        .arg("320")
        .arg("--height")
        .arg("180")
        .arg("--fps")
        .arg("30/1")
        .arg("--ffmpeg")
        .arg(&ffmpeg)
        .arg("--ffprobe")
        .arg(&ffprobe)
        .output()?;
    if !process.status.success() {
        return Err(format!("CLI failed: {}", String::from_utf8_lossy(&process.stderr)).into());
    }
    let stdout = String::from_utf8(process.stdout)?;
    assert!(stdout.contains("Exported"));
    let metadata = FfprobeInspector::new(ffprobe).inspect(&output_path)?;
    assert!(metadata.duration.is_some_and(RationalTime::is_positive));
    assert_eq!(metadata.streams.len(), 1);

    fs::remove_file(&output_path)?;
    fs::remove_file(&project_path)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
fn cli_inspects_media_and_commits_first_video_sequence() -> Result<(), Box<dyn std::error::Error>> {
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-edit-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("edit.prism");
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let created = ProcessCommand::new(binary)
        .args(["create-project", "--project"])
        .arg(&project_path)
        .args(["--name", "CLI edit"])
        .output()?;
    assert!(created.status.success());
    let edited = ProcessCommand::new(binary)
        .args(["create-video-sequence", "--project"])
        .arg(&project_path)
        .arg("--media")
        .arg(&fixture)
        .args([
            "--sequence-name",
            "Main",
            "--source-start",
            "1/4",
            "--duration",
            "1/2",
            "--ffprobe",
        ])
        .arg(&ffprobe)
        .output()?;
    if !edited.status.success() {
        return Err(String::from_utf8_lossy(&edited.stderr).into_owned().into());
    }
    let inspected = ProcessCommand::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(json["revision"], 1);
    assert_eq!(json["media"].as_array().map(Vec::len), Some(1));
    assert_eq!(json["sequences"].as_array().map(Vec::len), Some(1));
    let clip = &json["sequences"][0]["tracks"][0]["clips"][0];
    assert_eq!(clip["sourceStart"]["value"], 1);
    assert_eq!(clip["sourceStart"]["timescale"], 4);
    assert_eq!(clip["sourceDuration"]["value"], 1);
    assert_eq!(clip["sourceDuration"]["timescale"], 2);
    fs::remove_file(&project_path)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFPROBE and the generated media fixture"]
#[allow(clippy::too_many_lines)]
fn cli_adds_a_second_video_clip_to_existing_track() -> Result<(), Box<dyn std::error::Error>> {
    let ffprobe =
        PathBuf::from(std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?);
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-append-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("append.prism");
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let created = ProcessCommand::new(binary)
        .args(["create-project", "--project"])
        .arg(&project_path)
        .args(["--name", "CLI append"])
        .output()?;
    assert!(created.status.success());
    let sequence = ProcessCommand::new(binary)
        .args(["create-video-sequence", "--project"])
        .arg(&project_path)
        .arg("--media")
        .arg(&fixture)
        .args([
            "--sequence-name",
            "Main",
            "--source-start",
            "0/1",
            "--duration",
            "1/4",
            "--ffprobe",
        ])
        .arg(&ffprobe)
        .output()?;
    assert!(sequence.status.success());
    let inspected = ProcessCommand::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(inspected.status.success());
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    let track_id = json["sequences"][0]["tracks"][0]["id"]
        .as_str()
        .ok_or("missing track id")?;
    let appended = ProcessCommand::new(binary)
        .args(["add-video-clip", "--project"])
        .arg(&project_path)
        .args(["--track", track_id, "--media"])
        .arg(&fixture)
        .args([
            "--source-start",
            "1/2",
            "--timeline-start",
            "1/4",
            "--duration",
            "1/4",
            "--ffprobe",
        ])
        .arg(&ffprobe)
        .output()?;
    if !appended.status.success() {
        return Err(String::from_utf8_lossy(&appended.stderr)
            .into_owned()
            .into());
    }
    let appended_json: serde_json::Value = serde_json::from_slice(
        &ProcessCommand::new(binary)
            .args(["inspect", "--project"])
            .arg(&project_path)
            .output()?
            .stdout,
    )?;
    let clip_id = appended_json["sequences"][0]["tracks"][0]["clips"][1]["id"]
        .as_str()
        .ok_or("missing appended clip id")?;
    let moved = ProcessCommand::new(binary)
        .args(["move-video-clip", "--project"])
        .arg(&project_path)
        .args([
            "--track",
            track_id,
            "--clip",
            clip_id,
            "--timeline-start",
            "1/2",
        ])
        .output()?;
    if !moved.status.success() {
        return Err(String::from_utf8_lossy(&moved.stderr).into_owned().into());
    }
    let trimmed = ProcessCommand::new(binary)
        .args(["trim-video-clip", "--project"])
        .arg(&project_path)
        .args([
            "--track",
            track_id,
            "--clip",
            clip_id,
            "--timeline-start",
            "3/4",
            "--source-start",
            "1/4",
            "--duration",
            "1/8",
        ])
        .output()?;
    if !trimmed.status.success() {
        return Err(String::from_utf8_lossy(&trimmed.stderr).into_owned().into());
    }
    let split = ProcessCommand::new(binary)
        .args(["split-video-clip", "--project"])
        .arg(&project_path)
        .args([
            "--track",
            track_id,
            "--clip",
            clip_id,
            "--timeline-position",
            "13/16",
        ])
        .output()?;
    if !split.status.success() {
        return Err(String::from_utf8_lossy(&split.stderr).into_owned().into());
    }
    let split_json: serde_json::Value = serde_json::from_slice(
        &ProcessCommand::new(binary)
            .args(["inspect", "--project"])
            .arg(&project_path)
            .output()?
            .stdout,
    )?;
    let right_clip_id = split_json["sequences"][0]["tracks"][0]["clips"][2]["id"]
        .as_str()
        .ok_or("missing split clip id")?;
    let deleted = ProcessCommand::new(binary)
        .args(["delete-video-clip", "--project"])
        .arg(&project_path)
        .args(["--track", track_id, "--clip", right_clip_id])
        .output()?;
    if !deleted.status.success() {
        return Err(String::from_utf8_lossy(&deleted.stderr).into_owned().into());
    }
    let inspected = ProcessCommand::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    let clips = json["sequences"][0]["tracks"][0]["clips"]
        .as_array()
        .ok_or("missing clips")?;
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[1]["timelineStart"]["value"], 3);
    assert_eq!(clips[1]["timelineStart"]["timescale"], 4);
    assert_eq!(clips[1]["sourceStart"]["value"], 1);
    assert_eq!(clips[1]["sourceStart"]["timescale"], 4);
    assert_eq!(clips[1]["sourceDuration"]["value"], 1);
    assert_eq!(clips[1]["sourceDuration"]["timescale"], 16);
    fs::remove_file(&project_path)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}
