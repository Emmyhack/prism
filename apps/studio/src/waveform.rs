//! Background waveform cache lookup and generation.

use prism_media::{
    FfmpegReferenceDecoder, MediaArtifactError, MediaArtifactKind, MediaArtifactWorker,
    MediaCatalogEntry, MediaFingerprint, MediaItemKind, fingerprint,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::Duration,
};

/// One bounded waveform request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaveformRequest {
    /// Original media source.
    pub path: PathBuf,
    /// Project-scoped derived-cache root.
    pub cache_root: PathBuf,
    /// Output sample rate used for deterministic decoding.
    pub sample_rate: u32,
    /// Hard ceiling on decoded mono samples.
    pub max_samples: usize,
    /// Decoded samples summarized into each display point.
    pub samples_per_point: usize,
}

/// Completed waveform data and its cache provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct WaveformResult {
    /// Source path associated with the points.
    pub path: PathBuf,
    /// Source fingerprint used by the content-addressed cache key.
    pub fingerprint: MediaFingerprint,
    /// Peak/RMS points ready for presentation.
    pub points: Vec<prism_audio::WaveformPoint>,
    /// Whether decoding and generation were bypassed by a valid cache hit.
    pub cache_hit: bool,
}

/// Bounded off-thread cache-first waveform worker.
pub struct WaveformWorker {
    requests: SyncSender<WaveformRequest>,
    results: Receiver<Result<WaveformResult, String>>,
    pending: Arc<AtomicUsize>,
}

