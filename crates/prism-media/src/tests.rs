use super::{
    DecodedVideoFrame, FfmpegProxyGenerator, FfmpegReferenceDecoder, FrameRateEvidence,
    MediaArtifactKind, MediaArtifactWorker, MediaCatalog, MediaCatalogEntry, MediaError,
    MediaFingerprint, MediaItemKind, MediaMetadata, MediaSourceSelection, PacketTimingEvidence,
    ProbeOutput, ProbePacket, ProxyProfile, ProxyRequest, ProxyStatus, StreamKind,
    analyze_packet_timing, checked_rgba_length, decode_stereo_waveform_artifact,
    decode_thumbnail_artifact, decode_waveform_artifact, fingerprint, format_seek_timestamp,
    parse_decimal_time, proxy_status, select_media_source,
};
use prism_timeline::RationalTime;
use std::{fs, path::Path};

#[test]
fn fingerprint_control_checks_cancellation_and_deadline_before_io() {
    let cancelled = std::sync::atomic::AtomicBool::new(true);
    let control = super::MediaProcessControl {
        timeout: std::time::Duration::from_secs(1),
        cancelled: &cancelled,
    };
    assert!(matches!(
        super::fingerprint_with_control(Path::new("/nonexistent/source.mov"), control),
        Err(MediaError::OperationCancelled)
    ));
    let active = std::sync::atomic::AtomicBool::new(false);
    assert!(matches!(
        super::fingerprint_with_control(
            Path::new("/nonexistent/source.mov"),
            super::MediaProcessControl {
                timeout: std::time::Duration::ZERO,
                cancelled: &active
            }
        ),
        Err(MediaError::OperationTimedOut)
    ));
    assert!(matches!(
        FfmpegReferenceDecoder::new("/nonexistent/media-tool").decode_audio_mono_f32_with_control(
            Path::new("source.wav"),
            0,
            48_000,
            1,
            control
        ),
        Err(MediaError::OperationCancelled)
    ));
}

#[test]
fn supervised_preview_interfaces_honor_pre_cancelled_requests() {
    let cancelled = std::sync::atomic::AtomicBool::new(true);
    let control = super::MediaProcessControl {
        timeout: std::time::Duration::from_secs(1),
        cancelled: &cancelled,
    };
    assert!(matches!(
        super::FfprobeInspector::new("/nonexistent/media-tool")
            .inspect_with_control(Path::new("source.mp4"), control),
        Err(MediaError::OperationCancelled)
    ));
    assert!(matches!(
        FfmpegReferenceDecoder::new("/nonexistent/media-tool")
            .decode_video_frame_rgba_with_control(
                Path::new("source.mp4"),
                0,
                RationalTime::ZERO,
                1,
                1,
                control
            ),
        Err(MediaError::OperationCancelled)
    ));
    assert!(matches!(
        FfmpegReferenceDecoder::new("/nonexistent/media-tool").decode_audio_mono_f32_with_control(
            Path::new("source.mp4"),
            0,
            48_000,
            1_024,
            control
        ),
        Err(MediaError::OperationCancelled)
    ));
    assert!(matches!(
        super::FfprobeInspector::new("/nonexistent/media-tool")
            .inspect_video_packet_timing_with_control(Path::new("source.mp4"), 0, control),
        Err(MediaError::OperationCancelled)
    ));
}

#[test]
fn media_catalog_sorts_and_filters_real_project_entries() {
    let catalog = MediaCatalog::from_entries(vec![
        MediaCatalogEntry {
            path: Path::new("/media/z-roll.mp4").to_path_buf(),
            kind: MediaItemKind::Video,
            metadata: None,
            fingerprint: None,
            offline: false,
            diagnostic: None,
        },
        MediaCatalogEntry {
            path: Path::new("/media/A-dialogue.wav").to_path_buf(),
            kind: MediaItemKind::Audio,
            metadata: None,
            fingerprint: None,
            offline: false,
            diagnostic: None,
        },
    ]);
    assert_eq!(
        catalog.entries()[0].path,
        Path::new("/media/A-dialogue.wav")
    );
    assert_eq!(
        catalog.search("DIALOGUE", Some(MediaItemKind::Audio)).len(),
        1
    );
    assert!(catalog.search("missing", None).is_empty());
}

#[test]
fn artifact_keys_are_typed_and_deterministic_for_offline_entries() {
    let entry = MediaCatalogEntry {
        path: Path::new("/offline/shot.mov").to_path_buf(),
        kind: MediaItemKind::Video,
        metadata: None,
        fingerprint: None,
        offline: true,
        diagnostic: Some("missing".to_owned()),
    };
    let first = entry.artifact_key(MediaArtifactKind::Thumbnail, b"320x180");
    let second = entry.artifact_key(MediaArtifactKind::Thumbnail, b"320x180");
    let waveform = entry.artifact_key(MediaArtifactKind::Waveform, b"48000");
    assert_eq!(first, second);
    assert_ne!(first, waveform);
    assert_eq!(MediaArtifactKind::Thumbnail.extension(), "rgba");
    assert_eq!(MediaArtifactKind::Waveform.operation(), "waveform");
}

