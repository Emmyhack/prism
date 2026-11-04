//! Typed media capability and metadata inspection boundaries.
//!
//! The initial adapter invokes `ffprobe` as a reference implementation. Decode,
//! playback, and export will use a separate library-backed worker architecture.

use prism_timeline::{RationalTime, RoundingMode, TimeError};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};
use thiserror::Error;

/// Failures while discovering or inspecting media capabilities.
#[derive(Debug, Error)]
pub enum MediaError {
    /// The configured inspector process could not start.
    #[error("failed to start media inspector: {0}")]
    Process(#[from] std::io::Error),
    /// The inspector rejected or could not parse the input.
    #[error("media inspector failed with status {status}: {stderr}")]
    Inspector {
        /// Process exit status when available.
        status: i32,
        /// Diagnostic output with lossy UTF-8 conversion.
        stderr: String,
    },
    /// A reference decoder rejected the input or failed while decoding.
    #[error("reference decoder failed with status {status}: {stderr}")]
    Decoder {
        /// Process exit status when available.
        status: i32,
        /// Diagnostic output with lossy UTF-8 conversion.
        stderr: String,
    },
    /// Inspector output did not match the pinned JSON contract.
    #[error("invalid inspector JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A rational metadata field was malformed.
    #[error("invalid rational value for {field}: {value}")]
    InvalidRational {
        /// Metadata field name.
        field: &'static str,
        /// Invalid raw value.
        value: String,
    },
    /// Exact time conversion failed.
    #[error("invalid exact time: {0}")]
    Time(#[from] TimeError),
    /// Inspector version output was not valid UTF-8.
    #[error("inspector version output was not UTF-8")]
    InvalidVersionOutput,
    /// Reference decoding only accepts nonnegative sequence timestamps.
    #[error("decode timestamp must not be negative")]
    NegativeDecodeTimestamp,
    /// Requested decoded-frame dimensions were invalid or exceeded the limit.
    #[error("invalid decoded frame dimensions: {width}x{height}")]
    InvalidFrameDimensions {
        /// Requested width in pixels.
        width: u32,
        /// Requested height in pixels.
        height: u32,
    },
    /// The decoder returned a truncated or unexpectedly large frame.
    #[error("decoded RGBA frame had {actual} bytes; expected {expected}")]
    InvalidFrameLength {
        /// Expected byte count for tightly packed RGBA pixels.
        expected: usize,
        /// Actual byte count returned by the process.
        actual: usize,
    },
    /// A proxy profile contained invalid dimensions or codec identifiers.
    #[error("invalid proxy profile: {reason}")]
    InvalidProxyProfile {
        /// Validation failure description.
        reason: &'static str,
    },
    /// Proxy source is not a regular file.
    #[error("proxy source is not a regular file: {0}")]
    InvalidProxySource(PathBuf),
    /// Proxy destination already exists and is never overwritten.
    #[error("proxy destination already exists: {0}")]
    ProxyDestinationExists(PathBuf),
    /// Proxy output parent is unavailable.
    #[error("proxy output parent is unavailable: {0}")]
    InvalidProxyOutputParent(PathBuf),
    /// Proxy generation was cancelled.
    #[error("proxy generation cancelled")]
    ProxyCancelled,
    /// Proxy encoder failed.
    #[error("proxy encoder failed with status {status}: {stderr}")]
    ProxyEncoder {
        /// Process exit status when available.
        status: i32,
        /// Captured diagnostic output.
        stderr: String,
    },
    /// Proxy process succeeded but did not produce a file.
    #[error("proxy encoder produced no output")]
    ProxyOutputMissing,
}

const MAX_REFERENCE_FRAME_BYTES: usize = 512 * 1024 * 1024;

/// Runtime discovery report. This is not proof that a codec workflow works.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCapabilities {
    /// First version-output line.
    pub ffprobe_version: String,
    /// Build configuration reported by `FFmpeg`, if present.
    pub build_configuration: Option<String>,
}

/// Metadata extracted from one media source without decoding full streams.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaMetadata {
    /// Container/demuxer names reported by the inspector.
    pub format_names: Vec<String>,
    /// Exact container duration when available.
    pub duration: Option<RationalTime>,
    /// Ordered media streams.
    pub streams: Vec<StreamMetadata>,
}

