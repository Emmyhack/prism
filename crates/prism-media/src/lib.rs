//! Typed media capability and metadata inspection boundaries.
//!
//! The initial adapter invokes `ffprobe` as a reference implementation. Decode,
//! playback, and export will use a separate library-backed worker architecture.

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

/// Broad media classification used by project browsers and import filters.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MediaItemKind {
    /// Video-bearing source.
    Video,
    /// Audio-only source.
    Audio,
    /// Still image or image sequence.
    Image,
    /// Unknown or unsupported source.
    Unknown,
}

/// One real filesystem item presented by the media catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaCatalogEntry {
    /// Source path.
    pub path: PathBuf,
    /// Classification derived from probed streams or extension.
    pub kind: MediaItemKind,
    /// Probed metadata when inspection succeeded.
    pub metadata: Option<MediaMetadata>,
    /// Content fingerprint when the file was readable.
    pub fingerprint: Option<MediaFingerprint>,
    /// Whether the path is currently unavailable or inspection failed.
    pub offline: bool,
    /// Actionable probe diagnostic, when one exists.
    pub diagnostic: Option<String>,
}

/// Stable derived-artifact families exposed to media browsers and background
/// workers.  The enum keeps UI requests typed instead of passing ad-hoc cache
/// operation strings between panels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaArtifactKind {
    /// A representative RGBA frame for an asset tile.
    Thumbnail,
    /// Downsampled peak/RMS data for an audio or video clip.
    Waveform,
    /// A decoded preview frame used by a monitor.
    PreviewFrame,
}