#[test]
fn thumbnail_worker_writes_resized_rgba_artifact_off_thread()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "prism-media-artifact-worker-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let worker = MediaArtifactWorker::spawn(&root, 2);
    let entry = MediaCatalogEntry {
        path: Path::new("/offline/shot.mov").to_path_buf(),
        kind: MediaItemKind::Video,
        metadata: None,
        fingerprint: None,
        offline: true,
        diagnostic: Some("fixture".to_owned()),
    };
    let frame = DecodedVideoFrame {
        width: 2,
        height: 2,
        rgba: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    };
    let key = worker.schedule_thumbnail(&entry, &frame, 1, 1)?;
    let result = worker.result_timeout(std::time::Duration::from_secs(1))?;
    assert_eq!(result.key, key);
    let path = result.result?;
    let bytes = fs::read(path)?;
    assert_eq!(&bytes[..5], b"PRTH\x01");
    assert_eq!(
        worker.read_thumbnail(&entry, 1, 1)?,
        Some(DecodedVideoFrame {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        })
    );
    assert_eq!(worker.read_thumbnail(&entry, 2, 1)?, None);

    let variant_key =
        worker.schedule_thumbnail_variant(&entry, &frame, 2, 1, "browser:max-edge=160")?;
    let variant_result = worker.result_timeout(std::time::Duration::from_secs(1))?;
    assert_eq!(variant_result.key, variant_key);
    variant_result.result?;
    assert_eq!(
        worker.read_thumbnail_variant(&entry, "browser:max-edge=160")?,
        Some(DecodedVideoFrame {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        })
    );
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn cached_thumbnail_decoder_rejects_invalid_artifacts() {
    assert!(decode_thumbnail_artifact(b"raw rgba").is_err());

    let mut truncated = b"PRTH\x01".to_vec();
    truncated.extend_from_slice(&1_u32.to_be_bytes());
    truncated.extend_from_slice(&1_u32.to_be_bytes());
    assert!(decode_thumbnail_artifact(&truncated).is_err());
}

#[test]
fn waveform_worker_serializes_peak_rms_points() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "prism-media-waveform-worker-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let worker = MediaArtifactWorker::spawn(&root, 2);
    let entry = MediaCatalogEntry {
        path: Path::new("/offline/dialogue.wav").to_path_buf(),
        kind: MediaItemKind::Audio,
        metadata: None,
        fingerprint: None,
        offline: true,
        diagnostic: Some("fixture".to_owned()),
    };
    let key = worker.schedule_waveform(&entry, &[-1.0, 0.5, 1.0, 0.0], 2)?;
    let result = worker.result_timeout(std::time::Duration::from_secs(1))?;
    assert_eq!(result.key, key);
    let path = result.result?;
    let bytes = fs::read(path)?;
    assert_eq!(&bytes[..6], b"PRWF\x01\x01");
    assert_eq!(bytes.len(), 10 + 2 * 12);
    let points = worker
        .read_waveform(&entry, 2)?
        .ok_or("waveform cache miss")?;
    assert_eq!(points.len(), 2);
    assert!((points[0].min + 1.0).abs() < f32::EPSILON);
    assert!((points[0].max - 0.5).abs() < f32::EPSILON);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn waveform_decoder_rejects_truncated_and_non_finite_points() {
    assert!(decode_waveform_artifact(b"raw peaks").is_err());

    let mut invalid = b"PRWF\x01\x01".to_vec();
    invalid.extend_from_slice(&1_u32.to_be_bytes());
    invalid.extend_from_slice(&f32::NAN.to_le_bytes());
    invalid.extend_from_slice(&1.0_f32.to_le_bytes());
    invalid.extend_from_slice(&1.0_f32.to_le_bytes());
    assert!(decode_waveform_artifact(&invalid).is_err());
    assert!(decode_stereo_waveform_artifact(&invalid).is_err());
}

