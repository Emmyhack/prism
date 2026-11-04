//! Opt-in verification against a real generated audio/video fixture.

use prism_media::{FfprobeInspector, FrameRateEvidence, StreamKind};
use prism_timeline::RationalTime;
use std::path::PathBuf;

#[test]
#[ignore = "requires PRISM_FFPROBE and the generated media fixture"]
fn inspects_actual_ntsc_video_and_48k_audio() -> Result<(), Box<dyn std::error::Error>> {
    let ffprobe = std::env::var_os("PRISM_FFPROBE").ok_or("PRISM_FFPROBE is not set")?;
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let inspector = FfprobeInspector::new(ffprobe);
    let capabilities = inspector.capabilities()?;
    assert!(capabilities.ffprobe_version.starts_with("ffprobe version"));
    let metadata = inspector.inspect(&fixture)?;
    assert!(metadata.format_names.iter().any(|name| name == "mp4"));
    assert_eq!(metadata.duration, Some(RationalTime::new(1, 1)?));
    assert_eq!(metadata.streams.len(), 2);
    let StreamKind::Video(video) = &metadata.streams[0].kind else {
        return Err("stream 0 was not video".into());
    };
    assert_eq!(video.width, Some(320));
    assert_eq!(video.height, Some(180));
    assert_eq!(
        video.nominal_frame_rate,
        Some(RationalTime::new(30_000, 1_001)?)
    );
    assert_eq!(video.frame_rate_evidence, FrameRateEvidence::ConstantLikely);
    let StreamKind::Audio(audio) = &metadata.streams[1].kind else {
        return Err("stream 1 was not audio".into());
    };
    assert_eq!(audio.sample_rate, Some(48_000));
    assert_eq!(audio.channels, Some(1));
    Ok(())
}
