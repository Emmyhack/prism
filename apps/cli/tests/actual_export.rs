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