#[test]
fn stereo_waveform_worker_preserves_channel_peaks() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "prism-media-stereo-waveform-worker-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let worker = MediaArtifactWorker::spawn(&root, 2);
    let entry = MediaCatalogEntry {
        path: Path::new("/offline/stereo.wav").to_path_buf(),
        kind: MediaItemKind::Audio,
        metadata: None,
        fingerprint: None,
        offline: true,
        diagnostic: Some("fixture".to_owned()),
    };
    let key = worker.schedule_stereo_waveform(&entry, &[-1.0, 0.25, 0.5, 1.0], 2)?;
    let result = worker.result_timeout(std::time::Duration::from_secs(1))?;
    assert_eq!(result.key, key);
    let path = result.result?;
    let bytes = fs::read(path)?;
    assert_eq!(&bytes[..6], b"PRWF\x01\x02");
    assert_eq!(bytes.len(), 34);
    let points = worker
        .read_stereo_waveform(&entry, 2)?
        .ok_or("stereo waveform cache miss")?;
    assert_eq!(points.len(), 1);
    assert!((points[0].left.min + 1.0).abs() < f32::EPSILON);
    assert!((points[0].right.min - 0.25).abs() < f32::EPSILON);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn parses_probe_json_without_float_time() -> Result<(), Box<dyn std::error::Error>> {
    let raw: ProbeOutput = serde_json::from_str(
        r#"{
                "streams":[{
                    "index":0,"codec_name":"h264","codec_type":"video",
                    "width":1920,"height":1080,"pix_fmt":"yuv420p",
                    "r_frame_rate":"30000/1001","avg_frame_rate":"24000/1001",
                    "time_base":"1/90000"
                }],
                "format":{"format_name":"mov,mp4","duration":"1.250000"}
            }"#,
    )?;
    let metadata = MediaMetadata::try_from(raw)?;
    assert_eq!(metadata.duration, Some(RationalTime::new(5, 4)?));
    let StreamKind::Video(video) = &metadata.streams[0].kind else {
        return Err("expected video stream".into());
    };
    assert_eq!(video.frame_rate_evidence, FrameRateEvidence::VariableLikely);
    assert_eq!(
        video.nominal_frame_rate,
        Some(RationalTime::new(30_000, 1_001)?)
    );
    Ok(())
}

#[test]
fn decimal_duration_is_exact() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        parse_decimal_time("duration", "0.033367")?,
        RationalTime::new(33_367, 1_000_000)?
    );
    Ok(())
}

#[test]
fn packet_timing_detects_observed_variation() {
    let report = analyze_packet_timing(&[
        ProbePacket {
            pts: Some(0),
            duration: Some(1_001),
        },
        ProbePacket {
            pts: Some(1_001),
            duration: Some(1_001),
        },
        ProbePacket {
            pts: Some(2_002),
            duration: Some(2_002),
        },
    ]);
    assert_eq!(report.evidence, PacketTimingEvidence::ObservedVariable);
    assert_eq!(report.distinct_duration_ticks, vec![1_001, 2_002]);
}

#[test]
fn seek_timestamp_uses_exact_nanosecond_rounding() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        format_seek_timestamp(RationalTime::new(1_001, 30_000)?)?,
        "0.033366667"
    );
    assert!(matches!(
        format_seek_timestamp(RationalTime::new(-1, 1)?),
        Err(MediaError::NegativeDecodeTimestamp)
    ));
    Ok(())
}

#[test]
fn audio_decode_rejects_unbounded_requests_before_launching_process() {
    let decoder = FfmpegReferenceDecoder::new("unavailable-ffmpeg");
    for (sample_rate, max_samples) in [(0, 1), (48_000, 0), (384_001, 1), (48_000, usize::MAX)] {
        assert!(matches!(
            decoder.decode_audio_mono_f32(
                Path::new("/unavailable/source.mov"),
                0,
                sample_rate,
                max_samples
            ),
            Err(MediaError::InvalidAudioDecodeBounds { .. })
        ));
    }
}

#[test]
fn decoded_frame_size_is_checked_and_bounded() -> Result<(), MediaError> {
    assert_eq!(checked_rgba_length(320, 180)?, 230_400);
    assert!(matches!(
        checked_rgba_length(u32::MAX, u32::MAX),
        Err(MediaError::InvalidFrameDimensions { .. })
    ));
    Ok(())
}

#[test]
fn thumbnail_resize_is_deterministic_and_validates_source() -> Result<(), MediaError> {
    let frame = DecodedVideoFrame {
        width: 2,
        height: 2,
        rgba: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    };
    let thumbnail = frame.thumbnail(1, 1)?;
    assert_eq!(thumbnail.rgba, vec![255, 0, 0, 255]);
    assert_eq!(frame.pixel(0, 0)?, Some([255, 0, 0, 255]));
    assert_eq!(frame.pixel(2, 0)?, None);
    assert_eq!(
        frame.row_rgba(0)?,
        Some(&[255, 0, 0, 255, 0, 255, 0, 255][..])
    );
    let malformed = DecodedVideoFrame {
        rgba: vec![0, 1, 2],
        ..frame
    };
    assert!(matches!(
        malformed.thumbnail(1, 1),
        Err(MediaError::InvalidFrameLength { .. })
    ));
    Ok(())
}