/// Metadata common to all media streams.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamMetadata {
    /// Container stream index.
    pub index: u32,
    /// Codec identifier reported by `FFmpeg`.
    pub codec: Option<String>,
    /// Exact stream timebase when meaningful.
    pub time_base: Option<RationalTime>,
    /// Stream-specific metadata.
    pub kind: StreamKind,
}

/// Stream-specific metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamKind {
    /// Visual stream.
    Video(VideoMetadata),
    /// Sampled audio stream.
    Audio(AudioMetadata),
    /// Subtitle, data, attachment, or unknown stream type.
    Other {
        /// Raw `FFmpeg` stream type.
        codec_type: Option<String>,
    },
}

/// Video stream metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoMetadata {
    /// Encoded width in pixels.
    pub width: Option<u32>,
    /// Encoded height in pixels.
    pub height: Option<u32>,
    /// Pixel format identifier.
    pub pixel_format: Option<String>,
    /// Nominal/container frame rate.
    pub nominal_frame_rate: Option<RationalTime>,
    /// Average frame rate across the inspected stream.
    pub average_frame_rate: Option<RationalTime>,
    /// Evidence-based VFR classification, not a decoder guarantee.
    pub frame_rate_evidence: FrameRateEvidence,
    /// Color primaries identifier.
    pub color_primaries: Option<String>,
    /// Transfer function identifier.
    pub color_transfer: Option<String>,
    /// Matrix/color-space identifier.
    pub color_space: Option<String>,
    /// Rotation metadata in degrees when present.
    pub rotation_degrees: Option<i32>,
    /// Embedded source timecode when present.
    pub timecode: Option<String>,
}

/// Audio stream metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioMetadata {
    /// Sample rate in Hz.
    pub sample_rate: Option<u32>,
    /// Channel count.
    pub channels: Option<u32>,
    /// `FFmpeg` channel-layout identifier.
    pub channel_layout: Option<String>,
}

/// What header-level rate metadata suggests about variable frame rate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameRateEvidence {
    /// Nominal and average rates differ.
    VariableLikely,
    /// Nominal and average rates are present and equal.
    ConstantLikely,
    /// Insufficient metadata; packet timestamp analysis is required.
    Unknown,
}

/// Packet-level timing evidence observed across an inspected video stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacketTimingEvidence {
    /// Every observed positive duration and timestamp delta was identical.
    ObservedConstant,
    /// At least two observed positive durations or timestamp deltas differed.
    ObservedVariable,
    /// Too few usable packets were available to classify timing.
    Insufficient,
}

/// Packet timing summary for one selected video stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PacketTimingReport {
    /// Number of packets returned by the inspector.
    pub packet_count: usize,
    /// Distinct positive packet durations expressed in stream ticks.
    pub distinct_duration_ticks: Vec<i64>,
    /// Evidence derived from packet durations and presentation timestamps.
    pub evidence: PacketTimingEvidence,
}

/// Full-file SHA-256 fingerprint for relinking and change detection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaFingerprint {
    /// Lowercase hexadecimal SHA-256 digest.
    pub sha256: String,
    /// Number of bytes hashed.
    pub byte_length: u64,
}

/// Deterministic proxy-generation settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyProfile {
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
    /// Video codec identifier passed to the encoder.
    pub video_codec: String,
    /// Optional audio codec identifier.
    pub audio_codec: Option<String>,
    /// Cache artifact extension.
    pub extension: String,
}

/// A nondestructive proxy-generation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyRequest {
    /// Original media source.
    pub source: PathBuf,
    /// No-clobber proxy destination.
    pub output: PathBuf,
    /// Encoding profile.
    pub profile: ProxyProfile,
}

