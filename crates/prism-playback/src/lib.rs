//! UI-independent timeline evaluation for preview and export.
//!
//! This crate resolves authoritative project edits into source-frame requests.
//! Decoding and compositing consume this plan but do not redefine edit timing.

use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
use prism_media::{DecodedVideoFrame, FfmpegReferenceDecoder};
use prism_project::{Project, TrackKind};
use prism_timeline::{RationalTime, TimeError};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use thiserror::Error;

/// A visible source selected for one sequence timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoSourceRequest {
    /// Track containing the selected clip.
    pub track_id: TrackId,
    /// Selected clip.
    pub clip_id: ClipId,
    /// Referenced media asset.
    pub media_id: MediaAssetId,
    /// Exact timestamp to request from the source decoder.
    pub source_time: RationalTime,
}

/// Evaluation result in the sequence's stored video-track order.
///
/// Layer ordering and blend semantics will be finalized by the compositor. The
/// planner deliberately preserves authoritative track order without reversing
/// it or assigning an undocumented top/bottom interpretation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VideoFramePlan {
    /// Requested sequence timestamp.
    pub sequence_time: RationalTime,
    /// At most one visible source per video track, in stored track order.
    pub sources: Vec<VideoSourceRequest>,
}

/// A source selected from an unmuted audio track at one sequence timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioSourceRequest {
    /// Track containing the selected clip.
    pub track_id: TrackId,
    /// Selected clip.
    pub clip_id: ClipId,
    /// Referenced media asset.
    pub media_id: MediaAssetId,
    /// Exact timestamp to request from the source decoder.
    pub source_time: RationalTime,
}

/// Audio sources active at one exact sequence timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioFramePlan {
    /// Requested sequence timestamp.
    pub sequence_time: RationalTime,
    /// At most one source per unmuted audio track, in stored track order.
    pub sources: Vec<AudioSourceRequest>,
}

/// One scheduled decode operation derived from timeline evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeRequest {
    /// Caller-assigned identifier used to match asynchronous results.
    pub request_id: u64,
    /// Playback generation. Results older than the current generation are stale.
    pub generation: u64,
    /// Exact source selection produced by timeline evaluation.
    pub source: VideoSourceRequest,
}

/// Result delivered by an asynchronous decode worker.
#[derive(Debug, Eq, PartialEq)]
pub enum DecodeResult<Frame> {
    /// A current request completed successfully.
    Ready {
        /// Original request metadata.
        request: DecodeRequest,
        /// Decoder-specific frame value.
        frame: Frame,
    },
    /// A current request failed in the decoder adapter.
    Failed {
        /// Original request metadata.
        request: DecodeRequest,
        /// Stable diagnostic text supplied by the adapter.
        message: String,
    },
}

/// Non-blocking submission failures for a bounded decode queue.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SubmitError {
    /// The configured queue is full; the caller must drop or retry work.
    #[error("decode queue is full")]
    QueueFull,
    /// The worker thread has stopped accepting work.
    #[error("decode worker is stopped")]
    WorkerStopped,
}

/// A bounded, codec-independent asynchronous frame scheduler.
///
/// The supplied decoder closure owns codec integration and may block without
/// blocking the caller. Raising the generation suppresses queued and in-flight
/// results from older playback positions. It does not forcibly interrupt an
/// in-flight native decoder call; process-level cancellation belongs to the
/// decoder adapter. Completed frames are also bounded; if the consumer stops
/// draining results, newer completions may be dropped rather than blocking the
/// worker or its shutdown path.
pub struct AsyncDecodeWorker<Frame> {
    requests: Option<SyncSender<DecodeRequest>>,
    results: Receiver<DecodeResult<Frame>>,
    minimum_generation: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

/// Scheduling failures for the playback coordinator.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum PlaybackError {
    /// Timeline evaluation failed before any decoder work was submitted.
    #[error("could not evaluate playback frame: {0}")]
    Evaluation(#[from] EvaluationError),
    /// A decoder queue rejected a scheduled source.
    #[error("could not schedule source decode: {0}")]
    Submit(#[from] SubmitError),
    /// Request identifiers could not advance without wrapping.
    #[error("playback request identifier overflowed")]
    RequestIdOverflow,
}

/// One exact playback scheduling result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaybackSchedule {
    /// Generation assigned to all source requests.
    pub generation: u64,
    /// Exact sequence time evaluated by the timeline.
    pub sequence_time: RationalTime,
    /// Decoder request IDs submitted for active video sources.
    pub request_ids: Vec<u64>,
}

/// Playback coordinator joining exact timeline evaluation to bounded decoding.
pub struct PlaybackEngine<Frame> {
    worker: AsyncDecodeWorker<Frame>,
    generation: u64,
    next_request_id: u64,
}

/// Decoder source description used by the reference `FFmpeg` playback adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceVideoSource {
    /// Source media path.
    pub path: PathBuf,
    /// Zero-based video stream ordinal.
    pub video_ordinal: u32,
    /// Inspected frame width.
    pub width: u32,
    /// Inspected frame height.
    pub height: u32,
}