/// Errors returned while scheduling a derived media artifact.
#[derive(Debug, Error)]
pub enum MediaArtifactError {
    /// Thumbnail dimensions or source bytes were invalid.
    #[error("could not create media artifact: {0}")]
    Media(#[from] MediaError),
    /// Audio samples failed waveform validation.
    #[error("could not generate waveform: {0}")]
    Audio(#[from] prism_audio::AudioError),
    /// The bounded artifact queue applied backpressure or stopped.
    #[error("could not queue media artifact: {0}")]
    Queue(#[from] CacheSubmitError),
    /// A finalized cache artifact could not be read safely.
    #[error("could not read media artifact: {0}")]
    Cache(#[from] CacheError),
    /// Cached thumbnail bytes did not match the versioned artifact contract.
    #[error("invalid cached thumbnail artifact: {0}")]
    InvalidThumbnail(&'static str),
    /// Cached waveform bytes did not match the versioned artifact contract.
    #[error("invalid cached waveform artifact: {0}")]
    InvalidWaveform(&'static str),
}

/// Bounded asynchronous writer for derived media artifacts.
pub struct MediaArtifactWorker {
    worker: CacheWorker,
    layout: CacheLayout,
}

impl MediaArtifactWorker {
    /// Starts a worker rooted at `cache_root` with bounded queue capacity.
    #[must_use]
    pub fn spawn(cache_root: impl Into<PathBuf>, queue_capacity: usize) -> Self {
        let layout = CacheLayout::new(cache_root);
        Self {
            worker: CacheWorker::spawn(layout.clone(), queue_capacity),
            layout,
        }
    }

    /// Queues a deterministic RGBA thumbnail write from a decoded frame.
    ///
    /// The source frame is resized before it enters the queue, keeping media
    /// decoding and cache I/O off the UI thread while preserving backpressure.
    ///
    /// # Errors
    ///
    /// Returns an error when frame dimensions are invalid or the bounded queue
    /// is full or stopped.
    pub fn schedule_thumbnail(
        &self,
        entry: &MediaCatalogEntry,
        frame: &DecodedVideoFrame,
        width: u32,
        height: u32,
    ) -> Result<prism_cache::CacheKey, MediaArtifactError> {
        self.schedule_thumbnail_variant(entry, frame, width, height, &format!("{width}x{height}"))
    }

    /// Queues a resized thumbnail under a stable presentation variant.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid frame dimensions or queue backpressure.
    pub fn schedule_thumbnail_variant(
        &self,
        entry: &MediaCatalogEntry,
        frame: &DecodedVideoFrame,
        width: u32,
        height: u32,
        variant: &str,
    ) -> Result<prism_cache::CacheKey, MediaArtifactError> {
        let thumbnail = frame.thumbnail(width, height)?;
        let key = Self::thumbnail_variant_key(entry, variant);
        let contents = encode_thumbnail_artifact(&thumbnail);
        self.worker.try_submit(CacheWriteRequest {
            key,
            extension: MediaArtifactKind::Thumbnail.extension().to_owned(),
            contents,
        })?;
        Ok(key)
    }

    /// Reads and validates a versioned cached thumbnail for exact dimensions.
    ///
    /// Legacy raw-RGBA entries are treated as invalid rather than guessing their
    /// dimensions. Callers may safely regenerate them through `schedule_thumbnail`.
    ///
    /// # Errors
    ///
    /// Returns an error for cache I/O or an invalid versioned artifact.
    pub fn read_thumbnail(
        &self,
        entry: &MediaCatalogEntry,
        width: u32,
        height: u32,
    ) -> Result<Option<DecodedVideoFrame>, MediaArtifactError> {
        let thumbnail = self.read_thumbnail_variant(entry, &format!("{width}x{height}"))?;
        if thumbnail
            .as_ref()
            .is_some_and(|frame| frame.width != width || frame.height != height)
        {
            return Err(MediaArtifactError::InvalidThumbnail(
                "stored dimensions do not match cache key",
            ));
        }
        Ok(thumbnail)
    }

    /// Reads a validated cached thumbnail stored under a presentation variant.
    ///
    /// # Errors
    ///
    /// Returns an error for cache I/O or an invalid versioned artifact.
    pub fn read_thumbnail_variant(
        &self,
        entry: &MediaCatalogEntry,
        variant: &str,
    ) -> Result<Option<DecodedVideoFrame>, MediaArtifactError> {
        let key = Self::thumbnail_variant_key(entry, variant);
        let Some(contents) = self.read_artifact(key, MediaArtifactKind::Thumbnail)? else {
            return Ok(None);
        };
        decode_thumbnail_artifact(&contents).map(Some)
    }

    /// Returns the content-addressed key for one thumbnail presentation variant.
    #[must_use]
    pub fn thumbnail_variant_key(
        entry: &MediaCatalogEntry,
        variant: &str,
    ) -> prism_cache::CacheKey {
        entry.artifact_key(
            MediaArtifactKind::Thumbnail,
            format!("v1:{variant}").as_bytes(),
        )
    }

    /// Queues a deterministic mono peak/RMS waveform artifact.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid samples/window sizes or queue backpressure.
    pub fn schedule_waveform(
        &self,
        entry: &MediaCatalogEntry,
        samples: &[f32],
        samples_per_point: usize,
    ) -> Result<prism_cache::CacheKey, MediaArtifactError> {
        self.schedule_waveform_variant(entry, samples, samples_per_point, "mono")
    }

    /// Queues a mono waveform under a caller-defined decode variant.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid samples, an excessive point count, or queue backpressure.
    pub fn schedule_waveform_variant(
        &self,
        entry: &MediaCatalogEntry,
        samples: &[f32],
        samples_per_point: usize,
        variant: &str,
    ) -> Result<prism_cache::CacheKey, MediaArtifactError> {
        let points = prism_audio::generate_waveform(samples, samples_per_point)?;
        let point_count = u32::try_from(points.len())
            .map_err(|_| MediaArtifactError::InvalidWaveform("too many waveform points"))?;
        let mut contents = Vec::with_capacity(10usize.saturating_add(points.len() * 12));
        contents.extend_from_slice(b"PRWF");
        contents.push(1);
        contents.push(1);
        contents.extend_from_slice(&point_count.to_be_bytes());
        for point in points {
            contents.extend_from_slice(&point.min.to_le_bytes());
            contents.extend_from_slice(&point.max.to_le_bytes());
            contents.extend_from_slice(&point.rms.to_le_bytes());
        }
        let key = Self::waveform_variant_key(entry, samples_per_point, variant);
        self.worker.try_submit(CacheWriteRequest {
            key,
            extension: MediaArtifactKind::Waveform.extension().to_owned(),
            contents,
        })?;
        Ok(key)
    }

    /// Reads a validated versioned mono waveform, returning `None` on a miss.
    ///
    /// # Errors
    ///
    /// Returns an error for cache I/O or an invalid versioned artifact.
    pub fn read_waveform(
        &self,
        entry: &MediaCatalogEntry,
        samples_per_point: usize,
    ) -> Result<Option<Vec<prism_audio::WaveformPoint>>, MediaArtifactError> {
        self.read_waveform_variant(entry, samples_per_point, "mono")
    }

    /// Reads a validated mono waveform stored under a decode variant.
    ///
    /// # Errors
    ///
    /// Returns an error for cache I/O or an invalid versioned artifact.
    pub fn read_waveform_variant(
        &self,
        entry: &MediaCatalogEntry,
        samples_per_point: usize,
        variant: &str,
    ) -> Result<Option<Vec<prism_audio::WaveformPoint>>, MediaArtifactError> {
        let key = entry.artifact_key(
            MediaArtifactKind::Waveform,
            format!("v1:{variant}:{samples_per_point}").as_bytes(),
        );
        let Some(contents) = self.read_artifact(key, MediaArtifactKind::Waveform)? else {
            return Ok(None);
        };
        decode_waveform_artifact(&contents).map(Some)
    }

    /// Returns the content-addressed key for one mono waveform decode variant.
    #[must_use]
    pub fn waveform_variant_key(
        entry: &MediaCatalogEntry,
        samples_per_point: usize,
        variant: &str,
    ) -> prism_cache::CacheKey {
        entry.artifact_key(
            MediaArtifactKind::Waveform,
            format!("v1:{variant}:{samples_per_point}").as_bytes(),
        )
    }

    /// Removes one exact derived artifact so failed validation can regenerate it.
    ///
    /// # Errors
    ///
    /// Returns a cache filesystem or extension validation error.
    pub fn remove_artifact(
        &self,
        key: prism_cache::CacheKey,
        artifact: MediaArtifactKind,
    ) -> Result<bool, prism_cache::CacheError> {
        self.layout.remove(key, artifact.extension())
    }

    /// Queues a deterministic stereo peak/RMS waveform artifact.
    ///
    /// Each point is serialized as six little-endian `f32` values in the
    /// order left-min, left-max, left-rms, right-min, right-max, right-rms.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid samples/window sizes or queue backpressure.
    pub fn schedule_stereo_waveform(
        &self,
        entry: &MediaCatalogEntry,
        interleaved_samples: &[f32],
        frames_per_point: usize,
    ) -> Result<prism_cache::CacheKey, MediaArtifactError> {
        let points = prism_audio::generate_stereo_waveform(interleaved_samples, frames_per_point)?;
        let point_count = u32::try_from(points.len())
            .map_err(|_| MediaArtifactError::InvalidWaveform("too many waveform points"))?;
        let mut contents = Vec::with_capacity(10usize.saturating_add(points.len() * 24));
        contents.extend_from_slice(b"PRWF");
        contents.push(1);
        contents.push(2);
        contents.extend_from_slice(&point_count.to_be_bytes());
        for point in points {
            for value in [
                point.left.min,
                point.left.max,
                point.left.rms,
                point.right.min,
                point.right.max,
                point.right.rms,
            ] {
                contents.extend_from_slice(&value.to_le_bytes());
            }
        }
        let key = entry.artifact_key(
            MediaArtifactKind::Waveform,
            format!("v1:stereo:{frames_per_point}").as_bytes(),
        );
        self.worker.try_submit(CacheWriteRequest {
            key,
            extension: MediaArtifactKind::Waveform.extension().to_owned(),
            contents,
        })?;
        Ok(key)
    }

    /// Reads a validated versioned stereo waveform, returning `None` on a miss.
    ///
    /// # Errors
    ///
    /// Returns an error for cache I/O or an invalid versioned artifact.
    pub fn read_stereo_waveform(
        &self,
        entry: &MediaCatalogEntry,
        frames_per_point: usize,
    ) -> Result<Option<Vec<prism_audio::StereoWaveformPoint>>, MediaArtifactError> {
        let key = entry.artifact_key(
            MediaArtifactKind::Waveform,
            format!("v1:stereo:{frames_per_point}").as_bytes(),
        );
        let Some(contents) = self.read_artifact(key, MediaArtifactKind::Waveform)? else {
            return Ok(None);
        };
        decode_stereo_waveform_artifact(&contents).map(Some)
    }

    /// Returns the next completed artifact write without blocking.
    ///
    /// # Errors
    ///
    /// Returns the channel receive error when no result is ready or the worker
    /// has disconnected.
    pub fn try_result(&self) -> Result<CacheWriteResult, std::sync::mpsc::TryRecvError> {
        self.worker.try_result()
    }

    /// Waits for one completed artifact write for at most `timeout`.
    ///
    /// # Errors
    ///
    /// Returns a timeout or disconnected-channel error.
    pub fn result_timeout(
        &self,
        timeout: Duration,
    ) -> Result<CacheWriteResult, std::sync::mpsc::RecvTimeoutError> {
        self.worker.result_timeout(timeout)
    }

    /// Number of writes waiting for completion.
    #[must_use]
    pub fn pending_writes(&self) -> usize {
        self.worker.pending_writes()
    }

    /// Reads a completed derived artifact, returning `None` on a cache miss.
    ///
    /// # Errors
    ///
    /// Returns a cache filesystem or extension validation error.
    pub fn read_artifact(
        &self,
        key: prism_cache::CacheKey,
        artifact: MediaArtifactKind,
    ) -> Result<Option<Vec<u8>>, prism_cache::CacheError> {
        self.layout.read(key, artifact.extension())
    }

    /// Returns the finalized path for a completed artifact, if present.
    ///
    /// # Errors
    ///
    /// Returns a cache extension validation error.
    pub fn artifact_path(
        &self,
        key: prism_cache::CacheKey,
        artifact: MediaArtifactKind,
    ) -> Result<Option<PathBuf>, prism_cache::CacheError> {
        self.layout.lookup(key, artifact.extension())
    }
}

fn encode_thumbnail_artifact(thumbnail: &DecodedVideoFrame) -> Vec<u8> {
    let mut contents = Vec::with_capacity(13usize.saturating_add(thumbnail.rgba.len()));
    contents.extend_from_slice(b"PRTH");
    contents.push(1);
    contents.extend_from_slice(&thumbnail.width.to_be_bytes());
    contents.extend_from_slice(&thumbnail.height.to_be_bytes());
    contents.extend_from_slice(&thumbnail.rgba);
    contents
}

/// Decodes a self-describing cached thumbnail after strict structural validation.
///
/// # Errors
///
/// Returns an error when the header, dimensions, or RGBA payload is invalid.
pub fn decode_thumbnail_artifact(contents: &[u8]) -> Result<DecodedVideoFrame, MediaArtifactError> {
    if contents.len() < 13 || &contents[..4] != b"PRTH" || contents[4] != 1 {
        return Err(MediaArtifactError::InvalidThumbnail(
            "unsupported header or version",
        ));
    }
    let width = u32::from_be_bytes(
        contents[5..9]
            .try_into()
            .map_err(|_| MediaArtifactError::InvalidThumbnail("missing width"))?,
    );
    let height = u32::from_be_bytes(
        contents[9..13]
            .try_into()
            .map_err(|_| MediaArtifactError::InvalidThumbnail("missing height"))?,
    );
    let rgba = contents[13..].to_vec();
    let expected = checked_rgba_length(width, height)?;
    if rgba.len() != expected {
        return Err(MediaArtifactError::InvalidThumbnail(
            "RGBA payload length does not match dimensions",
        ));
    }
    Ok(DecodedVideoFrame {
        width,
        height,
        rgba,
    })
}

/// Decodes versioned mono waveform points after structural and numeric validation.
///
/// # Errors
///
/// Returns an error when the header, point count, payload, or amplitudes are invalid.
pub fn decode_waveform_artifact(
    contents: &[u8],
) -> Result<Vec<prism_audio::WaveformPoint>, MediaArtifactError> {
    let point_count = waveform_header(contents, 1, 12)?;
    let mut points = Vec::with_capacity(point_count);
    for bytes in contents[10..].chunks_exact(12) {
        let min = f32::from_le_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| MediaArtifactError::InvalidWaveform("missing minimum"))?,
        );
        let max = f32::from_le_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| MediaArtifactError::InvalidWaveform("missing maximum"))?,
        );
        let rms = f32::from_le_bytes(
            bytes[8..12]
                .try_into()
                .map_err(|_| MediaArtifactError::InvalidWaveform("missing RMS"))?,
        );
        let point = prism_audio::WaveformPoint { min, max, rms };
        validate_waveform_point(point)?;
        points.push(point);
    }
    Ok(points)
}

/// Decodes versioned stereo waveform points after structural and numeric validation.
///
/// # Errors
///
/// Returns an error when the header, point count, payload, or amplitudes are invalid.
pub fn decode_stereo_waveform_artifact(
    contents: &[u8],
) -> Result<Vec<prism_audio::StereoWaveformPoint>, MediaArtifactError> {
    let point_count = waveform_header(contents, 2, 24)?;
    let mut points = Vec::with_capacity(point_count);
    for bytes in contents[10..].chunks_exact(24) {
        let mut values = [0.0_f32; 6];
        for (value, encoded) in values.iter_mut().zip(bytes.chunks_exact(4)) {
            let mut raw = [0_u8; 4];
            raw.copy_from_slice(encoded);
            *value = f32::from_le_bytes(raw);
        }
        let left = prism_audio::WaveformPoint {
            min: values[0],
            max: values[1],
            rms: values[2],
        };
        let right = prism_audio::WaveformPoint {
            min: values[3],
            max: values[4],
            rms: values[5],
        };
        validate_waveform_point(left)?;
        validate_waveform_point(right)?;
        points.push(prism_audio::StereoWaveformPoint { left, right });
    }
    Ok(points)
}

fn waveform_header(
    contents: &[u8],
    channels: u8,
    bytes_per_point: usize,
) -> Result<usize, MediaArtifactError> {
    if contents.len() < 10
        || &contents[..4] != b"PRWF"
        || contents[4] != 1
        || contents[5] != channels
    {
        return Err(MediaArtifactError::InvalidWaveform(
            "unsupported header, version, or channel count",
        ));
    }
    let point_count = u32::from_be_bytes(
        contents[6..10]
            .try_into()
            .map_err(|_| MediaArtifactError::InvalidWaveform("missing point count"))?,
    ) as usize;
    let expected = point_count
        .checked_mul(bytes_per_point)
        .and_then(|bytes| bytes.checked_add(10))
        .ok_or(MediaArtifactError::InvalidWaveform("point count overflow"))?;
    if contents.len() != expected {
        return Err(MediaArtifactError::InvalidWaveform(
            "payload length does not match point count",
        ));
    }
    Ok(point_count)
}

fn validate_waveform_point(point: prism_audio::WaveformPoint) -> Result<(), MediaArtifactError> {
    if !point.min.is_finite()
        || !point.max.is_finite()
        || !point.rms.is_finite()
        || point.min > point.max
        || point.rms < 0.0
    {
        return Err(MediaArtifactError::InvalidWaveform(
            "waveform point contains invalid amplitudes",
        ));
    }
    Ok(())
}

impl MediaArtifactKind {
    /// Cache operation identifier.
    #[must_use]
    pub const fn operation(self) -> &'static str {
        match self {
            Self::Thumbnail => "thumbnail",
            Self::Waveform => "waveform",
            Self::PreviewFrame => "preview-frame",
        }
    }

    /// Safe cache extension for the encoded artifact.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Thumbnail | Self::PreviewFrame => "rgba",
            Self::Waveform => "peaks",
        }
    }
}

