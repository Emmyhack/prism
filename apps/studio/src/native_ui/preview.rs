use super::*;

/// Native shell startup failures.
#[derive(Debug, Error)]
pub enum NativeUiError {
    /// Native window or event-loop initialization failed.
    #[error("native platform error: {0}")]
    Platform(#[from] slint::PlatformError),
    /// The requested project could not be loaded safely.
    #[error("could not open project: {0}")]
    Persistence(#[from] prism_persistence::PersistenceError),
}

// Decode away from the UI thread. Each monitor holds at most one queued request;
// a newer scrub position supersedes pending work and stale replies are discarded.
#[derive(Debug)]
pub(super) struct PreviewRequest {
    pub(super) generation: u64,
    pub(super) path: PathBuf,
    pub(super) time: RationalTime,
    pub(super) cache_root: Option<PathBuf>,
    pub(super) playback: bool,
    pub(super) frame_rate: RationalTime,
    pub(super) transform: Option<prism_project::ClipTransform>,
    pub(super) cancelled: Arc<AtomicBool>,
}

type PreviewPayload = (
    prism_media::DecodedVideoFrame,
    RationalTime,
    Option<MediaFingerprint>,
    bool,
);
type PreviewReply = Result<PreviewPayload, String>;

#[derive(Debug)]
pub(super) struct PreviewWorker {
    pub(super) sender: SyncSender<PreviewRequest>,
    pub(super) receiver: Receiver<(u64, PreviewReply)>,
    pub(super) pending: RefCell<Option<PreviewRequest>>,
    pub(super) generation: Cell<u64>,
    pub(super) busy: Cell<bool>,
    pub(super) requested_at: Cell<Option<Instant>>,
    pub(super) stream_control: StreamControl,
}

#[derive(Debug)]
pub(super) struct ProxyGeneration {
    pub(super) receiver: Receiver<Result<PathBuf, String>>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) media_id: MediaAssetId,
    pub(super) source_fingerprint: ProxySourceFingerprint,
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.stream_control.stop();
    }
}

impl PreviewWorker {
    pub(super) fn new() -> Self {
        Self::with_thumbnails(false)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn with_thumbnails(thumbnails: bool) -> Self {
        let (sender, requests) = mpsc::sync_channel::<PreviewRequest>(1);
        let (replies, receiver) = mpsc::sync_channel(1);
        let stream_control = StreamControl::default();
        let decode_control = stream_control.clone();
        std::thread::spawn(move || {
            let mut stream: Option<StreamPreview> = None;
            while let Ok(mut request) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                if request.cancelled.load(Ordering::Acquire) {
                    continue;
                }
                if !request.playback {
                    stream.take();
                }
                let source_fingerprint = thumbnails
                    .then(|| fingerprint(&request.path).ok())
                    .flatten();
                let cached = if thumbnails {
                    request.cache_root.as_ref().and_then(|root| {
                        let entry = MediaCatalogEntry {
                            path: request.path.clone(),
                            kind: MediaItemKind::Video,
                            metadata: None,
                            fingerprint: source_fingerprint.clone(),
                            offline: false,
                            diagnostic: None,
                        };
                        let key = MediaArtifactWorker::thumbnail_variant_key(
                            &entry,
                            BROWSER_THUMBNAIL_VARIANT,
                        );
                        let layout = CacheLayout::new(root);
                        match layout.read(key, MediaArtifactKind::Thumbnail.extension()) {
                            Ok(Some(bytes)) => {
                                if let Ok(frame) = decode_thumbnail_artifact(&bytes) {
                                    Some(frame)
                                } else {
                                    let _ = layout
                                        .remove(key, MediaArtifactKind::Thumbnail.extension());
                                    None
                                }
                            }
                            Ok(None) | Err(_) => None,
                        }
                    })
                } else {
                    None
                };
                let result = if let Some(frame) = cached {
                    Ok((frame, RationalTime::ZERO, source_fingerprint, true))
                } else if request.playback {
                    (|| {
                        if !decode_control.active() {
                            return Err("Video playback stopped".to_owned());
                        }
                        if !stream.as_ref().is_some_and(|stream| {
                            stream.reusable(
                                &request.path,
                                request.time,
                                request.frame_rate,
                                request.transform.unwrap_or_default(),
                            )
                        }) {
                            stream.take();
                            stream = Some(StreamPreview::open(
                                &request.path,
                                request.time,
                                request.frame_rate,
                                request.transform.unwrap_or_default(),
                                &media_tool("ffmpeg"),
                                &media_tool("ffprobe"),
                                decode_control.clone(),
                                &request.cancelled,
                            )?);
                        }
                        let (frame, duration) = stream
                            .as_mut()
                            .ok_or("Video decoder unavailable")?
                            .frame(request.time)?;
                        Ok((frame, duration, None, false))
                    })()
                } else {
                    decode_preview(
                        &request.path,
                        request.time,
                        request.transform,
                        &request.cancelled,
                    )
                    .and_then(|(frame, duration)| {
                        if thumbnails {
                            let longest = u64::from(frame.width.max(frame.height)).max(1);
                            let width = u32::try_from(u64::from(frame.width) * 160 / longest)
                                .unwrap_or(1)
                                .max(1);
                            let height = u32::try_from(u64::from(frame.height) * 160 / longest)
                                .unwrap_or(1)
                                .max(1);
                            frame
                                .thumbnail(width, height)
                                .map(|frame| (frame, duration, source_fingerprint, false))
                                .map_err(|e| e.to_string())
                        } else {
                            Ok((frame, duration, None, false))
                        }
                    })
                };
                if request.playback && result.is_err() {
                    stream.take();
                }
                if replies.send((request.generation, result)).is_err() {
                    break;
                }
            }
        });
        Self {
            sender,
            receiver,
            pending: RefCell::new(None),
            generation: Cell::new(0),
            busy: Cell::new(false),
            requested_at: Cell::new(None),
            stream_control,
        }
    }