impl WaveformWorker {
    /// Starts a worker using an explicit `FFmpeg` executable.
    #[must_use]
    pub fn spawn(ffmpeg: impl Into<PathBuf>, capacity: usize) -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel(capacity.max(1));
        let (result_tx, result_rx) = mpsc::sync_channel(capacity.max(1));
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = Arc::clone(&pending);
        let decoder = FfmpegReferenceDecoder::new(ffmpeg);
        thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let result = process_request(&decoder, &request);
                worker_pending.fetch_sub(1, Ordering::Release);
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        });
        Self {
            requests: request_tx,
            results: result_rx,
            pending,
        }
    }

    /// Queues one request without blocking.
    ///
    /// # Errors
    ///
    /// Returns the original request when the queue is full or disconnected.
    pub fn try_submit(
        &self,
        request: WaveformRequest,
    ) -> Result<(), TrySendError<WaveformRequest>> {
        self.pending.fetch_add(1, Ordering::Relaxed);
        if let Err(error) = self.requests.try_send(request) {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    /// Polls one completed request without blocking.
    ///
    /// # Errors
    ///
    /// Returns empty or disconnected channel state when no result is available.
    pub fn try_result(&self) -> Result<Result<WaveformResult, String>, TryRecvError> {
        self.results.try_recv()
    }

    /// Number of requests queued or executing.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }
}

fn process_request(
    decoder: &FfmpegReferenceDecoder,
    request: &WaveformRequest,
) -> Result<WaveformResult, String> {
    let fingerprint = fingerprint(&request.path).map_err(|error| error.to_string())?;
    let entry = MediaCatalogEntry {
        path: request.path.clone(),
        kind: MediaItemKind::Audio,
        metadata: None,
        fingerprint: Some(fingerprint.clone()),
        offline: false,
        diagnostic: None,
    };
    let artifacts = MediaArtifactWorker::spawn(&request.cache_root, 1);
    let variant = format!(
        "mono-{}hz-{}samples",
        request.sample_rate, request.max_samples
    );
    match artifacts.read_waveform_variant(&entry, request.samples_per_point, &variant) {
        Ok(Some(points)) => {
            return Ok(WaveformResult {
                path: request.path.clone(),
                fingerprint,
                points,
                cache_hit: true,
            });
        }
        Ok(None) => {}
        Err(MediaArtifactError::InvalidWaveform(_)) => {
            let key = MediaArtifactWorker::waveform_variant_key(
                &entry,
                request.samples_per_point,
                &variant,
            );
            artifacts
                .remove_artifact(key, MediaArtifactKind::Waveform)
                .map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let samples = decoder
        .decode_audio_mono_f32(&request.path, 0, request.sample_rate, request.max_samples)
        .map_err(|error| error.to_string())?;
    let key = artifacts
        .schedule_waveform_variant(&entry, &samples, request.samples_per_point, &variant)
        .map_err(|error| error.to_string())?;
    let write = artifacts
        .result_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    if write.key != key {
        return Err("waveform cache worker returned a mismatched key".to_owned());
    }
    write.result.map_err(|error| error.to_string())?;
    let points = artifacts
        .read_waveform_variant(&entry, request.samples_per_point, &variant)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "waveform cache write completed without an artifact".to_owned())?;
    Ok(WaveformResult {
        path: request.path.clone(),
        fingerprint,
        points,
        cache_hit: false,
    })
}

#[cfg(test)]
mod tests {
    use super::{WaveformRequest, WaveformWorker};
    use prism_cache::CacheLayout;
    use prism_media::{
        MediaArtifactKind, MediaArtifactWorker, MediaCatalogEntry, MediaItemKind, fingerprint,
    };
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };

    #[test]
    fn valid_cache_hit_bypasses_decoder() -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("prism-studio-waveform-hit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        let path = root.join("not-audio.mov");
        std::fs::write(&path, b"identity only; intentionally not media")?;
        let cache_root = root.join("cache");
        let entry = MediaCatalogEntry {
            path: path.clone(),
            kind: MediaItemKind::Audio,
            metadata: None,
            fingerprint: Some(fingerprint(&path)?),
            offline: false,
            diagnostic: None,
        };
        let artifacts = MediaArtifactWorker::spawn(&cache_root, 1);
        artifacts.schedule_waveform_variant(
            &entry,
            &[-1.0, 0.5, 1.0, 0.0],
            2,
            "mono-48000hz-48000samples",
        )?;
        artifacts.result_timeout(Duration::from_secs(1))?.result?;

        let worker = WaveformWorker::spawn(PathBuf::from("unavailable-ffmpeg"), 1);
        worker.try_submit(WaveformRequest {
            path: path.clone(),
            cache_root: cache_root.clone(),
            sample_rate: 48_000,
            max_samples: 48_000,
            samples_per_point: 2,
        })?;
        let started = Instant::now();
        let result = loop {
            match worker.try_result() {
                Ok(result) => break result?,
                Err(std::sync::mpsc::TryRecvError::Empty)
                    if started.elapsed() < Duration::from_secs(1) =>
                {
                    std::thread::yield_now();
                }
                Err(error) => return Err(error.into()),
            }
        };
        assert!(result.cache_hit);
        assert_eq!(result.points.len(), 2);
        assert_eq!(worker.pending(), 0);

        let key = MediaArtifactWorker::waveform_variant_key(&entry, 2, "mono-48000hz-48000samples");
        let layout = CacheLayout::new(&cache_root);
        let artifact = layout
            .lookup(key, MediaArtifactKind::Waveform.extension())?
            .ok_or("waveform artifact missing")?;
        std::fs::write(artifact, b"corrupt waveform")?;
        worker.try_submit(WaveformRequest {
            path: entry.path,
            cache_root: cache_root.clone(),
            sample_rate: 48_000,
            max_samples: 48_000,
            samples_per_point: 2,
        })?;
        let started = Instant::now();
        let repair = loop {
            match worker.try_result() {
                Ok(result) => break result,
                Err(std::sync::mpsc::TryRecvError::Empty)
                    if started.elapsed() < Duration::from_secs(1) =>
                {
                    std::thread::yield_now();
                }
                Err(error) => return Err(error.into()),
            }
        };
        assert!(repair.is_err());
        assert_eq!(
            layout.lookup(key, MediaArtifactKind::Waveform.extension())?,
            None
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