/// Errors specific to the reference playback adapter.
#[derive(Debug, Error)]
pub enum ReferencePlaybackError {
    /// Timeline scheduling or queue backpressure failed.
    #[error("playback scheduling failed: {0}")]
    Playback(#[from] PlaybackError),
    /// A timeline source did not have an inspected decoder description.
    #[error("no decoder source is registered for media asset {0:?}")]
    MissingSource(MediaAssetId),
}

/// FFmpeg-backed playback adapter that executes decoding only on the worker.
pub struct ReferenceVideoPlayback {
    engine: PlaybackEngine<DecodedVideoFrame>,
}

impl ReferenceVideoPlayback {
    /// Creates a reference playback adapter from inspected source descriptions.
    #[must_use]
    pub fn new(
        executable: impl Into<PathBuf>,
        queue_capacity: usize,
        sources: BTreeMap<MediaAssetId, ReferenceVideoSource>,
    ) -> Self {
        let decoder = FfmpegReferenceDecoder::new(executable);
        let engine = PlaybackEngine::new(queue_capacity, move |request| {
            let source = sources.get(&request.source.media_id).ok_or_else(|| {
                format!("missing media source {}", request.source.media_id.as_uuid())
            })?;
            decoder
                .decode_video_frame_rgba(
                    &source.path,
                    source.video_ordinal,
                    request.source.source_time,
                    source.width,
                    source.height,
                )
                .map_err(|error| error.to_string())
        });
        Self { engine }
    }

    /// Invalidates queued frames and advances the seek generation.
    pub fn seek(&mut self) -> u64 {
        self.engine.seek()
    }

    /// Schedules one exact sequence timestamp for reference decoding.
    ///
    /// # Errors
    ///
    /// Returns [`ReferencePlaybackError`] when timeline evaluation fails or
    /// the bounded decode queue rejects a source.
    pub fn schedule_video(
        &mut self,
        project: &Project,
        sequence_id: SequenceId,
        sequence_time: RationalTime,
    ) -> Result<PlaybackSchedule, ReferencePlaybackError> {
        self.engine
            .schedule_video(project, sequence_id, sequence_time)
            .map_err(ReferencePlaybackError::from)
    }

    /// Waits for one decoded frame or decoder diagnostic.
    ///
    /// # Errors
    ///
    /// Returns a timeout or worker disconnection error when no frame arrives.
    pub fn result_timeout(
        &self,
        timeout: Duration,
    ) -> Result<DecodeResult<DecodedVideoFrame>, RecvTimeoutError> {
        self.engine.result_timeout(timeout)
    }
}

impl<Frame: Send + 'static> PlaybackEngine<Frame> {
    /// Creates a coordinator with a bounded decoder queue.
    #[must_use]
    pub fn new(
        queue_capacity: usize,
        decode: impl FnMut(&DecodeRequest) -> Result<Frame, String> + Send + 'static,
    ) -> Self {
        Self {
            worker: AsyncDecodeWorker::spawn(queue_capacity, decode),
            generation: 0,
            next_request_id: 0,
        }
    }

    /// Advances the seek/playback generation and invalidates older results.
    pub fn seek(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.worker.cancel_before(self.generation);
        self.generation
    }

