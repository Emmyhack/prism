use super::{
    CacheError, CacheLayout, CacheSubmitError, CacheWorker, CacheWriteRequest, CacheWriteResult,
    DecodedVideoFrame, Digest, Duration, Error, FfprobeInspector, MediaError, MediaFingerprint,
    MediaMetadata, PathBuf, RationalTime, Sha256, checked_rgba_length, fingerprint, fs,
};

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