    pub(super) fn invalidate(&self) {
        self.stream_control.stop();
        self.busy.set(false);
        self.requested_at.set(None);
        self.generation.set(self.generation.get().wrapping_add(1));
        *self.pending.borrow_mut() = None;
    }

    pub(super) fn request(&self, path: PathBuf, time: RationalTime) {
        self.invalidate();
        self.busy.set(true);
        self.requested_at.set(Some(Instant::now()));
        *self.pending.borrow_mut() = Some(PreviewRequest {
            generation: self.generation.get(),
            path,
            time,
            cache_root: None,
            playback: false,
            frame_rate: RationalTime::from_integer(30),
            transform: None,
            cancelled: self.stream_control.request_token(),
        });
        self.flush();
    }

    pub(super) fn request_program(
        &self,
        path: PathBuf,
        time: RationalTime,
        transform: prism_project::ClipTransform,
    ) {
        self.invalidate();
        self.busy.set(true);
        self.requested_at.set(Some(Instant::now()));
        *self.pending.borrow_mut() = Some(PreviewRequest {
            generation: self.generation.get(),
            path,
            time,
            cache_root: None,
            playback: false,
            frame_rate: RationalTime::from_integer(30),
            transform: Some(transform),
            cancelled: self.stream_control.request_token(),
        });
        self.flush();
    }

    // Playback must let the active decode finish. Scrubbing still supersedes it.
    pub(super) fn request_playback(
        &self,
        path: PathBuf,
        time: RationalTime,
        frame_rate: RationalTime,
        transform: prism_project::ClipTransform,
    ) {
        if !self.busy.get() {
            self.stream_control.activate();
            self.generation.set(self.generation.get().wrapping_add(1));
            self.busy.set(true);
            self.requested_at.set(Some(Instant::now()));
            *self.pending.borrow_mut() = Some(PreviewRequest {
                generation: self.generation.get(),
                path,
                time,
                cache_root: None,
                playback: true,
                frame_rate,
                transform: Some(transform),
                cancelled: self.stream_control.request_token(),
            });
            self.flush();
        }
    }

    pub(super) fn request_thumbnail(&self, path: PathBuf, cache_root: Option<PathBuf>) {
        self.invalidate();
        self.busy.set(true);
        self.requested_at.set(Some(Instant::now()));
        *self.pending.borrow_mut() = Some(PreviewRequest {
            generation: self.generation.get(),
            path,
            time: RationalTime::ZERO,
            cache_root,
            playback: false,
            frame_rate: RationalTime::from_integer(30),
            transform: None,
            cancelled: self.stream_control.request_token(),
        });
        self.flush();
    }

    pub(super) fn flush(&self) {
        let pending = self.pending.borrow_mut().take();
        if let Some(request) = pending
            && let Err(TrySendError::Full(request)) = self.sender.try_send(request)
        {
            *self.pending.borrow_mut() = Some(request);
        }
    }

    pub(super) fn timed_out(&self, deadline: Duration) -> bool {
        self.busy.get()
            && self
                .requested_at
                .get()
                .is_some_and(|started| started.elapsed() > deadline)
    }

    pub(super) fn poll(&self) -> Option<Result<(slint::Image, RationalTime), String>> {
        self.poll_decoded().map(|result| {
            result.map(|(frame, duration, _, _)| {
                let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &frame.rgba,
                    frame.width,
                    frame.height,
                );
                (slint::Image::from_rgba8(buffer), duration)
            })
        })
    }

    pub(super) fn poll_decoded(&self) -> Option<PreviewReply> {
        self.flush();
        let mut result = None;
        while let Ok((generation, frame)) = self.receiver.try_recv() {
            if generation == self.generation.get() {
                self.busy.set(false);
                self.requested_at.set(None);
                result = Some(frame);
            }
        }
        result
    }
}