    /// Schedules all active visible video sources for one exact sequence time.
    ///
    /// # Errors
    ///
    /// Returns [`PlaybackError`] when timeline evaluation or queue submission
    /// fails. Requests already submitted in this call remain tagged with the
    /// same generation and can be safely discarded by a later seek.
    pub fn schedule_video(
        &mut self,
        project: &Project,
        sequence_id: SequenceId,
        sequence_time: RationalTime,
    ) -> Result<PlaybackSchedule, PlaybackError> {
        let plan = evaluate_video_frame(project, sequence_id, sequence_time)?;
        let mut request_ids = Vec::with_capacity(plan.sources.len());
        for source in plan.sources {
            let request_id = self.next_request_id;
            self.next_request_id = self
                .next_request_id
                .checked_add(1)
                .ok_or(PlaybackError::RequestIdOverflow)?;
            self.worker.try_submit(DecodeRequest {
                request_id,
                generation: self.generation,
                source,
            })?;
            request_ids.push(request_id);
        }
        Ok(PlaybackSchedule {
            generation: self.generation,
            sequence_time: plan.sequence_time,
            request_ids,
        })
    }

    /// Polls one completed decode result without blocking the caller.
    ///
    /// # Errors
    ///
    /// Returns the worker receiver error when the queue is empty or stopped.
    pub fn try_result(&self) -> Result<DecodeResult<Frame>, TryRecvError> {
        self.worker.try_result()
    }

    /// Waits for one decoder result on a coordinator thread.
    ///
    /// # Errors
    ///
    /// Returns timeout or disconnection when no result arrives before the
    /// deadline or the decoder worker stops.
    pub fn result_timeout(
        &self,
        timeout: Duration,
    ) -> Result<DecodeResult<Frame>, RecvTimeoutError> {
        self.worker.result_timeout(timeout)
    }
}

impl<Frame: Send + 'static> AsyncDecodeWorker<Frame> {
    /// Starts one decoder thread with an explicitly bounded pending queue.
    ///
    /// The decoder closure receives exact source requests and returns either a
    /// frame value or stable diagnostic text.
    #[must_use]
    pub fn spawn(
        queue_capacity: usize,
        mut decode: impl FnMut(&DecodeRequest) -> Result<Frame, String> + Send + 'static,
    ) -> Self {
        let queue_capacity = queue_capacity.max(1);
        let (request_tx, request_rx) = sync_channel::<DecodeRequest>(queue_capacity);
        let (result_tx, result_rx) = sync_channel(queue_capacity.saturating_add(1));
        let minimum_generation = Arc::new(AtomicU64::new(0));
        let worker_generation = Arc::clone(&minimum_generation);
        let worker_thread = thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                if request.generation < worker_generation.load(Ordering::Acquire) {
                    continue;
                }
                let decoded = decode(&request);
                if request.generation < worker_generation.load(Ordering::Acquire) {
                    continue;
                }
                let result = match decoded {
                    Ok(frame) => DecodeResult::Ready { request, frame },
                    Err(message) => DecodeResult::Failed { request, message },
                };
                match result_tx.try_send(result) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
        });
        Self {
            requests: Some(request_tx),
            results: result_rx,
            minimum_generation,
            thread: Some(worker_thread),
        }
    }

    /// Attempts to enqueue work without blocking the caller.
    ///
    /// # Errors
    ///
    /// Returns [`SubmitError::QueueFull`] when backpressure is required or
    /// [`SubmitError::WorkerStopped`] after worker shutdown.
    pub fn try_submit(&self, request: DecodeRequest) -> Result<(), SubmitError> {
        self.requests
            .as_ref()
            .ok_or(SubmitError::WorkerStopped)?
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => SubmitError::QueueFull,
                TrySendError::Disconnected(_) => SubmitError::WorkerStopped,
            })
    }

    /// Suppresses results from all generations lower than `generation`.
    pub fn cancel_before(&self, generation: u64) {
        self.minimum_generation
            .fetch_max(generation, Ordering::AcqRel);
    }

    /// Polls for one completed current-generation result without blocking.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Empty`] when nothing is ready or
    /// [`TryRecvError::Disconnected`] after the worker exits and drains.
    pub fn try_result(&self) -> Result<DecodeResult<Frame>, TryRecvError> {
        self.results.try_recv()
    }

    /// Waits up to `timeout` for one completed current-generation result.
    ///
    /// This is intended for coordinator/background threads, not UI event loops.
    ///
    /// # Errors
    ///
    /// Returns [`RecvTimeoutError::Timeout`] when the deadline expires or
    /// [`RecvTimeoutError::Disconnected`] after the worker exits and drains.
    pub fn result_timeout(
        &self,
        timeout: Duration,
    ) -> Result<DecodeResult<Frame>, RecvTimeoutError> {
        self.results.recv_timeout(timeout)
    }
}

