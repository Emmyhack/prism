//! Opt-in basic MP4 export verification against the generated fixture.

use prism_export::{BasicMp4ExportRequest, BasicMp4Exporter};
use prism_media::{FfmpegReferenceDecoder, FfprobeInspector, StreamKind};
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