/// Explicit `FFmpeg` proxy generator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FfmpegProxyGenerator {
    executable: PathBuf,
}

impl FfmpegProxyGenerator {
    /// Creates a generator using an explicitly configured `FFmpeg` executable.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Generates a proxy with temporary-output cleanup and no-clobber finalization.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when validation, process execution, cancellation,
    /// or finalization fails.
    pub fn generate(
        &self,
        request: &ProxyRequest,
        cancelled: Option<&AtomicBool>,
    ) -> Result<PathBuf, MediaError> {
        if !request.source.is_file() {
            return Err(MediaError::InvalidProxySource(request.source.clone()));
        }
        if request.output.exists() {
            return Err(MediaError::ProxyDestinationExists(request.output.clone()));
        }
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(MediaError::ProxyCancelled);
        }
        let parent = request
            .output
            .parent()
            .ok_or_else(|| MediaError::InvalidProxyOutputParent(request.output.clone()))?;
        if !parent.is_dir() {
            return Err(MediaError::InvalidProxyOutputParent(parent.to_path_buf()));
        }
        let temporary = request.output.with_file_name(format!(
            ".{}.proxy-{}.tmp",
            request
                .output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("proxy"),
            std::process::id()
        ));
        let mut command = Command::new(&self.executable);
        command
            .args(["-v", "error", "-y", "-i"])
            .arg(&request.source)
            .args([
                "-vf",
                &format!("scale={}:{}", request.profile.width, request.profile.height),
                "-c:v",
            ])
            .arg(&request.profile.video_codec);
        if let Some(audio) = &request.profile.audio_codec {
            command.args(["-c:a", audio]);
        } else {
            command.arg("-an");
        }
        command
            .arg(&temporary)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| MediaError::ProxyEncoder {
            status: -1,
            stderr: error.to_string(),
        })?;
        let output = wait_proxy_child(&mut child, cancelled)?;
        if !output.status.success() {
            let _ = fs::remove_file(&temporary);
            return Err(MediaError::ProxyEncoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if !temporary.is_file() {
            return Err(MediaError::ProxyOutputMissing);
        }
        let result = fs::hard_link(&temporary, &request.output);
        let _ = fs::remove_file(&temporary);
        result.map_err(|error| {
            if request.output.exists() {
                MediaError::ProxyDestinationExists(request.output.clone())
            } else {
                MediaError::ProxyEncoder {
                    status: -1,
                    stderr: error.to_string(),
                }
            }
        })?;
        Ok(request.output.clone())
    }
}