impl MediaCatalogEntry {
    /// Returns a content-addressed key for a derived artifact.
    ///
    /// Offline entries still receive a deterministic key from their path, so
    /// queued work can be reconciled after relinking without using a path as a
    /// cache filename. Online entries use the full content fingerprint.
    #[must_use]
    pub fn artifact_key(
        &self,
        artifact: MediaArtifactKind,
        parameters: &[u8],
    ) -> prism_cache::CacheKey {
        if let Some(fingerprint) = &self.fingerprint {
            return fingerprint.cache_key("media", artifact.operation(), parameters);
        }
        let mut hasher = Sha256::new();
        hasher.update(self.path.to_string_lossy().as_bytes());
        hasher.update([0]);
        if let Ok(metadata) = fs::metadata(&self.path) {
            hasher.update(metadata.len().to_be_bytes());
            if let Ok(modified) = metadata.modified()
                && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                hasher.update(duration.as_nanos().to_be_bytes());
            }
        }
        prism_cache::CacheKey::from_parts(
            "media",
            &hasher.finalize(),
            artifact.operation(),
            parameters,
        )
    }
}

/// Deterministic project media catalog built from real paths.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MediaCatalog {
    entries: Vec<MediaCatalogEntry>,
}

impl MediaCatalog {
    /// Builds a catalog from already-probed entries.
    #[must_use]
    pub fn from_entries(mut entries: Vec<MediaCatalogEntry>) -> Self {
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        Self { entries }
    }

