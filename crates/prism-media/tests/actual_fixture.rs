//! Opt-in verification against a real generated audio/video fixture.

use prism_media::{
    FfmpegReferenceDecoder, FfprobeInspector, FrameRateEvidence, PacketTimingEvidence, StreamKind,
    fingerprint,
};
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
    let timing = inspector.inspect_video_packet_timing(&fixture, 0)?;
    assert_eq!(timing.packet_count, 29);
    assert_eq!(timing.distinct_duration_ticks, vec![1_001]);
    assert_eq!(timing.evidence, PacketTimingEvidence::ObservedConstant);
    let fingerprint = fingerprint(&fixture)?;
    assert_eq!(fingerprint.byte_length, 110_212);
    assert_eq!(
        fingerprint.sha256,
        "0cde7c90cb7b7e01034a91ed883054d4f01432668f9bf2b8dbae99954f7ec54d"
    );
    Ok(())
}

#[test]
#[ignore = "requires PRISM_FFMPEG and the generated media fixture"]
fn decodes_and_seeks_actual_video_frames() -> Result<(), Box<dyn std::error::Error>> {
    let ffmpeg = std::env::var_os("PRISM_FFMPEG").ok_or("PRISM_FFMPEG is not set")?;
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
    let decoder = FfmpegReferenceDecoder::new(ffmpeg);
    let first = decoder.decode_video_frame_rgba(&fixture, 0, RationalTime::new(0, 1)?, 320, 180)?;
    let later = decoder.decode_video_frame_rgba(&fixture, 0, RationalTime::new(1, 2)?, 320, 180)?;
    assert_eq!(first.rgba.len(), 320 * 180 * 4);
    assert_eq!(later.rgba.len(), 320 * 180 * 4);
    assert_eq!(
        first.sha256(),
        "87274041e634c48da08ae47818a30e5934c45d17a4782e81b3b1979405925ad4"
    );
    assert_eq!(
        later.sha256(),
        "425a3107ba67e8586bdb38737176a7a916f9994dbb4966497c2311abf24f6001"
    );
    Ok(())
}