fn wait_proxy_child(
    child: &mut Child,
    cancelled: Option<&AtomicBool>,
) -> Result<std::process::Output, MediaError> {
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::ProxyCancelled);
        }
        if child
            .try_wait()
            .map_err(|error| MediaError::ProxyEncoder {
                status: -1,
                stderr: error.to_string(),
            })?
            .is_some()
        {
            let status = child.wait().map_err(|error| MediaError::ProxyEncoder {
                status: -1,
                stderr: error.to_string(),
            })?;
            let mut stderr = Vec::new();
            if let Some(mut stream) = child.stderr.take() {
                stream
                    .read_to_end(&mut stderr)
                    .map_err(|error| MediaError::ProxyEncoder {
                        status: -1,
                        stderr: error.to_string(),
                    })?;
            }
            return Ok(std::process::Output {
                status,
                stdout: Vec::new(),
                stderr,
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

impl ProxyProfile {
    /// Creates a validated proxy profile.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidProxyProfile`] when dimensions or codec
    /// identifiers are empty/unsafe.
    pub fn new(
        width: u32,
        height: u32,
        video_codec: impl Into<String>,
        audio_codec: Option<String>,
        extension: impl Into<String>,
    ) -> Result<Self, MediaError> {
        let profile = Self {
            width,
            height,
            video_codec: video_codec.into(),
            audio_codec,
            extension: extension.into(),
        };
        if profile.width == 0 || profile.height == 0 {
            return Err(MediaError::InvalidProxyProfile {
                reason: "dimensions must be nonzero",
            });
        }
        for value in std::iter::once(&profile.video_codec)
            .chain(profile.audio_codec.as_ref())
            .chain(std::iter::once(&profile.extension))
        {
            if value.is_empty()
                || !value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
            {
                return Err(MediaError::InvalidProxyProfile {
                    reason: "codec and extension identifiers must be filename-safe",
                });
            }
        }
        Ok(profile)
    }

    /// Returns canonical bytes for cache-key derivation.
    #[must_use]
    pub fn cache_parameters(&self) -> Vec<u8> {
        format!(
            "{}x{};v={};a={};ext={}",
            self.width,
            self.height,
            self.video_codec,
            self.audio_codec.as_deref().unwrap_or(""),
            self.extension
        )
        .into_bytes()
    }
}

/// One tightly packed, top-to-bottom RGBA frame from the reference decoder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedVideoFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Four bytes per pixel in red, green, blue, alpha order.
    pub rgba: Vec<u8>,
}

impl DecodedVideoFrame {
    /// Returns a lowercase SHA-256 digest of the decoded pixel bytes.
    #[must_use]
    pub fn sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.rgba))
    }

    /// Creates a deterministic nearest-neighbor thumbnail in RGBA order.
    ///
    /// This CPU helper is intentionally simple and stable for cache fixtures;
    /// quality filters and GPU scaling belong to later render backends.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidFrameDimensions`] for invalid target or
    /// source dimensions, or [`MediaError::InvalidFrameLength`] when the
    /// source bytes do not match its declared dimensions.
    #[allow(clippy::cast_possible_truncation)]
    pub fn thumbnail(&self, width: u32, height: u32) -> Result<Self, MediaError> {
        let expected_source = checked_rgba_length(self.width, self.height)?;
        if self.rgba.len() != expected_source {
            return Err(MediaError::InvalidFrameLength {
                expected: expected_source,
                actual: self.rgba.len(),
            });
        }
        let target_length = checked_rgba_length(width, height)?;
        let mut rgba = vec![0_u8; target_length];
        let source_width = u64::from(self.width);
        let source_height = u64::from(self.height);
        let target_width = u64::from(width);
        let target_height = u64::from(height);
        for y in 0..target_height {
            let source_y = y * source_height / target_height;
            for x in 0..target_width {
                let source_x = x * source_width / target_width;
                let source_index = ((source_y * source_width + source_x) * 4) as usize;
                let target_index = ((y * target_width + x) * 4) as usize;
                rgba[target_index..target_index + 4]
                    .copy_from_slice(&self.rgba[source_index..source_index + 4]);
            }
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }
}

impl MediaFingerprint {
    /// Derives a content-addressed key for a media-derived artifact.
    ///
    /// The operation and parameters must describe every setting that affects
    /// the artifact (for example, thumbnail dimensions or waveform rate).
    #[must_use]
    pub fn cache_key(
        &self,
        namespace: &str,
        operation: &str,
        parameters: &[u8],
    ) -> prism_cache::CacheKey {
        prism_cache::CacheKey::from_parts(namespace, self.sha256.as_bytes(), operation, parameters)
    }

    /// Derives the stable cache key for a proxy profile.
    #[must_use]
    pub fn proxy_cache_key(&self, profile: &ProxyProfile) -> prism_cache::CacheKey {
        self.cache_key("proxy", "transcode", &profile.cache_parameters())
    }
}

/// Computes a deterministic full-file fingerprint in bounded memory.
///
/// # Errors
///
/// Returns [`MediaError::Process`] when the file cannot be opened or read.
pub fn fingerprint(path: &Path) -> Result<MediaFingerprint, MediaError> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_length = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        byte_length = byte_length
            .checked_add(
                u64::try_from(read).map_err(|error| {
                    MediaError::Process(std::io::Error::other(error.to_string()))
                })?,
            )
            .ok_or_else(|| MediaError::Process(std::io::Error::other("file length overflow")))?;
    }
    Ok(MediaFingerprint {
        sha256: format!("{:x}", digest.finalize()),
        byte_length,
    })
}