    /// Probes each path without aborting the complete import batch on one bad item.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] only for a catastrophic catalog operation; per-file
    /// probe failures are retained as entry diagnostics.
    pub fn probe(
        paths: impl IntoIterator<Item = PathBuf>,
        inspector: &FfprobeInspector,
    ) -> Result<Self, MediaError> {
        let mut entries = Vec::new();
        for path in paths {
            if !path.is_file() {
                entries.push(MediaCatalogEntry {
                    path,
                    kind: MediaItemKind::Unknown,
                    metadata: None,
                    fingerprint: None,
                    offline: true,
                    diagnostic: Some("media file is unavailable".to_owned()),
                });
                continue;
            }
            let fingerprint = fingerprint(&path).ok();
            match inspector.inspect(&path) {
                Ok(metadata) => {
                    let kind = classify_metadata(&metadata);
                    entries.push(MediaCatalogEntry {
                        path,
                        kind,
                        metadata: Some(metadata),
                        fingerprint,
                        offline: false,
                        diagnostic: None,
                    });
                }
                Err(error) => entries.push(MediaCatalogEntry {
                    path,
                    kind: MediaItemKind::Unknown,
                    metadata: None,
                    fingerprint,
                    offline: false,
                    diagnostic: Some(error.to_string()),
                }),
            }
        }
        Ok(Self::from_entries(entries))
    }

