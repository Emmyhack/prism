//! Typed media capability and metadata inspection boundaries.
//!
//! The initial adapter invokes `ffprobe` as a reference implementation. Decode,
//! playback, and export will use a separate library-backed worker architecture.

mod supervised_process;
pub use supervised_process::MediaProcessControl;

use prism_cache::{
    CacheError, CacheLayout, CacheSubmitError, CacheWorker, CacheWriteRequest, CacheWriteResult,
};
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
    /// A caller cancelled a supervised media subprocess.
    #[error("media operation cancelled")]
    OperationCancelled,
    /// A supervised media subprocess exceeded its deadline.
    #[error("media operation timed out")]
    OperationTimedOut,
    /// A supervised tool produced more output than the caller permits.
    #[error("media {stream} exceeded its {maximum}-byte output limit")]
    ProcessOutputLimit {
        /// Output stream that exceeded its limit.
        stream: &'static str,
        /// Maximum permitted capture size.
        maximum: usize,
    },
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
    /// Audio decode bounds were zero or could not be represented safely.
    #[error("invalid audio decode bounds: {sample_rate} Hz, {max_samples} samples")]
    InvalidAudioDecodeBounds {
        /// Requested output sample rate.
        sample_rate: u32,
        /// Maximum decoded mono samples.
        max_samples: usize,
    },
    /// The decoder returned malformed or excessive mono `f32` PCM bytes.
    #[error("decoded mono audio had {actual} bytes; maximum valid length is {maximum}")]
    InvalidAudioFrameLength {
        /// Maximum permitted byte count.
        maximum: usize,
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
    /// Proxy playback was requested without an available relinked proxy.
    #[error("proxy media is unavailable")]
    ProxyUnavailable,
}

const MAX_REFERENCE_FRAME_BYTES: usize = 512 * 1024 * 1024;
const MAX_REFERENCE_AUDIO_BYTES: usize = 512 * 1024 * 1024;

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

mod catalog;
mod probe;
mod proxy;

pub use catalog::{
    AudioMetadata, FrameRateEvidence, MediaArtifactError, MediaArtifactKind, MediaArtifactWorker,
    MediaCatalog, MediaCatalogEntry, MediaItemKind, PacketTimingEvidence, PacketTimingReport,
    StreamKind, StreamMetadata, VideoMetadata, decode_stereo_waveform_artifact,
    decode_thumbnail_artifact, decode_waveform_artifact,
};
pub use probe::{
    DecodedVideoFrame, FfmpegReferenceDecoder, FfprobeInspector, fingerprint,
    fingerprint_with_control,
};
pub use proxy::{
    FfmpegProxyGenerator, MediaFingerprint, MediaSourceSelection, ProxyProfile, ProxyRequest,
    ProxyStatus, proxy_status, select_media_source,
};

use probe::checked_rgba_length;

#[cfg(test)]
use probe::{
    ProbeOutput, ProbePacket, analyze_packet_timing, format_seek_timestamp, parse_decimal_time,
};

#[cfg(test)]
mod tests;