impl<Frame> AsyncDecodeWorker<Frame> {
    fn stop(&mut self) {
        self.requests.take();
        if let Some(worker_thread) = self.thread.take() {
            let _ = worker_thread.join();
        }
    }
}

impl<Frame> Drop for AsyncDecodeWorker<Frame> {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Timeline evaluation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum EvaluationError {
    /// The requested sequence does not exist in the project.
    #[error("sequence does not exist")]
    SequenceNotFound,
    /// Sequence evaluation does not accept timestamps before zero.
    #[error("sequence timestamp must not be negative")]
    NegativeTimestamp,
    /// Exact range or source-time arithmetic failed.
    #[error("timeline arithmetic failed: {0}")]
    Time(#[from] TimeError),
}

/// Resolves active video clips at an exact sequence timestamp.
///
/// Clip ranges are start-inclusive and end-exclusive. Empty gaps produce no
/// request for that track. Audio tracks are ignored by this video planner.
///
/// # Errors
///
/// Returns [`EvaluationError`] for a missing sequence, negative timestamp, or
/// unrepresentable exact-time arithmetic.
pub fn evaluate_video_frame(
    project: &Project,
    sequence_id: SequenceId,
    sequence_time: RationalTime,
) -> Result<VideoFramePlan, EvaluationError> {
    if !sequence_time.is_non_negative() {
        return Err(EvaluationError::NegativeTimestamp);
    }
    let sequence = project
        .sequences
        .get(&sequence_id)
        .ok_or(EvaluationError::SequenceNotFound)?;
    let mut sources = Vec::new();
    for track in &sequence.tracks {
        if track.kind != TrackKind::Video || !track.visible {
            continue;
        }
        for clip in &track.clips {
            if sequence_time < clip.timeline_start {
                break;
            }
            let timeline_end = clip.timeline_start.checked_add(clip.source_duration)?;
            if sequence_time >= timeline_end {
                continue;
            }
            let offset = sequence_time.checked_sub(clip.timeline_start)?;
            sources.push(VideoSourceRequest {
                track_id: track.id,
                clip_id: clip.id,
                media_id: clip.media_id,
                source_time: clip.source_start.checked_add(offset)?,
            });
            break;
        }
    }
    Ok(VideoFramePlan {
        sequence_time,
        sources,
    })
}

/// Resolves active clips on unmuted audio tracks at an exact sequence time.
///
/// # Errors
///
/// Returns [`EvaluationError`] for a missing sequence, negative timestamp, or
/// unrepresentable exact-time arithmetic.
pub fn evaluate_audio_frame(
    project: &Project,
    sequence_id: SequenceId,
    sequence_time: RationalTime,
) -> Result<AudioFramePlan, EvaluationError> {
    if !sequence_time.is_non_negative() {
        return Err(EvaluationError::NegativeTimestamp);
    }
    let sequence = project
        .sequences
        .get(&sequence_id)
        .ok_or(EvaluationError::SequenceNotFound)?;
    let mut sources = Vec::new();
    for track in &sequence.tracks {
        if track.kind != TrackKind::Audio || track.muted {
            continue;
        }
        for clip in &track.clips {
            if sequence_time < clip.timeline_start {
                break;
            }
            let timeline_end = clip.timeline_start.checked_add(clip.source_duration)?;
            if sequence_time >= timeline_end {
                continue;
            }
            let offset = sequence_time.checked_sub(clip.timeline_start)?;
            sources.push(AudioSourceRequest {
                track_id: track.id,
                clip_id: clip.id,
                media_id: clip.media_id,
                source_time: clip.source_start.checked_add(offset)?,
            });
            break;
        }
    }
    Ok(AudioFramePlan {
        sequence_time,
        sources,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AsyncDecodeWorker, DecodeRequest, DecodeResult, EvaluationError, PlaybackEngine,
        SubmitError, VideoSourceRequest, evaluate_audio_frame, evaluate_video_frame,
    };
    use prism_core::{ClipId, MediaAssetId, SequenceId, TrackId};
    use prism_project::{Clip, MediaAsset, Project, Sequence, Track, TrackKind};
    use prism_timeline::RationalTime;
    use std::{sync::mpsc, time::Duration};

    struct Fixture {
        project: Project,
        sequence_id: SequenceId,
        video_clip_id: ClipId,
        second_video_clip_id: ClipId,
        video_media_id: MediaAssetId,
    }

    fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
        let mut project = Project::new("Evaluation");
        let sequence_id = SequenceId::new();
        let video_media_id = MediaAssetId::new();
        let audio_media_id = MediaAssetId::new();
        project.media.insert(
            video_media_id,
            MediaAsset {
                id: video_media_id,
                source_path: "video.mp4".to_owned(),
                proxy_path: None,
            },
        );
        project.media.insert(
            audio_media_id,
            MediaAsset {
                id: audio_media_id,
                source_path: "audio.wav".to_owned(),
                proxy_path: None,
            },
        );
        let video_clip_id = ClipId::new();
        let second_video_clip_id = ClipId::new();
        project.sequences.insert(
            sequence_id,
            Sequence {
                id: sequence_id,
                name: "Main".to_owned(),
                markers: Vec::new(),
                transitions: Vec::new(),
                tracks: vec![
                    Track {
                        id: TrackId::new(),
                        kind: TrackKind::Video,
                        clips: vec![
                            Clip {
                                id: video_clip_id,
                                media_id: video_media_id,
                                timeline_start: RationalTime::new(1, 1)?,
                                source_start: RationalTime::new(10, 1)?,
                                source_duration: RationalTime::new(2, 1)?,
                            },
                            Clip {
                                id: second_video_clip_id,
                                media_id: video_media_id,
                                timeline_start: RationalTime::new(3, 1)?,
                                source_start: RationalTime::new(20, 1)?,
                                source_duration: RationalTime::new(1, 1)?,
                            },
                        ],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                    Track {
                        id: TrackId::new(),
                        kind: TrackKind::Audio,
                        clips: vec![Clip {
                            id: ClipId::new(),
                            media_id: audio_media_id,
                            timeline_start: RationalTime::ZERO,
                            source_start: RationalTime::ZERO,
                            source_duration: RationalTime::new(30, 1)?,
                        }],
                        locked: false,
                        visible: true,
                        muted: false,
                    },
                ],
            },
        );
        project.validate()?;
        Ok(Fixture {
            project,
            sequence_id,
            video_clip_id,
            second_video_clip_id,
            video_media_id,
        })
    }

    fn decode_request(request_id: u64, generation: u64) -> DecodeRequest {
        DecodeRequest {
            request_id,
            generation,
            source: VideoSourceRequest {
                track_id: TrackId::new(),
                clip_id: ClipId::new(),
                media_id: MediaAssetId::new(),
                source_time: RationalTime::ZERO,
            },
        }
    }

    #[test]
    fn maps_sequence_time_to_exact_source_time() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture()?;
        let plan = evaluate_video_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(3, 2)?,
        )?;
        assert_eq!(plan.sources.len(), 1);
        assert_eq!(plan.sources[0].clip_id, fixture.video_clip_id);
        assert_eq!(plan.sources[0].media_id, fixture.video_media_id);
        assert_eq!(plan.sources[0].source_time, RationalTime::new(21, 2)?);
        Ok(())
    }

