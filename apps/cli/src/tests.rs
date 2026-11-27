use super::{
    CliError, CliOutcome, ProjectInspection, parse_export_preset_track, parse_export_track,
    parse_export_video_audio, run,
};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId};
use prism_project::{Clip, Marker, MediaAsset, Project, Sequence, Track, TrackKind};
use prism_timeline::RationalTime;
use std::ffi::OsString;
use uuid::Uuid;

#[test]
fn help_and_version_are_successful_informational_commands() -> Result<(), Box<dyn std::error::Error>>
{
    let CliOutcome::Information(help) = run([OsString::from("--help")])? else {
        return Err("help did not return informational output".into());
    };
    assert!(help.starts_with("usage: prism-cli"));
    let CliOutcome::Information(version) = run([OsString::from("--version")])? else {
        return Err("version did not return informational output".into());
    };
    assert_eq!(version, format!("prism-cli {}", env!("CARGO_PKG_VERSION")));
    Ok(())
}

fn valid_arguments() -> Vec<OsString> {
    [
        "export-track",
        "--project",
        "edit.prism",
        "--sequence",
        "11111111-1111-4111-8111-111111111111",
        "--track",
        "22222222-2222-4222-8222-222222222222",
        "--output",
        "finish.mp4",
        "--width",
        "1920",
        "--height",
        "1080",
        "--fps",
        "30000/1001",
        "--video-bitrate-kbps",
        "8000",
        "--ffmpeg",
        "/tools/ffmpeg",
        "--ffprobe",
        "/tools/ffprobe",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

fn valid_video_audio_arguments() -> Vec<OsString> {
    [
        "export-video-audio",
        "--project",
        "edit.prism",
        "--sequence",
        "11111111-1111-4111-8111-111111111111",
        "--video-track",
        "22222222-2222-4222-8222-222222222222",
        "--audio-track",
        "33333333-3333-4333-8333-333333333333",
        "--output",
        "finish.mp4",
        "--width",
        "1920",
        "--height",
        "1080",
        "--fps",
        "30000/1001",
        "--video-bitrate-kbps",
        "8000",
        "--audio-bitrate-kbps",
        "192",
        "--ffmpeg",
        "/tools/ffmpeg",
        "--ffprobe",
        "/tools/ffprobe",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

#[test]
fn parses_typed_export_track_command() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse_export_track(valid_arguments())?;
    assert_eq!(parsed.width, 1920);
    assert_eq!(parsed.height, 1080);
    assert_eq!(parsed.frame_rate, RationalTime::new(30_000, 1_001)?);
    assert_eq!(parsed.video_bitrate_kbps, 8_000);
    assert_eq!(
        parsed.sequence_id.as_uuid().to_string(),
        "11111111-1111-4111-8111-111111111111"
    );
    Ok(())
}

#[test]
fn export_track_uses_safe_default_bitrate_when_omitted() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = valid_arguments();
    let index = arguments
        .iter()
        .position(|argument| argument == "--video-bitrate-kbps")
        .ok_or("missing bitrate option")?;
    arguments.drain(index..=index + 1);
    let parsed = parse_export_track(arguments)?;
    assert_eq!(parsed.video_bitrate_kbps, 5_000);
    Ok(())
}

#[test]
fn parses_preset_export_command() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = [
        "export-preset-track",
        "--project",
        "edit.prism",
        "--sequence",
        "11111111-1111-4111-8111-111111111111",
        "--track",
        "22222222-2222-4222-8222-222222222222",
        "--preset",
        "youtube-1080p",
        "--video-bitrate-kbps",
        "6000",
        "--output",
        "finish.mp4",
        "--ffmpeg",
        "/tools/ffmpeg",
        "--ffprobe",
        "/tools/ffprobe",
    ]
    .into_iter()
    .map(OsString::from);
    let parsed = parse_export_preset_track(arguments)?;
    assert_eq!(parsed.video_bitrate_kbps, Some(6_000));
    assert_eq!(parsed.preset, prism_export::ExportPreset::Youtube1080p);
    assert_eq!(parsed.output, std::path::PathBuf::from("finish.mp4"));
    Ok(())
}

#[test]
fn parses_typed_video_audio_export_command() -> Result<(), Box<dyn std::error::Error>> {
    let parsed = parse_export_video_audio(valid_video_audio_arguments())?;
    assert_eq!(parsed.width, 1920);
    assert_eq!(parsed.height, 1080);
    assert_eq!(parsed.frame_rate, RationalTime::new(30_000, 1_001)?);
    assert_eq!(parsed.video_bitrate_kbps, 8_000);
    assert_eq!(parsed.audio_bitrate_kbps, 192);
    assert_eq!(
        parsed.audio_track_id.as_uuid().to_string(),
        "33333333-3333-4333-8333-333333333333"
    );
    Ok(())
}

#[test]
fn rejects_duplicate_and_missing_options() {
    let mut duplicate = valid_arguments();
    duplicate.extend([OsString::from("--width"), OsString::from("640")]);
    assert!(matches!(
        parse_export_track(duplicate),
        Err(CliError::InvalidOption(option)) if option == "--width"
    ));
    assert!(matches!(
        parse_export_track([OsString::from("export-track")]),
        Err(CliError::MissingOption("--project"))
    ));
}

#[test]
fn rejects_malformed_frame_rate() {
    let mut arguments = valid_arguments();
    let fps = arguments
        .iter_mut()
        .skip_while(|argument| argument.as_os_str() != "--fps")
        .nth(1);
    if let Some(fps) = fps {
        *fps = OsString::from("29.97");
    }
    assert!(matches!(
        parse_export_track(arguments),
        Err(CliError::InvalidValue {
            option: "--fps",
            ..
        })
    ));
}

#[test]
fn rejects_zero_export_bitrate() {
    let mut arguments = valid_arguments();
    let index = arguments
        .iter()
        .position(|argument| argument == "--video-bitrate-kbps");
    assert!(index.is_some());
    let index = index.unwrap_or_default() + 1;
    arguments[index] = OsString::from("0");
    assert!(matches!(
        parse_export_track(arguments),
        Err(CliError::InvalidValue {
            option: "--video-bitrate-kbps",
            ..
        })
    ));
}

#[test]
fn rejects_zero_preset_bitrate_override() {
    let arguments = [
        "export-preset-track",
        "--project",
        "edit.prism",
        "--sequence",
        "11111111-1111-4111-8111-111111111111",
        "--track",
        "22222222-2222-4222-8222-222222222222",
        "--preset",
        "youtube-1080p",
        "--video-bitrate-kbps",
        "0",
        "--output",
        "finish.mp4",
        "--ffmpeg",
        "/tools/ffmpeg",
        "--ffprobe",
        "/tools/ffprobe",
    ]
    .into_iter()
    .map(OsString::from);
    assert!(matches!(
        parse_export_preset_track(arguments),
        Err(CliError::InvalidValue {
            option: "--video-bitrate-kbps",
            ..
        })
    ));
}

#[test]
fn lists_autosave_candidates_as_newest_first_json() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("prism-cli-autosaves-{}", Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let project = directory.join("edit.prism");
    std::fs::write(directory.join("edit.prism.autosave.2"), b"candidate")?;
    std::fs::write(directory.join("edit.prism.autosave.5"), b"candidate")?;
    let outcome = run([
        OsString::from("list-autosaves"),
        OsString::from("--project"),
        project.as_os_str().to_owned(),
    ])?;
    let CliOutcome::AutosavesListed { json } = outcome else {
        return Err("unexpected CLI outcome".into());
    };
    let values: Vec<serde_json::Value> = serde_json::from_str(&json)?;
    assert_eq!(values[0]["revision"], 5);
    assert_eq!(values[1]["revision"], 2);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn inspection_json_preserves_ids_order_and_exact_times() -> Result<(), Box<dyn std::error::Error>> {
    let mut project = Project::new("Inspection");
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    let clip_id = ClipId::new();
    let marker_id = MarkerId::new();
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
            markers: vec![Marker {
                id: marker_id,
                time: RationalTime::new(2, 1)?,
                label: "Beat".to_owned(),
            }],
            transitions: Vec::new(),
            tracks: vec![Track {
                id: track_id,
                kind: TrackKind::Video,
                locked: false,
                visible: true,
                muted: false,
                clips: vec![Clip {
                    id: clip_id,
                    media_id,
                    timeline_start: RationalTime::new(1_001, 30_000)?,
                    source_start: RationalTime::new(1, 2)?,
                    source_duration: RationalTime::new(3, 4)?,
                }],
            }],
        },
    );
    project.validate()?;
    let json = serde_json::to_value(ProjectInspection::from(&project))?;
    assert_eq!(json["formatVersion"], 1);
    assert_eq!(json["projectId"], project.id.as_uuid().to_string());
    assert_eq!(json["media"][0]["id"], media_id.as_uuid().to_string());
    assert_eq!(
        json["sequences"][0]["id"],
        sequence_id.as_uuid().to_string()
    );
    assert_eq!(json["sequences"][0]["tracks"][0]["kind"], "video");
    assert_eq!(
        json["sequences"][0]["markers"][0]["id"],
        marker_id.as_uuid().to_string()
    );
    assert_eq!(json["sequences"][0]["markers"][0]["label"], "Beat");
    let time = &json["sequences"][0]["tracks"][0]["clips"][0]["timelineStart"];
    assert_eq!(time["value"], 1_001);
    assert_eq!(time["timescale"], 30_000);
    Ok(())
}