    /// Returns all catalog entries in deterministic path order.
    #[must_use]
    pub fn entries(&self) -> &[MediaCatalogEntry] {
        &self.entries
    }

    /// Filters entries by case-insensitive path/name query and optional kind.
    #[must_use]
    pub fn search(&self, query: &str, kind: Option<MediaItemKind>) -> Vec<&MediaCatalogEntry> {
        let query = query.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|entry| kind.is_none_or(|expected| entry.kind == expected))
            .filter(|entry| {
                entry
                    .path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains(&query)
            })
            .collect()
    }
}

fn classify_metadata(metadata: &MediaMetadata) -> MediaItemKind {
    if metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
    {
        MediaItemKind::Video
    } else if metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Audio(_)))
    {
        MediaItemKind::Audio
    } else {
        MediaItemKind::Unknown
    }
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

impl MediaFingerprint {
    /// Returns whether two fingerprints describe identical source bytes.
    #[must_use]
    pub fn same_content(&self, other: &Self) -> bool {
        self.byte_length == other.byte_length && self.sha256 == other.sha256
    }
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

/// Source variant selected for preview or proxy playback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaSourceSelection {
    /// Use the original source media.
    Original,
    /// Use a relinked proxy source.
    Proxy,
}

/// Current validity of a relinked proxy against its source media.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyStatus {
    /// No proxy path is configured or the configured file is missing.
    Missing,
    /// The proxy exists and has the same fingerprint as the source.
    Available,
    /// The proxy exists but no longer matches the source fingerprint.
    Stale,
}