#[test]
fn media_fingerprint_derives_distinct_cache_keys() {
    let fingerprint = MediaFingerprint {
        sha256: "abc123".to_owned(),
        byte_length: 6,
    };
    assert_eq!(fingerprint.sha256_hex(), "abc123");
    assert_eq!(fingerprint.byte_length(), 6);
    assert!(fingerprint.same_content(&MediaFingerprint {
        sha256: "abc123".to_owned(),
        byte_length: 6,
    }));
    assert!(!fingerprint.same_content(&MediaFingerprint {
        sha256: "abc123".to_owned(),
        byte_length: 7,
    }));
    assert!(!fingerprint.same_content(&MediaFingerprint {
        sha256: "different".to_owned(),
        byte_length: 6,
    }));
    let thumbnail = fingerprint.cache_key("thumbnail", "rgba", b"320x180");
    let waveform = fingerprint.cache_key("waveform", "peak-rms", b"48000");
    assert_ne!(thumbnail, waveform);
    assert_eq!(
        thumbnail,
        fingerprint.cache_key("thumbnail", "rgba", b"320x180")
    );
}

#[test]
fn proxy_profiles_validate_and_partition_cache_keys() -> Result<(), Box<dyn std::error::Error>> {
    let fingerprint = MediaFingerprint {
        sha256: "abc123".to_owned(),
        byte_length: 6,
    };
    let profile = ProxyProfile::new(960, 540, "h264", Some("aac".to_owned()), "mp4")?;
    let changed = ProxyProfile::new(640, 360, "h264", Some("aac".to_owned()), "mp4")?;
    assert_ne!(
        fingerprint.proxy_cache_key(&profile),
        fingerprint.proxy_cache_key(&changed)
    );
    assert!(matches!(
        ProxyProfile::new(0, 360, "h264", None, "mp4"),
        Err(MediaError::InvalidProxyProfile { .. })
    ));
    assert!(matches!(
        ProxyProfile::new(960, 540, "h264", None, "../mp4"),
        Err(MediaError::InvalidProxyProfile { .. })
    ));
    Ok(())
}

#[test]
fn media_source_selection_never_silently_falls_back() {
    let original = Path::new("source.mp4");
    let proxy = Path::new("proxy.mp4");
    assert!(matches!(
        select_media_source(original, Some(proxy), MediaSourceSelection::Original),
        Ok(path) if path == original
    ));
    assert!(matches!(
        select_media_source(original, Some(proxy), MediaSourceSelection::Proxy),
        Ok(path) if path == proxy
    ));
    assert!(matches!(
        select_media_source(original, None, MediaSourceSelection::Proxy),
        Err(MediaError::ProxyUnavailable)
    ));
}

#[test]
fn proxy_status_uses_recorded_source_fingerprint() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("prism-proxy-status-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    let source = directory.join("source.mp4");
    let proxy = directory.join("proxy.mp4");
    fs::write(&source, b"source")?;
    fs::write(&proxy, b"proxy")?;
    let recorded = fingerprint(&source)?;
    assert_eq!(
        proxy_status(&source, None, Some(&recorded))?,
        ProxyStatus::Missing
    );
    assert_eq!(
        proxy_status(&source, Some(&proxy), Some(&recorded))?,
        ProxyStatus::Available
    );
    fs::write(&source, b"changed")?;
    assert_eq!(
        proxy_status(&source, Some(&proxy), Some(&recorded))?,
        ProxyStatus::Stale
    );
    assert_eq!(
        proxy_status(&source, Some(&proxy), None)?,
        ProxyStatus::Stale
    );
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn proxy_generator_finalizes_output_and_honors_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicBool;
    let directory = std::env::temp_dir().join(format!("prism-proxy-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory)?;
    let source = directory.join("source.mov");
    let script = directory.join("fake-ffmpeg.sh");
    let output = directory.join("proxy.mp4");
    std::fs::write(&source, b"source")?;
    std::fs::write(&script, b"#!/bin/sh\nlast=''\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf proxy > \"$last\"\n")?;
    let mut permissions = std::fs::metadata(&script)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions)?;
    let profile = ProxyProfile::new(320, 180, "h264", None, "mp4")?;
    let request = ProxyRequest {
        source,
        output: output.clone(),
        profile,
    };
    let generated = FfmpegProxyGenerator::new(&script).generate(&request, None)?;
    assert_eq!(generated, output);
    assert_eq!(std::fs::read(&output)?, b"proxy");
    let cancelled = AtomicBool::new(true);
    let second = ProxyRequest {
        source: request.source.clone(),
        output: directory.join("cancelled.mp4"),
        profile: request.profile.clone(),
    };
    assert!(matches!(
        FfmpegProxyGenerator::new(&script).generate(&second, Some(&cancelled)),
        Err(MediaError::ProxyCancelled)
    ));
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