/// `ffprobe` process adapter with no ambient shell invocation.
#[derive(Clone, Debug)]
pub struct FfprobeInspector {
    executable: PathBuf,
}

/// `FFmpeg` command-line adapter used to establish decode and seek test vectors.
///
/// This is intentionally separate from the future isolated, library-backed
/// playback decoder and must not be used on the UI thread.
#[derive(Clone, Debug)]
pub struct FfmpegReferenceDecoder {
    executable: PathBuf,
}

impl FfmpegReferenceDecoder {
    /// Creates a decoder using an explicit executable path.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Decodes the first frame at or after an exact, nonnegative timestamp.
    ///
    /// `video_ordinal` is zero-based among video streams. The caller supplies
    /// dimensions obtained from inspected metadata; returned bytes are checked
    /// against those dimensions before a frame is constructed.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for invalid bounds, time conversion, process
    /// failure, or output that is not one tightly packed RGBA frame.
    pub fn decode_video_frame_rgba(
        &self,
        path: &Path,
        video_ordinal: u32,
        timestamp: RationalTime,
        width: u32,
        height: u32,
    ) -> Result<DecodedVideoFrame, MediaError> {
        let expected = checked_rgba_length(width, height)?;
        let seek = format_seek_timestamp(timestamp)?;
        let selection = format!("0:v:{video_ordinal}");
        let output = Command::new(&self.executable)
            .args(["-v", "error", "-ss", &seek, "-i"])
            .arg(path)
            .args([
                "-map",
                &selection,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .output()?;
        if !output.status.success() {
            return Err(MediaError::Decoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if output.stdout.len() != expected {
            return Err(MediaError::InvalidFrameLength {
                expected,
                actual: output.stdout.len(),
            });
        }
        Ok(DecodedVideoFrame {
            width,
            height,
            rgba: output.stdout,
        })
    }
}

fn checked_rgba_length(width: u32, height: u32) -> Result<usize, MediaError> {
    let length = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4));
    match length {
        Some(length) if length > 0 && length <= MAX_REFERENCE_FRAME_BYTES => Ok(length),
        _ => Err(MediaError::InvalidFrameDimensions { width, height }),
    }
}

fn format_seek_timestamp(timestamp: RationalTime) -> Result<String, MediaError> {
    let nanoseconds = timestamp.to_ticks(1_000_000_000, RoundingMode::NearestTiesAway)?;
    if nanoseconds < 0 {
        return Err(MediaError::NegativeDecodeTimestamp);
    }
    let seconds = nanoseconds / 1_000_000_000;
    let subsecond = nanoseconds % 1_000_000_000;
    Ok(format!("{seconds}.{subsecond:09}"))
}

impl FfprobeInspector {
    /// Creates an inspector using an explicit executable path.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Reports the installed tool version and build configuration.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when the process cannot run or reports failure.
    pub fn capabilities(&self) -> Result<RuntimeCapabilities, MediaError> {
        let output = Command::new(&self.executable).arg("-version").output()?;
        ensure_success(&output)?;
        let stdout =
            String::from_utf8(output.stdout).map_err(|_| MediaError::InvalidVersionOutput)?;
        let mut lines = stdout.lines();
        let version = lines.next().unwrap_or_default().to_owned();
        let configuration = lines
            .find_map(|line| line.strip_prefix("configuration: "))
            .map(str::to_owned);
        Ok(RuntimeCapabilities {
            ffprobe_version: version,
            build_configuration: configuration,
        })
    }

    /// Inspects container and stream metadata without decoding full media.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for process, input, JSON, or exact-time failures.
    pub fn inspect(&self, path: &Path) -> Result<MediaMetadata, MediaError> {
        let output = Command::new(&self.executable)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=format_name,duration:stream=index,codec_type,codec_name,width,height,pix_fmt,r_frame_rate,avg_frame_rate,time_base,sample_rate,channels,channel_layout,color_space,color_transfer,color_primaries:stream_tags=rotate,timecode",
                "-of",
                "json",
            ])
            .arg(path)
            .output()?;
        ensure_success(&output)?;
        let raw: ProbeOutput = serde_json::from_slice(&output.stdout)?;
        raw.try_into()
    }