/// Selects a source path without silently falling back from proxy mode.
///
/// # Errors
///
/// Returns [`MediaError::ProxyUnavailable`] when proxy mode is requested but
/// no proxy path has been provided.
pub fn select_media_source<'a>(
    original: &'a Path,
    proxy: Option<&'a Path>,
    selection: MediaSourceSelection,
) -> Result<&'a Path, MediaError> {
    match selection {
        MediaSourceSelection::Original => Ok(original),
        MediaSourceSelection::Proxy => proxy.ok_or(MediaError::ProxyUnavailable),
    }
}

/// Compares a configured proxy with its source fingerprint.
///
/// # Errors
///
/// Returns [`MediaError::InvalidProxySource`] when the source is not a regular
/// file, or an I/O error while fingerprinting either regular file.
pub fn proxy_status(
    source: &Path,
    proxy: Option<&Path>,
    recorded_source: Option<&MediaFingerprint>,
) -> Result<ProxyStatus, MediaError> {
    if !source.is_file() {
        return Err(MediaError::InvalidProxySource(source.to_path_buf()));
    }
    let Some(proxy) = proxy else {
        return Ok(ProxyStatus::Missing);
    };
    if !proxy.is_file() {
        return Ok(ProxyStatus::Missing);
    }
    let Some(recorded_source) = recorded_source else {
        return Ok(ProxyStatus::Stale);
    };
    Ok(if fingerprint(source)?.same_content(recorded_source) {
        ProxyStatus::Available
    } else {
        ProxyStatus::Stale
    })
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

    /// Returns one RGBA pixel when `(x, y)` is inside the frame.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidFrameDimensions`] or
    /// [`MediaError::InvalidFrameLength`] when the frame storage is invalid.
    pub fn pixel(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, MediaError> {
        let expected = checked_rgba_length(self.width, self.height)?;
        if self.rgba.len() != expected {
            return Err(MediaError::InvalidFrameLength {
                expected,
                actual: self.rgba.len(),
            });
        }
        if x >= self.width || y >= self.height {
            return Ok(None);
        }
        let index = usize::try_from((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4)
            .map_err(|_| MediaError::InvalidFrameDimensions {
                width: self.width,
                height: self.height,
            })?;
        Ok(Some(
            self.rgba[index..index + 4].try_into().unwrap_or([0; 4]),
        ))
    }

    /// Returns one packed RGBA row, or `None` when `y` is out of range.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame dimensions or packed RGBA buffer length
    /// are inconsistent.
    pub fn row_rgba(&self, y: u32) -> Result<Option<&[u8]>, MediaError> {
        let expected = checked_rgba_length(self.width, self.height)?;
        if self.rgba.len() != expected {
            return Err(MediaError::InvalidFrameLength {
                expected,
                actual: self.rgba.len(),
            });
        }
        if y >= self.height {
            return Ok(None);
        }
        let row_length = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or(MediaError::InvalidFrameDimensions {
                width: self.width,
                height: self.height,
            })?;
        let start = usize::try_from(y)
            .ok()
            .and_then(|row| row.checked_mul(row_length))
            .ok_or(MediaError::InvalidFrameDimensions {
                width: self.width,
                height: self.height,
            })?;
        Ok(Some(&self.rgba[start..start + row_length]))
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
    /// Returns the exact byte length captured by the fingerprint.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Returns the lowercase hexadecimal full-file digest.
    #[must_use]
    pub fn sha256_hex(&self) -> &str {
        &self.sha256
    }

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

    /// Decodes a bounded mono `f32` sample window from the start of an audio stream.
    ///
    /// `audio_ordinal` is zero-based among audio streams. `FFmpeg` resamples to the
    /// requested rate and is given an exact duration ceiling derived from
    /// `max_samples`; returned bytes are independently bounded and validated.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for invalid bounds, process failure, excessive
    /// output, or a partial `f32` sample.
    pub fn decode_audio_mono_f32(
        &self,
        path: &Path,
        audio_ordinal: u32,
        sample_rate: u32,
        max_samples: usize,
    ) -> Result<Vec<f32>, MediaError> {
        let max_samples_u32 = u32::try_from(max_samples).ok();
        let maximum = max_samples.checked_mul(4);
        if sample_rate == 0
            || sample_rate > 384_000
            || max_samples_u32.is_none()
            || maximum.is_none_or(|bytes| bytes == 0 || bytes > MAX_REFERENCE_AUDIO_BYTES)
        {
            return Err(MediaError::InvalidAudioDecodeBounds {
                sample_rate,
                max_samples,
            });
        }
        let maximum = maximum.ok_or(MediaError::InvalidAudioDecodeBounds {
            sample_rate,
            max_samples,
        })?;
        let duration = format!(
            "{:.9}",
            f64::from(max_samples_u32.unwrap_or_default()) / f64::from(sample_rate)
        );
        let selection = format!("0:a:{audio_ordinal}");
        let sample_rate = sample_rate.to_string();
        let output = Command::new(&self.executable)
            .args(["-v", "error", "-i"])
            .arg(path)
            .args([
                "-map",
                &selection,
                "-t",
                &duration,
                "-ac",
                "1",
                "-ar",
                &sample_rate,
                "-f",
                "f32le",
                "pipe:1",
            ])
            .output()?;
        if !output.status.success() {
            return Err(MediaError::Decoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if output.stdout.len() > maximum || !output.stdout.len().is_multiple_of(4) {
            return Err(MediaError::InvalidAudioFrameLength {
                maximum,
                actual: output.stdout.len(),
            });
        }
        Ok(output
            .stdout
            .chunks_exact(4)
            .map(|bytes| {
                let mut sample = [0_u8; 4];
                sample.copy_from_slice(bytes);
                f32::from_le_bytes(sample)
            })
            .collect())
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
        for (sample_rate, max_samples) in [(0, 1), (48_000, 0), (384_001, 1), (48_000, usize::MAX)]
        {
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
        let directory =
            std::env::temp_dir().join(format!("prism-proxy-status-{}", std::process::id()));
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