    #[test]
    fn clip_end_is_exclusive_and_next_start_is_inclusive() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture()?;
        let plan = evaluate_video_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(3, 1)?,
        )?;
        assert_eq!(plan.sources.len(), 1);
        assert_eq!(plan.sources[0].clip_id, fixture.second_video_clip_id);
        assert_eq!(plan.sources[0].source_time, RationalTime::new(20, 1)?);
        Ok(())
    }

    #[test]
    fn gaps_and_audio_tracks_do_not_produce_video_sources() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture()?;
        let plan = evaluate_video_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(1, 2)?,
        )?;
        assert!(plan.sources.is_empty());
        Ok(())
    }

    #[test]
    fn hidden_video_tracks_do_not_produce_sources() -> Result<(), Box<dyn std::error::Error>> {
        let mut fixture = fixture()?;
        fixture
            .project
            .sequences
            .get_mut(&fixture.sequence_id)
            .ok_or("missing sequence")?
            .tracks[0]
            .visible = false;
        let plan = evaluate_video_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(3, 2)?,
        )?;
        assert!(plan.sources.is_empty());
        Ok(())
    }

    #[test]
    fn muted_audio_tracks_do_not_produce_sources() -> Result<(), Box<dyn std::error::Error>> {
        let mut fixture = fixture()?;
        let plan = evaluate_audio_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(1, 1)?,
        )?;
        assert_eq!(plan.sources.len(), 1);
        fixture
            .project
            .sequences
            .get_mut(&fixture.sequence_id)
            .ok_or("missing sequence")?
            .tracks[1]
            .muted = true;
        let muted_plan = evaluate_audio_frame(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(1, 1)?,
        )?;
        assert!(muted_plan.sources.is_empty());
        Ok(())
    }

    #[test]
    fn rejects_negative_time_and_missing_sequence() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture()?;
        assert_eq!(
            evaluate_video_frame(
                &fixture.project,
                fixture.sequence_id,
                RationalTime::new(-1, 1)?,
            ),
            Err(EvaluationError::NegativeTimestamp)
        );
        assert_eq!(
            evaluate_video_frame(&fixture.project, SequenceId::new(), RationalTime::ZERO),
            Err(EvaluationError::SequenceNotFound)
        );
        Ok(())
    }

    #[test]
    fn async_worker_delivers_decoder_result() -> Result<(), Box<dyn std::error::Error>> {
        let worker = AsyncDecodeWorker::spawn(1, |request| Ok(request.request_id * 2));
        let request = decode_request(7, 0);
        worker.try_submit(request.clone())?;
        assert_eq!(
            worker.result_timeout(Duration::from_secs(1))?,
            DecodeResult::Ready { request, frame: 14 }
        );
        Ok(())
    }

    #[test]
    fn async_worker_applies_backpressure_and_suppresses_stale_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = AsyncDecodeWorker::spawn(1, move |request| {
            started_tx
                .send(request.request_id)
                .map_err(|error| error.to_string())?;
            release_rx.recv().map_err(|error| error.to_string())?;
            Ok(request.request_id)
        });
        worker.try_submit(decode_request(1, 1))?;
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(1))?, 1);
        worker.try_submit(decode_request(2, 1))?;
        assert_eq!(
            worker.try_submit(decode_request(3, 1)),
            Err(SubmitError::QueueFull)
        );
        worker.cancel_before(2);
        release_tx.send(())?;
        assert!(worker.result_timeout(Duration::from_millis(20)).is_err());
        worker.try_submit(decode_request(4, 2))?;
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(1))?, 4);
        release_tx.send(())?;
        let DecodeResult::Ready { request, frame } =
            worker.result_timeout(Duration::from_secs(1))?
        else {
            return Err("expected ready frame".into());
        };
        assert_eq!(request.request_id, 4);
        assert_eq!(frame, 4);
        Ok(())
    }

    #[test]
    fn playback_engine_schedules_exact_sources_and_invalidates_prior_seeks()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture()?;
        let mut engine = PlaybackEngine::new(4, |request| Ok(request.source.source_time));
        let first = engine.schedule_video(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(3, 2)?,
        )?;
        assert_eq!(first.generation, 0);
        assert_eq!(first.request_ids, vec![0]);
        let generation = engine.seek();
        assert_eq!(generation, 1);
        let second = engine.schedule_video(
            &fixture.project,
            fixture.sequence_id,
            RationalTime::new(3, 1)?,
        )?;
        assert_eq!(second.generation, 1);
        assert_eq!(second.request_ids, vec![1]);
        let result = engine.result_timeout(Duration::from_secs(1))?;
        let DecodeResult::Ready { request, frame } = result else {
            return Err("expected a current-generation frame".into());
        };
        assert_eq!(request.generation, 1);
        assert_eq!(frame, RationalTime::new(20, 1)?);
        Ok(())
    }
}