pub(super) fn decode_preview(
    path: &Path,
    time: RationalTime,
    transform: Option<prism_project::ClipTransform>,
    cancelled: &AtomicBool,
) -> Result<(prism_media::DecodedVideoFrame, RationalTime), String> {
    let metadata = FfprobeInspector::new(media_tool("ffprobe"))
        .inspect_with_control(
            path,
            prism_media::MediaProcessControl {
                timeout: Duration::from_secs(5),
                cancelled,
            },
        )
        .map_err(|e| e.to_string())?;
    let video = metadata
        .streams
        .iter()
        .find_map(|stream| match &stream.kind {
            StreamKind::Video(video) => Some(video),
            _ => None,
        })
        .ok_or_else(|| "No video stream available".to_owned())?;
    let (Some(width), Some(height)) = (video.width, video.height) else {
        return Err("Video dimensions unavailable".to_owned());
    };
    let decoder = FfmpegReferenceDecoder::new(media_tool("ffmpeg"));
    let control = prism_media::MediaProcessControl {
        timeout: Duration::from_secs(5),
        cancelled,
    };
    let frame = if let Some(transform) = transform {
        let mut graph = "[0:v:0]setpts=PTS-STARTPTS[program_raw];".to_owned();
        graph.push_str(
            &prism_render::ffmpeg_composited_clip_transform_filter(
                "program_raw",
                "program",
                width,
                height,
                transform,
            )
            .map_err(|error| error.to_string())?,
        );
        decoder.decode_video_frame_rgba_filtered_with_control(
            path, time, width, height, &graph, "program", control,
        )
    } else {
        decoder.decode_video_frame_rgba_with_control(path, 0, time, width, height, control)
    }
    .map_err(|e| e.to_string())?;
    Ok((frame, metadata.duration.unwrap_or(RationalTime::ZERO)))
}

pub(super) fn display_timecode(time: RationalTime) -> String {
    display_timecode_at_rate(time, RationalTime::from_integer(30))
}

pub(super) fn display_timecode_at_rate(time: RationalTime, frame_rate: RationalTime) -> String {
    let scale = u32::try_from(frame_rate.value()).unwrap_or(30);
    let frames = time
        .to_ticks(scale, prism_timeline::RoundingMode::Floor)
        .unwrap_or(0)
        .checked_div(i64::from(frame_rate.timescale()))
        .unwrap_or(0)
        .max(0);
    #[allow(clippy::cast_possible_truncation)]
    let nominal = frame_rate.as_seconds_f64().round().max(1.0) as i64;
    let minute = nominal.saturating_mul(60);
    let hour = minute.saturating_mul(60);
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        frames / hour,
        frames / minute % 60,
        frames / nominal % 60,
        frames % nominal
    )
}

pub(super) fn timing_text(time: RationalTime) -> slint::SharedString {
    if time.timescale() == 1 {
        time.value().to_string().into()
    } else {
        format!("{}/{}", time.value(), time.timescale()).into()
    }
}

pub(super) fn parse_timing(text: &str) -> Result<RationalTime, String> {
    let text = text.trim();
    let invalid = || "Enter non-negative seconds (1.5) or a fraction (3/2)".to_owned();
    let time = if let Some((numerator, denominator)) = text.split_once('/') {
        RationalTime::new(
            numerator.trim().parse().map_err(|_| invalid())?,
            denominator.trim().parse().map_err(|_| invalid())?,
        )
    } else if let Some((whole, fraction)) = text.split_once('.') {
        if fraction.is_empty()
            || fraction.len() > 6
            || !fraction.bytes().all(|c| c.is_ascii_digit())
            || !whole.bytes().all(|c| c.is_ascii_digit())
        {
            return Err(invalid());
        }
        let scale = 10_u32
            .checked_pow(u32::try_from(fraction.len()).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
        let whole: i64 = if whole.is_empty() {
            0
        } else {
            whole.parse().map_err(|_| invalid())?
        };
        let fraction: i64 = fraction.parse().map_err(|_| invalid())?;
        let value = whole
            .checked_mul(i64::from(scale))
            .and_then(|v| v.checked_add(fraction))
            .ok_or_else(invalid)?;
        RationalTime::new(value, scale)
    } else {
        RationalTime::new(text.parse().map_err(|_| invalid())?, 1)
    }
    .map_err(|e| e.to_string())?;
    if time.is_negative() {
        return Err(invalid());
    }
    Ok(time)
}