    /// Inspects packet timestamps for one zero-based video-stream ordinal.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for process, input, or JSON failures.
    pub fn inspect_video_packet_timing(
        &self,
        path: &Path,
        video_ordinal: u32,
    ) -> Result<PacketTimingReport, MediaError> {
        let selection = format!("v:{video_ordinal}");
        let output = Command::new(&self.executable)
            .args([
                "-v",
                "error",
                "-select_streams",
                &selection,
                "-show_entries",
                "packet=pts,duration",
                "-of",
                "json",
            ])
            .arg(path)
            .output()?;
        ensure_success(&output)?;
        let raw: PacketOutput = serde_json::from_slice(&output.stdout)?;
        Ok(analyze_packet_timing(&raw.packets))
    }
}

fn ensure_success(output: &std::process::Output) -> Result<(), MediaError> {
    if output.status.success() {
        return Ok(());
    }
    Err(MediaError::Inspector {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    index: u32,
    codec_name: Option<String>,
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    time_base: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    tags: Option<ProbeTags>,
}

#[derive(Debug, Deserialize)]
struct ProbeTags {
    rotate: Option<String>,
    timecode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PacketOutput {
    #[serde(default)]
    packets: Vec<ProbePacket>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct ProbePacket {
    pts: Option<i64>,
    duration: Option<i64>,
}

fn analyze_packet_timing(packets: &[ProbePacket]) -> PacketTimingReport {
    let durations = packets
        .iter()
        .filter_map(|packet| packet.duration)
        .filter(|duration| *duration > 0)
        .collect::<BTreeSet<_>>();
    let deltas = packets
        .windows(2)
        .filter_map(|window| window[1].pts?.checked_sub(window[0].pts?))
        .filter(|delta| *delta > 0)
        .collect::<BTreeSet<_>>();
    let usable_intervals = packets.len().saturating_sub(1);
    let evidence = if usable_intervals == 0 || (durations.is_empty() && deltas.is_empty()) {
        PacketTimingEvidence::Insufficient
    } else if durations.len() > 1 || deltas.len() > 1 {
        PacketTimingEvidence::ObservedVariable
    } else {
        PacketTimingEvidence::ObservedConstant
    };
    PacketTimingReport {
        packet_count: packets.len(),
        distinct_duration_ticks: durations.into_iter().collect(),
        evidence,
    }
}

impl TryFrom<ProbeOutput> for MediaMetadata {
    type Error = MediaError;

    fn try_from(raw: ProbeOutput) -> Result<Self, Self::Error> {
        let format_names = raw
            .format
            .as_ref()
            .and_then(|format| format.format_name.as_deref())
            .map(|names| names.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        let duration = raw
            .format
            .and_then(|format| format.duration)
            .map(|value| parse_decimal_time("format.duration", &value))
            .transpose()?;
        let streams = raw
            .streams
            .into_iter()
            .map(StreamMetadata::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            format_names,
            duration,
            streams,
        })
    }
}

impl TryFrom<ProbeStream> for StreamMetadata {
    type Error = MediaError;

    fn try_from(raw: ProbeStream) -> Result<Self, Self::Error> {
        let time_base = parse_optional_fraction("stream.time_base", raw.time_base.as_deref())?;
        let kind = match raw.codec_type.as_deref() {
            Some("video") => {
                let nominal =
                    parse_optional_fraction("stream.r_frame_rate", raw.r_frame_rate.as_deref())?;
                let average = parse_optional_fraction(
                    "stream.avg_frame_rate",
                    raw.avg_frame_rate.as_deref(),
                )?;
                StreamKind::Video(VideoMetadata {
                    width: raw.width,
                    height: raw.height,
                    pixel_format: raw.pix_fmt,
                    nominal_frame_rate: nominal,
                    average_frame_rate: average,
                    frame_rate_evidence: classify_frame_rate(nominal, average),
                    color_primaries: raw.color_primaries,
                    color_transfer: raw.color_transfer,
                    color_space: raw.color_space,
                    rotation_degrees: raw
                        .tags
                        .as_ref()
                        .and_then(|tags| tags.rotate.as_deref())
                        .and_then(|value| value.parse().ok()),
                    timecode: raw.tags.and_then(|tags| tags.timecode),
                })
            }
            Some("audio") => StreamKind::Audio(AudioMetadata {
                sample_rate: raw
                    .sample_rate
                    .as_deref()
                    .and_then(|value| value.parse().ok()),
                channels: raw.channels,
                channel_layout: raw.channel_layout,
            }),
            _ => StreamKind::Other {
                codec_type: raw.codec_type,
            },
        };
        Ok(Self {
            index: raw.index,
            codec: raw.codec_name,
            time_base,
            kind,
        })
    }
}

fn classify_frame_rate(
    nominal: Option<RationalTime>,
    average: Option<RationalTime>,
) -> FrameRateEvidence {
    match (nominal, average) {
        (Some(left), Some(right)) if left == right => FrameRateEvidence::ConstantLikely,
        (Some(_), Some(_)) => FrameRateEvidence::VariableLikely,
        _ => FrameRateEvidence::Unknown,
    }
}

fn parse_optional_fraction(
    field: &'static str,
    value: Option<&str>,
) -> Result<Option<RationalTime>, MediaError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some((numerator, denominator)) = value.split_once('/') else {
        return Err(MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        });
    };
    let numerator = numerator
        .parse::<i64>()
        .map_err(|_| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    let denominator = denominator
        .parse::<u32>()
        .map_err(|_| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    if denominator == 0 {
        return Ok(None);
    }
    Ok(Some(RationalTime::new(numerator, denominator)?))
}

fn parse_decimal_time(field: &'static str, value: &str) -> Result<RationalTime, MediaError> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 9 || whole.starts_with('-') {
        return Err(MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        });
    }
    let exponent = u32::try_from(fraction.len()).map_err(|_| MediaError::InvalidRational {
        field,
        value: value.to_owned(),
    })?;
    let scale = 10_u32
        .checked_pow(exponent)
        .ok_or_else(|| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    let whole = whole
        .parse::<i64>()
        .map_err(|_| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i64>()
            .map_err(|_| MediaError::InvalidRational {
                field,
                value: value.to_owned(),
            })?
    };
    let ticks = whole
        .checked_mul(i64::from(scale))
        .and_then(|integer| integer.checked_add(fraction))
        .ok_or_else(|| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    Ok(RationalTime::new(ticks, scale)?)
}

#[cfg(test)]
mod tests {
    use super::{
        DecodedVideoFrame, FfmpegProxyGenerator, FrameRateEvidence, MediaError, MediaFingerprint,
        MediaMetadata, PacketTimingEvidence, ProbeOutput, ProbePacket, ProxyProfile, ProxyRequest,
        StreamKind, analyze_packet_timing, checked_rgba_length, format_seek_timestamp,
        parse_decimal_time,
    };
    use prism_timeline::RationalTime;

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
        let thumbnail = fingerprint.cache_key("thumbnail", "rgba", b"320x180");
        let waveform = fingerprint.cache_key("waveform", "peak-rms", b"48000");
        assert_ne!(thumbnail, waveform);
        assert_eq!(
            thumbnail,
            fingerprint.cache_key("thumbnail", "rgba", b"320x180")
        );
    }

    #[test]
    fn proxy_profiles_validate_and_partition_cache_keys() -> Result<(), Box<dyn std::error::Error>>
    {
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

    #[cfg(unix)]
    #[test]
    fn proxy_generator_finalizes_output_and_honors_cancellation()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicBool;
        let directory =
            std::env::temp_dir().join(format!("prism-proxy-test-{}", std::process::id()));
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
}
