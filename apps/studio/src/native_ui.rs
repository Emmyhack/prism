//! Native Slint adapter for the Prism desktop workspace.

use prism_cache::{CacheEvictionPolicy, CacheEvictionWorker, CacheLayout};
use prism_commands::{Command, CommandEngine, UndoRecord};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_export::{ExportPreset, SingleTrackMp4ExportRequest, VideoAudioMp4ExportRequest};
use prism_media::{
    FfmpegProxyGenerator, FfmpegReferenceDecoder, FfprobeInspector, ProxyProfile, ProxyRequest,
    StreamKind,
};
use prism_project::{Project, TrackKind, TransitionKind};
use prism_studio::desktop::{ExportController, ExportStatus, ExportWorker, ExportWorkerEvent};
use prism_timeline::RationalTime;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};
use thiserror::Error;

slint::include_modules!();

const PROXY_AVAILABLE: &str = "AVAILABLE";
const PROXY_MISSING: &str = "MISSING";
const PROXY_STALE: &str = "STALE";

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    const GIB: u64 = MIB * 1_024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn proxy_status(source: &Path, proxy: Option<&Path>) -> &'static str {
    let Some(proxy) = proxy else {
        return PROXY_MISSING;
    };
    if !proxy.is_file() {
        return PROXY_MISSING;
    }
    let Ok(source_modified) = std::fs::metadata(source).and_then(|metadata| metadata.modified())
    else {
        return PROXY_STALE;
    };
    let Ok(proxy_modified) = std::fs::metadata(proxy).and_then(|metadata| metadata.modified())
    else {
        return PROXY_STALE;
    };
    if proxy_modified < source_modified {
        PROXY_STALE
    } else {
        PROXY_AVAILABLE
    }
}

fn next_proxy_version(base: &Path) -> Result<PathBuf, String> {
    let parent = base
        .parent()
        .ok_or_else(|| "Proxy destination has no parent directory".to_owned())?;
    let stem = base
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Prism.proxy");
    let (stem, first_version) = stem
        .rsplit_once(".v")
        .and_then(|(root, version)| {
            version
                .parse::<u32>()
                .ok()
                .map(|version| (root, version + 1))
        })
        .unwrap_or((stem, 2));
    for version in first_version..=9_999 {
        let candidate = parent.join(format!("{stem}.v{version}.mp4"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("No available versioned proxy filename was found".to_owned())
}

#[derive(Debug, Eq, PartialEq)]
struct NativeExportStatus {
    label: &'static str,
    output: String,
    progress: i32,
    error: String,
    can_cancel: bool,
    can_retry: bool,
    can_remove: bool,
}

impl From<&ExportStatus> for NativeExportStatus {
    fn from(status: &ExportStatus) -> Self {
        Self {
            label: status.label,
            output: status.output.clone(),
            progress: i32::from(status.progress_percent.unwrap_or(0)),
            error: status.error.clone().unwrap_or_default(),
            can_cancel: status.can_cancel,
            can_retry: status.can_retry,
            can_remove: status.can_remove,
        }
    }
}

fn apply_export_status(ui: &PrismStudio, status: Option<&ExportStatus>) {
    if let Some(status) = status {
        let status = NativeExportStatus::from(status);
        ui.set_export_status(status.label.into());
        ui.set_export_output(status.output.into());
        ui.set_export_progress(status.progress);
        ui.set_export_error(status.error.into());
        ui.set_export_can_cancel(status.can_cancel);
        ui.set_export_can_retry(status.can_retry);
        ui.set_export_can_remove(status.can_remove);
    } else {
        ui.set_export_status("READY".into());
        ui.set_export_progress(-1);
        ui.set_export_error("".into());
        ui.set_export_can_cancel(false);
        ui.set_export_can_retry(false);
        ui.set_export_can_remove(false);
    }
}

fn launch_default_export(
    controller: &mut ExportController,
    worker: &mut Option<ExportWorker>,
    project: &Project,
    output: PathBuf,
) -> Result<(), String> {
    controller.start().map_err(|error| error.to_string())?;
    let target = project
        .sequences
        .iter()
        .find_map(|(sequence_id, sequence)| {
            sequence
                .tracks
                .iter()
                .find(|track| {
                    track.kind == TrackKind::Video && track.visible && !track.clips.is_empty()
                })
                .map(|track| {
                    let audio = sequence.tracks.iter().find(|candidate| {
                        candidate.kind == TrackKind::Audio
                            && !candidate.muted
                            && !candidate.clips.is_empty()
                    });
                    (*sequence_id, track.id, audio.map(|candidate| candidate.id))
                })
        });
    let Some((sequence_id, track_id, audio_track_id)) = target else {
        controller
            .fail("project has no visible non-empty video track")
            .map_err(|error| error.to_string())?;
        return Ok(());
    };
    *worker = Some(if let Some(audio_track_id) = audio_track_id {
        ExportWorker::spawn_single_video_audio_track(
            PathBuf::from("ffmpeg"),
            PathBuf::from("ffprobe"),
            project.clone(),
            sequence_id,
            track_id,
            audio_track_id,
            VideoAudioMp4ExportRequest::from_preset(ExportPreset::Youtube1080p, output),
        )
    } else {
        ExportWorker::spawn_single_video_track(
            PathBuf::from("ffmpeg"),
            PathBuf::from("ffprobe"),
            project.clone(),
            sequence_id,
            track_id,
            SingleTrackMp4ExportRequest::from_preset(ExportPreset::Youtube1080p, output),
        )
    });
    Ok(())
}

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
struct PreviewRequest {
    generation: u64,
    path: PathBuf,
    time: RationalTime,
}

#[derive(Debug)]
struct PreviewWorker {
    sender: SyncSender<PreviewRequest>,
    receiver: Receiver<(
        u64,
        Result<(prism_media::DecodedVideoFrame, RationalTime), String>,
    )>,
    pending: RefCell<Option<PreviewRequest>>,
    generation: Cell<u64>,
}

#[derive(Debug)]
struct ProxyGeneration {
    receiver: Receiver<Result<PathBuf, String>>,
    cancelled: Arc<AtomicBool>,
    media_id: MediaAssetId,
}

impl PreviewWorker {
    fn new() -> Self {
        Self::with_thumbnails(false)
    }

    fn with_thumbnails(thumbnails: bool) -> Self {
        let (sender, requests) = mpsc::sync_channel::<PreviewRequest>(1);
        let (replies, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            while let Ok(mut request) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let result =
                    decode_preview(&request.path, request.time).and_then(|(frame, duration)| {
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
                                .map(|frame| (frame, duration))
                                .map_err(|e| e.to_string())
                        } else {
                            Ok((frame, duration))
                        }
                    });
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
        }
    }

    fn invalidate(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        *self.pending.borrow_mut() = None;
    }

    fn request(&self, path: PathBuf, time: RationalTime) {
        self.invalidate();
        *self.pending.borrow_mut() = Some(PreviewRequest {
            generation: self.generation.get(),
            path,
            time,
        });
        self.flush();
    }

    fn flush(&self) {
        let pending = self.pending.borrow_mut().take();
        if let Some(request) = pending {
            if let Err(TrySendError::Full(request)) = self.sender.try_send(request) {
                *self.pending.borrow_mut() = Some(request);
            }
        }
    }

    fn poll(&self) -> Option<Result<(slint::Image, RationalTime), String>> {
        self.flush();
        let mut result = None;
        while let Ok((generation, frame)) = self.receiver.try_recv() {
            if generation == self.generation.get() {
                result = Some(frame.map(|(frame, duration)| {
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &frame.rgba,
                        frame.width,
                        frame.height,
                    );
                    (slint::Image::from_rgba8(buffer), duration)
                }));
            }
        }
        result
    }
}

fn decode_preview(
    path: &Path,
    time: RationalTime,
) -> Result<(prism_media::DecodedVideoFrame, RationalTime), String> {
    let metadata = FfprobeInspector::new("ffprobe")
        .inspect(path)
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
    let frame = FfmpegReferenceDecoder::new("ffmpeg")
        .decode_video_frame_rgba(path, 0, time, width, height)
        .map_err(|e| e.to_string())?;
    Ok((frame, metadata.duration.unwrap_or(RationalTime::ZERO)))
}

fn display_timecode(time: RationalTime) -> String {
    let frames = time
        .to_ticks(30, prism_timeline::RoundingMode::Floor)
        .unwrap_or(0)
        .max(0);
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        frames / 108_000,
        frames / 1_800 % 60,
        frames / 30 % 60,
        frames % 30
    )
}

fn timing_text(time: RationalTime) -> slint::SharedString {
    if time.timescale() == 1 {
        time.value().to_string().into()
    } else {
        format!("{}/{}", time.value(), time.timescale()).into()
    }
}

fn parse_timing(text: &str) -> Result<RationalTime, String> {
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

struct NativeExportRuntime {
    project: RefCell<Option<Project>>,
    project_path: RefCell<Option<PathBuf>>,
    controller: RefCell<ExportController>,
    worker: RefCell<Option<ExportWorker>>,
    program_preview: PreviewWorker,
    source_preview: PreviewWorker,
    playhead: Cell<RationalTime>,
    playing: Cell<bool>,
    playback_started: Cell<Option<Instant>>,
    playback_origin: Cell<RationalTime>,
    audio_process: RefCell<Option<Child>>,
    source_position: Cell<RationalTime>,
    source_duration: Cell<RationalTime>,
    source_in: Cell<RationalTime>,
    source_out: Cell<RationalTime>,
    undo: RefCell<Vec<UndoRecord>>,
    redo: RefCell<Vec<UndoRecord>>,
    thumbnail_preview: PreviewWorker,
    thumbnail_queue: RefCell<VecDeque<(usize, PathBuf)>>,
    thumbnail_current: Cell<Option<usize>>,
    thumbnail_total: Cell<usize>,
    thumbnail_completed: Cell<usize>,
    thumbnail_failed: Cell<usize>,
    cards: RefCell<Vec<MediaCard>>,
    use_proxy: Cell<bool>,
    proxy_generation: RefCell<Option<ProxyGeneration>>,
    recovery_candidates: RefCell<Vec<prism_persistence::AutosaveCandidate>>,
    autosave_scheduler: RefCell<Option<prism_persistence::AutosaveScheduler>>,
    pending_autosave: RefCell<Option<Project>>,
    cache_eviction: RefCell<Option<CacheEvictionWorker>>,
}

impl NativeExportRuntime {
    fn new(project: Option<Project>, project_path: Option<PathBuf>) -> Self {
        Self {
            project: RefCell::new(project),
            project_path: RefCell::new(project_path),
            controller: RefCell::new(ExportController::new()),
            worker: RefCell::new(None),
            program_preview: PreviewWorker::new(),
            source_preview: PreviewWorker::new(),
            playhead: Cell::new(RationalTime::ZERO),
            playing: Cell::new(false),
            playback_started: Cell::new(None),
            playback_origin: Cell::new(RationalTime::ZERO),
            audio_process: RefCell::new(None),
            source_position: Cell::new(RationalTime::ZERO),
            source_duration: Cell::new(RationalTime::ZERO),
            source_in: Cell::new(RationalTime::ZERO),
            source_out: Cell::new(RationalTime::ZERO),
            undo: RefCell::new(Vec::new()),
            redo: RefCell::new(Vec::new()),
            thumbnail_preview: PreviewWorker::with_thumbnails(true),
            thumbnail_queue: RefCell::new(VecDeque::new()),
            thumbnail_current: Cell::new(None),
            thumbnail_total: Cell::new(0),
            thumbnail_completed: Cell::new(0),
            thumbnail_failed: Cell::new(0),
            cards: RefCell::new(Vec::new()),
            use_proxy: Cell::new(false),
            proxy_generation: RefCell::new(None),
            recovery_candidates: RefCell::new(Vec::new()),
            autosave_scheduler: RefCell::new(None),
            pending_autosave: RefCell::new(None),
            cache_eviction: RefCell::new(None),
        }
    }

    fn handle(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        match command {
            "project.new" => self.new_project(ui),
            "project.save" => self.save(ui),
            "edit.undo" | "edit.redo" => self.apply_history(ui, command == "edit.redo"),
            "timeline.apply_timing" => self.apply_timing(ui),
            "timeline.move_left"
            | "timeline.move_right"
            | "timeline.trim_in_left"
            | "timeline.trim_in_right"
            | "timeline.trim_out_left"
            | "timeline.trim_out_right" => self.adjust_clip_frame(ui, command),
            "timeline.slip_left"
            | "timeline.slip_right"
            | "timeline.roll_in_left"
            | "timeline.roll_in_right"
            | "timeline.roll_out_left"
            | "timeline.roll_out_right"
            | "timeline.slide_left"
            | "timeline.slide_right" => self.precision_edit_frame(ui, command),
            "source.append" => self.append_source(ui),
            "media.assign_proxy" => self.assign_proxy(ui),
            "media.relink_source" => self.relink_source(ui),
            "media.clear_proxy" => self.clear_proxy(ui),
            "media.generate_proxy" => self.generate_proxy(ui),
            "media.regenerate_proxy" => self.regenerate_proxy(ui),
            "media.cancel_proxy" => {
                if let Some(generation) = self.proxy_generation.borrow().as_ref() {
                    generation.cancelled.store(true, Ordering::Release);
                    ui.set_feedback("Cancelling proxy generation…".into());
                }
                Ok(())
            }
            "cache.clean" => self.clean_cache(ui),
            "media.toggle_proxy" => {
                self.use_proxy.set(!self.use_proxy.get());
                ui.set_proxy_active(self.use_proxy.get());
                self.select_media(ui, ui.get_selected_media_index());
                self.seek_time(ui, self.playhead.get());
                Ok(())
            }
            "source.mark_in" | "source.mark_out" => {
                if !ui.get_source_ready() {
                    return Err("Select an available source first".to_owned());
                }
                let position = self.source_position.get();
                if command == "source.mark_in" {
                    if position >= self.source_out.get() {
                        return Err("In point must be before Out".to_owned());
                    }
                    self.source_in.set(position);
                } else {
                    let end = position
                        .checked_add(RationalTime::new(1, 30).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?
                        .min(self.source_duration.get());
                    if end <= self.source_in.get() {
                        return Err("Out point must be after In".to_owned());
                    }
                    self.source_out.set(end);
                }
                self.update_source_labels(ui);
                Ok(())
            }
            "timeline.split" | "timeline.delete" => self.edit_clip(command, ui),
            "timeline.ripple_delete" => self.edit_clip(command, ui),
            "timeline.add_marker" => self.add_marker(ui),
            "timeline.delete_marker" => self.delete_marker(ui),
            "timeline.toggle_dissolve" => self.toggle_dissolve(ui),
            "timeline.duplicate" => self.duplicate_clip(ui),
            "timeline.track_up" => self.change_track(ui, ui.get_selected_track_index(), "up"),
            "timeline.track_down" => self.change_track(ui, ui.get_selected_track_index(), "down"),
            "timeline.toggle_snap" => {
                ui.set_snap_enabled(!ui.get_snap_enabled());
                ui.set_feedback(
                    if ui.get_snap_enabled() {
                        "Timeline snapping enabled"
                    } else {
                        "Timeline snapping disabled"
                    }
                    .into(),
                );
                Ok(())
            }
            "playback.play_pause" => {
                let playing = !self.playing.get();
                self.playing.set(playing);
                ui.set_playing(playing);
                if playing {
                    self.playback_origin.set(self.playhead.get());
                    self.playback_started.set(Some(Instant::now()));
                    if let Err(error) = self.start_audio_playback() {
                        ui.set_feedback(error.into());
                    }
                } else {
                    self.playback_started.set(None);
                }
                Ok(())
            }
            "playback.step_backward" | "playback.step_forward" => {
                self.stop_playback(ui);
                let delta = RationalTime::new(
                    if command == "playback.step_forward" {
                        1
                    } else {
                        -1
                    },
                    30,
                )
                .map_err(|e| e.to_string())?;
                let time = self
                    .playhead
                    .get()
                    .checked_add(delta)
                    .map_err(|e| e.to_string())?;
                self.seek_time(ui, time.max(RationalTime::ZERO));
                Ok(())
            }
            "export.destination" => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export video")
                    .set_file_name("Prism export.mp4")
                    .add_filter("MP4 video", &["mp4"])
                    .save_file()
                {
                    ui.set_export_output(path.display().to_string().into());
                }
                Ok(())
            }
            "project.open" => self.open_project(ui),
            "recovery.recover_latest" => self.recover_latest(ui),
            "project.import_media" => self.import_media(ui),
            "export.queue" => self.queue(ui),
            "export.cancel" => {
                if let Some(worker) = self.worker.borrow().as_ref() {
                    worker.cancel();
                }
                self.controller
                    .borrow_mut()
                    .cancel()
                    .map_err(|error| error.to_string())
            }
            "export.retry" => self.retry(),
            "export.remove" => self
                .controller
                .borrow_mut()
                .remove()
                .map_err(|error| error.to_string()),
            _ => Err("This command is not available".to_owned()),
        }
    }

    fn new_project(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Create Prism Project")
            .set_file_name("Untitled.prism")
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled");
        let project = Project::new(name);
        prism_persistence::save_atomic(&project, &path).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &path);
        ui.set_export_output(
            path.with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(path);
        self.reset_view(ui);
        Ok(())
    }

    fn open_project(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open Prism Project")
            .add_filter("Prism Project", &["prism", "json"])
            .pick_file()
        else {
            return Ok(());
        };
        let project = prism_persistence::load(&path).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &path);
        ui.set_export_output(
            path.with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(path);
        self.reset_view(ui);
        Ok(())
    }

    fn queue(&self, ui: &PrismStudio) -> Result<(), String> {
        let project = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "open a project before exporting".to_owned())?;
        if self.worker.borrow().is_some() {
            return Err("An export is already running".to_owned());
        }
        if ui.get_export_output().trim().is_empty() {
            return Err("Choose an output destination first".to_owned());
        }
        let output = PathBuf::from(ui.get_export_output().as_str());
        let mut controller = self.controller.borrow_mut();
        controller.queue(output.clone());
        launch_default_export(
            &mut controller,
            &mut self.worker.borrow_mut(),
            &project,
            output,
        )
    }

    fn retry(&self) -> Result<(), String> {
        let project = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "open a project before exporting".to_owned())?;
        let mut controller = self.controller.borrow_mut();
        controller.retry().map_err(|error| error.to_string())?;
        let output = controller
            .status()
            .map(|status| PathBuf::from(status.output))
            .ok_or_else(|| "export job is unavailable".to_owned())?;
        launch_default_export(
            &mut controller,
            &mut self.worker.borrow_mut(),
            &project,
            output,
        )
    }

    fn import_media(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("Import Media")
            .add_filter(
                "Video",
                &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpeg", "mpg"],
            )
            .pick_files()
        else {
            return Ok(());
        };
        let mut project = self.project.borrow().clone().unwrap_or_else(|| {
            let name = paths
                .first()
                .and_then(|path| path.file_stem())
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled");
            Project::new(name)
        });
        for path in paths {
            import_video_into_project(&mut project, &path)?;
        }
        let project_path = if let Some(path) = self.project_path.borrow().clone() {
            path
        } else {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Save Prism Project")
                .set_file_name(format!("{}.prism", project.name))
                .add_filter("Prism Project", &["prism"])
                .save_file()
            else {
                return Ok(());
            };
            path
        };
        prism_persistence::save_atomic(&project, &project_path)
            .map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &project_path);
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(project_path);
        self.reset_view(ui);
        Ok(())
    }

    fn save(&self, ui: &PrismStudio) -> Result<(), String> {
        let project = self.project.borrow();
        let project = project
            .as_ref()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let path = self.project_path.borrow();
        let path = path
            .as_ref()
            .ok_or_else(|| "Project has no save destination".to_owned())?;
        prism_persistence::save_atomic(project, path).map_err(|e| e.to_string())?;
        ui.set_feedback("Project saved".into());
        Ok(())
    }

    fn refresh_recovery(&self, ui: &PrismStudio) {
        let candidates = self
            .project_path
            .borrow()
            .as_deref()
            .map(prism_persistence::list_autosaves)
            .transpose();
        match candidates {
            Ok(Some(candidates)) => {
                ui.set_recovery_count(i32::try_from(candidates.len()).unwrap_or(i32::MAX));
                if let Some(candidate) = candidates.first() {
                    ui.set_recovery_revision(candidate.revision.to_string().into());
                    ui.set_recovery_path(candidate.path.display().to_string().into());
                } else {
                    ui.set_recovery_revision("".into());
                    ui.set_recovery_path("".into());
                }
                *self.recovery_candidates.borrow_mut() = candidates;
            }
            Ok(None) => {
                self.recovery_candidates.borrow_mut().clear();
                ui.set_recovery_count(0);
                ui.set_recovery_revision("".into());
                ui.set_recovery_path("".into());
            }
            Err(error) => {
                self.recovery_candidates.borrow_mut().clear();
                ui.set_recovery_count(0);
                ui.set_recovery_revision("".into());
                ui.set_recovery_path("".into());
                ui.set_feedback(format!("Could not inspect recovery files: {error}").into());
            }
        }
    }

    fn reset_autosave_scheduler(&self, ui: &PrismStudio) {
        self.autosave_scheduler.borrow_mut().take();
        self.pending_autosave.borrow_mut().take();
        let Some(path) = self.project_path.borrow().clone() else {
            ui.set_autosave_status("UNAVAILABLE".into());
            return;
        };
        match prism_persistence::AutosaveScheduler::spawn(path, 2) {
            Ok(scheduler) => {
                *self.autosave_scheduler.borrow_mut() = Some(scheduler);
                ui.set_autosave_status("READY".into());
            }
            Err(error) => {
                ui.set_autosave_status("FAILED".into());
                ui.set_feedback(format!("Could not start autosave: {error}").into());
            }
        }
    }

    fn queue_autosave(&self, ui: &PrismStudio, project: &Project) {
        if self.pending_autosave.borrow().is_some() {
            *self.pending_autosave.borrow_mut() = Some(project.clone());
            ui.set_autosave_status("BUSY".into());
            return;
        }
        let scheduler_ref = self.autosave_scheduler.borrow();
        let Some(scheduler) = scheduler_ref.as_ref() else {
            ui.set_autosave_status("UNAVAILABLE".into());
            return;
        };
        match scheduler.try_submit(project) {
            prism_persistence::AutosaveSubmit::Queued => {
                ui.set_autosave_status("QUEUED".into());
            }
            prism_persistence::AutosaveSubmit::QueueFull => {
                *self.pending_autosave.borrow_mut() = Some(project.clone());
                ui.set_autosave_status("BUSY".into());
            }
            prism_persistence::AutosaveSubmit::WorkerStopped => {
                ui.set_autosave_status("FAILED".into());
            }
        }
    }

    fn retry_pending_autosave(&self, ui: &PrismStudio) {
        let Some(project) = self.pending_autosave.borrow().clone() else {
            return;
        };
        let scheduler_ref = self.autosave_scheduler.borrow();
        let Some(scheduler) = scheduler_ref.as_ref() else {
            return;
        };
        match scheduler.try_submit(&project) {
            prism_persistence::AutosaveSubmit::Queued => {
                self.pending_autosave.borrow_mut().take();
                ui.set_autosave_status("QUEUED".into());
            }
            prism_persistence::AutosaveSubmit::QueueFull => {
                ui.set_autosave_status("BUSY".into());
            }
            prism_persistence::AutosaveSubmit::WorkerStopped => {
                ui.set_autosave_status("FAILED".into());
            }
        }
    }

    fn recover_latest(&self, ui: &PrismStudio) -> Result<(), String> {
        let source = self
            .recovery_candidates
            .borrow()
            .first()
            .map(|candidate| candidate.path.clone())
            .ok_or_else(|| "No recovery candidate is available".to_owned())?;
        let default_name = self
            .project_path
            .borrow()
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}.recovered.prism"))
            .unwrap_or_else(|| "Recovered.prism".to_owned());
        let Some(destination) = rfd::FileDialog::new()
            .set_title("Recover Prism Autosave As")
            .set_file_name(default_name)
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        let revision = prism_persistence::recover_autosave(&source, &destination)
            .map_err(|error| error.to_string())?;
        let project = prism_persistence::load(&destination).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &destination);
        ui.set_export_output(
            destination
                .with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(destination);
        self.reset_view(ui);
        ui.set_feedback(
            format!("Recovered autosave revision {revision} into a new project").into(),
        );
        Ok(())
    }

    fn reset_view(&self, ui: &PrismStudio) {
        self.stop_playback(ui);
        if let Some(generation) = self.proxy_generation.borrow().as_ref() {
            generation.cancelled.store(true, Ordering::Release);
        }
        self.proxy_generation.borrow_mut().take();
        ui.set_proxy_generating(false);
        self.undo.borrow_mut().clear();
        self.redo.borrow_mut().clear();
        self.update_history(ui);
        ui.set_timeline_zoom(1.0);
        ui.set_timeline_offset(0.0);
        ui.set_snap_enabled(true);
        self.refresh_recovery(ui);
        self.reset_autosave_scheduler(ui);
        self.cache_eviction.borrow_mut().take();
        self.update_cache_state(ui);
        self.source_duration.set(RationalTime::ZERO);
        self.source_position.set(RationalTime::ZERO);
        self.source_in.set(RationalTime::ZERO);
        self.source_out.set(RationalTime::ZERO);
        ui.set_source_ready(false);
        self.update_source_labels(ui);
        self.reset_thumbnails(ui);
        ui.set_inspector_name("".into());
        ui.set_inspector_path("".into());
        ui.set_selected_proxy("".into());
        ui.set_proxy_status(PROXY_MISSING.into());
        self.use_proxy.set(false);
        ui.set_proxy_active(false);
        ui.set_inspector_locked(false);
        ui.set_selected_clip_index(-1);
        ui.set_selected_media_index(-1);
        ui.set_selected_media("".into());
        ui.set_has_source_frame(false);
        ui.set_source_detail("Select footage in the media browser".into());
        self.source_preview.invalidate();
        ui.set_active_workspace("EDIT".into());
        self.seek_time(ui, RationalTime::ZERO);
    }

    fn cache_root(&self) -> Option<PathBuf> {
        self.project_path
            .borrow()
            .as_ref()
            .map(|path| path.with_extension("prism-cache"))
    }

    fn update_cache_state(&self, ui: &PrismStudio) {
        let pending = self
            .cache_eviction
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.pending_evictions() > 0);
        ui.set_cache_cleaning(pending);
        let Some(root) = self.cache_root() else {
            ui.set_cache_status("UNAVAILABLE".into());
            ui.set_cache_detail("Save the project to enable its derived cache".into());
            return;
        };
        match CacheLayout::new(root).inventory() {
            Ok(inventory) => {
                ui.set_cache_status(if pending { "CLEANING" } else { "READY" }.into());
                ui.set_cache_detail(
                    format!(
                        "{} artifacts · {}",
                        inventory.artifact_count,
                        format_bytes(inventory.total_bytes)
                    )
                    .into(),
                );
            }
            Err(error) => {
                ui.set_cache_status("FAILED".into());
                ui.set_cache_detail(error.to_string().into());
            }
        }
    }

    fn clean_cache(&self, ui: &PrismStudio) -> Result<(), String> {
        if self
            .cache_eviction
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.pending_evictions() > 0)
        {
            return Err("Cache cleanup is already running".to_owned());
        }
        let root = self
            .cache_root()
            .ok_or_else(|| "Save the project before cleaning its cache".to_owned())?;
        let worker = CacheEvictionWorker::spawn(CacheLayout::new(root), 1);
        worker
            .try_submit(CacheEvictionPolicy {
                max_bytes: Some(0),
                max_age: None,
            })
            .map_err(|error| error.to_string())?;
        *self.cache_eviction.borrow_mut() = Some(worker);
        ui.set_cache_cleaning(true);
        ui.set_cache_status("CLEANING".into());
        ui.set_feedback("Cleaning project derived cache in the background…".into());
        Ok(())
    }

    fn select_media(&self, ui: &PrismStudio, index: i32) {
        let project = self.project.borrow();
        let Some(asset) = project
            .as_ref()
            .and_then(|p| p.media.values().nth(usize::try_from(index).ok()?))
        else {
            return;
        };
        ui.set_selected_media_index(index);
        ui.set_selected_media(
            Path::new(&asset.source_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&asset.source_path)
                .into(),
        );
        self.source_position.set(RationalTime::ZERO);
        self.source_duration.set(RationalTime::ZERO);
        self.source_in.set(RationalTime::ZERO);
        self.source_out.set(RationalTime::ZERO);
        ui.set_source_ready(false);
        self.update_source_labels(ui);
        ui.set_has_source_frame(false);
        ui.set_source_detail(asset.source_path.clone().into());
        ui.set_selected_proxy(asset.proxy_path.clone().unwrap_or_default().into());
        let status = proxy_status(
            Path::new(&asset.source_path),
            asset.proxy_path.as_deref().map(Path::new),
        );
        ui.set_proxy_status(status.into());
        if status != PROXY_AVAILABLE {
            self.use_proxy.set(false);
            ui.set_proxy_active(false);
        }
        let preview_path = if self.use_proxy.get() && status == PROXY_AVAILABLE {
            asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
        } else {
            &asset.source_path
        };
        self.source_preview
            .request(PathBuf::from(preview_path), RationalTime::ZERO);
    }

    fn select_clip(&self, ui: &PrismStudio, index: i32) {
        ui.set_selected_clip_index(index);
        self.update_inspector(ui);
        let selected = self.project.borrow().as_ref().and_then(|p| {
            let sequence = p.sequences.values().next()?;
            let clip = sequence
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .nth(usize::try_from(index).ok()?)?;
            let media_index = p.media.keys().position(|id| *id == clip.media_id)?;
            Some((clip.timeline_start, media_index))
        });
        if let Some((time, media_index)) = selected {
            if let Ok(index) = i32::try_from(media_index) {
                ui.set_selected_media_index(index);
                if let Some(name) = slint::Model::row_data(&ui.get_media_items(), media_index) {
                    ui.set_selected_media(name);
                }
                self.select_media(ui, index);
            }
            let _ = time;
        }
    }

    fn seek_ratio(&self, ui: &PrismStudio, ratio: f32) {
        self.stop_playback(ui);
        if !ratio.is_finite() {
            return;
        }
        let duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next()?.duration().ok());
        if let Some(duration) = duration {
            // Pointer coordinates are display values; quantize once at the UI boundary.
            #[allow(clippy::cast_possible_truncation)]
            let frame = (duration.as_seconds_f64() * f64::from(ratio.clamp(0.0, 1.0)) * 30.0)
                .floor() as i64;
            if let Ok(time) = RationalTime::new(frame, 30) {
                self.seek_time(ui, time);
            }
        }
    }

    fn seek_time(&self, ui: &PrismStudio, time: RationalTime) {
        let project = self.project.borrow();
        let sequence = project.as_ref().and_then(|p| p.sequences.values().next());
        let duration = sequence
            .and_then(|s| s.duration().ok())
            .unwrap_or(RationalTime::ZERO);
        let time = time.min(duration).max(RationalTime::ZERO);
        self.playhead.set(time);
        ui.set_timecode(display_timecode(time).into());
        #[allow(clippy::cast_possible_truncation)]
        let ratio = if duration.is_positive() {
            (time.as_seconds_f64() / duration.as_seconds_f64()) as f32
        } else {
            0.0
        };
        ui.set_playhead_ratio(ratio);
        ui.set_has_program_frame(false);
        let target = sequence.and_then(|s| {
            s.tracks
                .iter()
                .filter(|t| t.visible && t.kind == TrackKind::Video)
                .flat_map(|t| &t.clips)
                .find_map(|clip| {
                    let end = clip.timeline_start.checked_add(clip.source_duration).ok()?;
                    if time < clip.timeline_start || time >= end {
                        return None;
                    }
                    let asset = project.as_ref()?.media.get(&clip.media_id)?;
                    let source_time = time
                        .checked_sub(clip.timeline_start)
                        .ok()?
                        .checked_add(clip.source_start)
                        .ok()?;
                    let proxy_is_available = proxy_status(
                        Path::new(&asset.source_path),
                        asset.proxy_path.as_deref().map(Path::new),
                    ) == PROXY_AVAILABLE;
                    let path = if self.use_proxy.get() && proxy_is_available {
                        asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
                    } else {
                        &asset.source_path
                    };
                    Some((PathBuf::from(path), source_time))
                })
        });
        if let Some((path, time)) = target {
            ui.set_preview_status("Decoding frame…".into());
            self.program_preview.request(path, time);
        } else {
            self.program_preview.invalidate();
            ui.set_preview_status(
                if duration.is_positive() {
                    "Gap / no visible video at playhead"
                } else {
                    "Import footage to create a sequence"
                }
                .into(),
            );
        }
    }

    fn stop_playback(&self, ui: &PrismStudio) {
        self.playing.set(false);
        self.playback_started.set(None);
        if let Some(mut child) = self.audio_process.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        ui.set_playing(false);
    }

    fn start_audio_playback(&self) -> Result<(), String> {
        if let Some(mut child) = self.audio_process.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let project = self.project.borrow();
        let Some(project) = project.as_ref() else {
            return Ok(());
        };
        let Some(sequence) = project.sequences.values().next() else {
            return Ok(());
        };
        let playhead = self.playhead.get();
        let source = sequence
            .tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio && !track.muted)
            .flat_map(|track| &track.clips)
            .find_map(|clip| {
                let end = clip.timeline_start.checked_add(clip.source_duration).ok()?;
                if playhead < clip.timeline_start || playhead >= end {
                    return None;
                }
                let asset = project.media.get(&clip.media_id)?;
                let offset = playhead.checked_sub(clip.timeline_start).ok()?;
                let source_time = clip.source_start.checked_add(offset).ok()?;
                Some((PathBuf::from(&asset.source_path), source_time))
            });
        let Some((path, source_time)) = source else {
            return Ok(());
        };
        let child = ProcessCommand::new("ffplay")
            .args(["-nodisp", "-autoexit", "-loglevel", "quiet", "-ss"])
            .arg(format!("{:.6}", source_time.as_seconds_f64()))
            .arg("-i")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Audio preview unavailable: {error}"))?;
        *self.audio_process.borrow_mut() = Some(child);
        Ok(())
    }

    fn commit(&self, ui: &PrismStudio, command: Command, label: &str) -> Result<(), String> {
        self.commit_commands(ui, vec![command], label)
    }

    fn selected_media_id(&self, ui: &PrismStudio) -> Result<MediaAssetId, String> {
        self.project
            .borrow()
            .as_ref()
            .and_then(|project| {
                project
                    .media
                    .keys()
                    .nth(usize::try_from(ui.get_selected_media_index()).ok()?)
                    .copied()
            })
            .ok_or_else(|| "Select media first".to_owned())
    }

    fn assign_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let Some(path) = rfd::FileDialog::new()
            .set_title("Assign Proxy Media")
            .add_filter("Video", &["mp4", "mov", "m4v", "mkv", "webm"])
            .pick_file()
        else {
            return Ok(());
        };
        let index = ui.get_selected_media_index();
        self.commit(
            ui,
            Command::SetMediaProxy {
                media_id,
                proxy_path: Some(path.display().to_string()),
            },
            "Assign media proxy",
        )?;
        self.use_proxy.set(true);
        ui.set_proxy_active(true);
        self.select_media(ui, index);
        Ok(())
    }

    fn relink_source(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Relink Source Media")
            .add_filter(
                "Video",
                &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpeg", "mpg"],
            )
            .pick_file()
        else {
            return Ok(());
        };
        self.relink_source_to(ui, path)
    }

    fn relink_source_to(&self, ui: &PrismStudio, path: PathBuf) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let metadata = FfprobeInspector::new("ffprobe")
            .inspect(&path)
            .map_err(|error| error.to_string())?;
        if !metadata
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
        {
            return Err("The replacement file has no video stream".to_owned());
        }
        let available = metadata
            .duration
            .filter(|duration| duration.is_positive())
            .ok_or_else(|| "The replacement file has no valid duration".to_owned())?;
        let required = self
            .project
            .borrow()
            .as_ref()
            .into_iter()
            .flat_map(|project| project.sequences.values())
            .flat_map(|sequence| &sequence.tracks)
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.media_id == media_id)
            .filter_map(|clip| clip.source_start.checked_add(clip.source_duration).ok())
            .max()
            .unwrap_or(RationalTime::ZERO);
        if available < required {
            return Err(format!(
                "Replacement is too short: clips require {}, file provides {}",
                timing_text(required),
                timing_text(available)
            ));
        }
        let index = ui.get_selected_media_index();
        self.commit(
            ui,
            Command::RelinkMedia {
                media_id,
                source_path: path.display().to_string(),
            },
            "Relink source media",
        )?;
        self.reset_thumbnails(ui);
        self.select_media(ui, index);
        self.update_inspector(ui);
        Ok(())
    }

    fn clear_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let index = ui.get_selected_media_index();
        self.commit(
            ui,
            Command::SetMediaProxy {
                media_id,
                proxy_path: None,
            },
            "Clear media proxy",
        )?;
        self.use_proxy.set(false);
        ui.set_proxy_active(false);
        self.select_media(ui, index);
        Ok(())
    }

    fn generate_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let source = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| PathBuf::from(&asset.source_path))
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let default_name = source
            .file_stem()
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}.proxy.mp4"))
            .unwrap_or_else(|| "Prism.proxy.mp4".to_owned());
        let Some(output) = rfd::FileDialog::new()
            .set_title("Generate Prism Proxy")
            .set_file_name(default_name)
            .add_filter("MP4 proxy", &["mp4"])
            .save_file()
        else {
            return Ok(());
        };
        self.start_proxy_generation(ui, media_id, source, output)
    }

    fn regenerate_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let (source, base) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| {
                let source = PathBuf::from(&asset.source_path);
                let base = asset.proxy_path.as_ref().map_or_else(
                    || {
                        let stem = source
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Prism");
                        source.with_file_name(format!("{stem}.proxy.mp4"))
                    },
                    PathBuf::from,
                );
                (source, base)
            })
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let output = next_proxy_version(&base)?;
        self.start_proxy_generation(ui, media_id, source, output)
    }

    fn start_proxy_generation(
        &self,
        ui: &PrismStudio,
        media_id: MediaAssetId,
        source: PathBuf,
        output: PathBuf,
    ) -> Result<(), String> {
        if self.proxy_generation.borrow().is_some() {
            return Err("A proxy is already being generated".to_owned());
        }
        let profile = ProxyProfile::new(960, 540, "libx264", Some("aac".to_owned()), "mp4")
            .map_err(|error| error.to_string())?;
        let request = ProxyRequest {
            source,
            output,
            profile,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = FfmpegProxyGenerator::new("ffmpeg")
                .generate(&request, Some(&worker_cancelled))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        *self.proxy_generation.borrow_mut() = Some(ProxyGeneration {
            receiver,
            cancelled,
            media_id,
        });
        ui.set_proxy_generating(true);
        ui.set_feedback("Generating 540p proxy in the background…".into());
        Ok(())
    }

    fn commit_commands(
        &self,
        ui: &PrismStudio,
        commands: Vec<Command>,
        label: &str,
    ) -> Result<(), String> {
        let mut updated = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let outcome = CommandEngine::execute_transaction(&mut updated, label, commands)
            .map_err(|e| e.to_string())?;
        let path = self
            .project_path
            .borrow()
            .clone()
            .ok_or_else(|| "Save a project first".to_owned())?;
        // Publish to the UI only after the validated edit has been saved successfully.
        prism_persistence::save_atomic(&updated, &path).map_err(|e| e.to_string())?;
        apply_project_summary(ui, &updated, &path);
        *self.project.borrow_mut() = Some(updated);
        if let Some(project) = self.project.borrow().as_ref() {
            self.queue_autosave(ui, project);
        }
        let mut undo = self.undo.borrow_mut();
        undo.push(outcome.undo);
        if undo.len() > 50 {
            undo.remove(0);
        }
        drop(undo);
        self.redo.borrow_mut().clear();
        self.update_history(ui);
        self.update_inspector(ui);
        self.update_ruler(ui);
        ui.set_feedback(format!("{label} · saved").into());
        self.seek_time(ui, self.playhead.get());
        Ok(())
    }

    fn edit_clip(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        let index = usize::try_from(ui.get_selected_clip_index())
            .map_err(|_| "Select a clip first".to_owned())?;
        let operation = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip) = sequence
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                .nth(index)
                .ok_or_else(|| "Selected clip is no longer available".to_owned())?;
            match command {
                "timeline.split" => Command::SplitClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    right_clip_id: ClipId::new(),
                    timeline_position: self.playhead.get(),
                },
                "timeline.ripple_delete" => Command::RippleDelete {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                },
                _ => Command::DeleteClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                },
            }
        };
        // End the immutable project borrow before commit replaces the snapshot.
        let label = match command {
            "timeline.split" => "Split clip",
            "timeline.ripple_delete" => "Ripple delete clip",
            _ => "Delete clip",
        };
        self.commit(ui, operation, label)?;
        ui.set_selected_clip_index(-1);
        self.update_inspector(ui);
        Ok(())
    }

    fn add_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let (sequence_id, marker_number) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .map(|sequence| (sequence.id, sequence.markers.len() + 1))
            .ok_or_else(|| "No sequence available".to_owned())?;
        self.commit(
            ui,
            Command::AddMarker {
                sequence_id,
                id: MarkerId::new(),
                time: self.playhead.get(),
                label: format!("Marker {marker_number}"),
            },
            "Add timeline marker",
        )
    }

    fn delete_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let index = usize::try_from(ui.get_selected_marker_index())
            .map_err(|_| "Select a marker first".to_owned())?;
        let (sequence_id, marker_id) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .and_then(|sequence| {
                sequence
                    .markers
                    .get(index)
                    .map(|marker| (sequence.id, marker.id))
            })
            .ok_or_else(|| "Selected marker is unavailable".to_owned())?;
        self.commit(
            ui,
            Command::DeleteMarker {
                sequence_id,
                marker_id,
            },
            "Delete timeline marker",
        )?;
        ui.set_selected_marker_index(-1);
        Ok(())
    }

    fn toggle_dissolve(&self, ui: &PrismStudio) -> Result<(), String> {
        let selected = usize::try_from(ui.get_selected_clip_index())
            .map_err(|_| "Select a video clip first".to_owned())?;
        let operation = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip_index) = sequence
                .tracks
                .iter()
                .flat_map(|track| (0..track.clips.len()).map(move |index| (track, index)))
                .nth(selected)
                .ok_or_else(|| "Selected clip is unavailable".to_owned())?;
            if track.kind != TrackKind::Video {
                return Err("Dissolves require a video track".to_owned());
            }
            let outgoing = &track.clips[clip_index];
            let incoming = track
                .clips
                .get(clip_index + 1)
                .ok_or_else(|| "Select a clip with an adjacent video clip after it".to_owned())?;
            if let Some(transition) = sequence.transitions.iter().find(|transition| {
                transition.outgoing_clip == outgoing.id && transition.incoming_clip == incoming.id
            }) {
                Command::DeleteTransition {
                    sequence_id: sequence.id,
                    transition_id: transition.id,
                }
            } else {
                let duration = RationalTime::new(15, 30)
                    .map_err(|error| error.to_string())?
                    .min(outgoing.source_duration)
                    .min(incoming.source_duration);
                Command::AddTransition {
                    sequence_id: sequence.id,
                    id: TransitionId::new(),
                    outgoing_clip: outgoing.id,
                    incoming_clip: incoming.id,
                    duration,
                    kind: TransitionKind::Dissolve,
                }
            }
        };
        let label = if ui.get_selected_has_transition() {
            "Remove dissolve"
        } else {
            "Add dissolve"
        };
        self.commit(ui, operation, label)
    }

    fn duplicate_clip(&self, ui: &PrismStudio) -> Result<(), String> {
        let selected = usize::try_from(ui.get_selected_clip_index())
            .map_err(|_| "Select a clip first".to_owned())?;
        let operation = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip) = sequence
                .tracks
                .iter()
                .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
                .nth(selected)
                .ok_or_else(|| "Selected clip is unavailable".to_owned())?;
            let timeline_start = track
                .clips
                .iter()
                .filter_map(|clip| clip.timeline_start.checked_add(clip.source_duration).ok())
                .max()
                .unwrap_or(RationalTime::ZERO);
            Command::DuplicateClip {
                sequence_id: sequence.id,
                track_id: track.id,
                clip_id: clip.id,
                new_clip_id: ClipId::new(),
                timeline_start,
            }
        };
        self.commit(ui, operation, "Duplicate clip to track end")
    }

    fn change_track(&self, ui: &PrismStudio, index: i32, action: &str) -> Result<(), String> {
        let (operation, selected_clip_id) = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|p| p.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let track = sequence
                .tracks
                .get(usize::try_from(index).map_err(|e| e.to_string())?)
                .ok_or_else(|| "Track unavailable".to_owned())?;
            let selected_clip_id = sequence
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .nth(usize::try_from(ui.get_selected_clip_index()).unwrap_or(usize::MAX))
                .map(|clip| clip.id);
            let operation = match action {
                "lock" => Command::SetTrackLocked {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    locked: !track.locked,
                },
                "mute" => Command::SetTrackMuted {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    muted: !track.muted,
                },
                "visible" => Command::SetTrackVisible {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    visible: !track.visible,
                },
                "up" | "down" => {
                    let current = usize::try_from(index).map_err(|e| e.to_string())?;
                    let destination_index = if action == "up" {
                        current
                            .checked_sub(1)
                            .ok_or_else(|| "Track is already first".to_owned())?
                    } else {
                        let next = current + 1;
                        if next >= sequence.tracks.len() {
                            return Err("Track is already last".to_owned());
                        }
                        next
                    };
                    Command::ReorderTrack {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        destination_index,
                    }
                }
                _ => return Err("Unknown track action".to_owned()),
            };
            (operation, selected_clip_id)
        };
        self.commit(ui, operation, "Track updated")?;
        if let Some(selected_clip_id) = selected_clip_id {
            let new_index = self
                .project
                .borrow()
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .position(|clip| clip.id == selected_clip_id)
                })
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1);
            ui.set_selected_clip_index(new_index);
            self.update_inspector(ui);
        }
        Ok(())
    }

    fn reset_thumbnails(&self, ui: &PrismStudio) {
        self.thumbnail_preview.invalidate();
        self.thumbnail_current.set(None);
        let project = self.project.borrow();
        let media = project
            .as_ref()
            .map(|p| p.media.values().enumerate().collect::<Vec<_>>())
            .unwrap_or_default();
        *self.cards.borrow_mut() = media
            .iter()
            .map(|(index, asset)| MediaCard {
                index: i32::try_from(*index).unwrap_or(i32::MAX),
                name: Path::new(&asset.source_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&asset.source_path)
                    .into(),
                frame: slint::Image::default(),
                has_frame: false,
                status: "Preparing preview…".into(),
            })
            .collect();
        *self.thumbnail_queue.borrow_mut() = media
            .iter()
            .map(|(i, a)| (*i, PathBuf::from(&a.source_path)))
            .collect();
        self.thumbnail_total.set(media.len());
        self.thumbnail_completed.set(0);
        self.thumbnail_failed.set(0);
        self.filter_cards(ui);
        self.next_thumbnail(ui);
    }

    fn filter_cards(&self, ui: &PrismStudio) {
        let query = ui.get_search().to_lowercase();
        let cards = self
            .cards
            .borrow()
            .iter()
            .filter(|item| item.name.to_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();
        ui.set_media_cards(slint::ModelRc::new(slint::VecModel::from(cards)));
    }

    fn update_media_task_state(&self, ui: &PrismStudio) {
        let total = self.thumbnail_total.get();
        let completed = self.thumbnail_completed.get();
        let failed = self.thumbnail_failed.get();
        let pending = total.saturating_sub(completed);
        ui.set_media_task_pending(i32::try_from(pending).unwrap_or(i32::MAX));
        ui.set_media_task_status(
            if total == 0 {
                "IDLE"
            } else if pending > 0 {
                "DECODING"
            } else if failed > 0 {
                "COMPLETE WITH ERRORS"
            } else {
                "COMPLETE"
            }
            .into(),
        );
        ui.set_media_task_detail(
            format!(
                "{completed}/{total} previews{}",
                if failed > 0 {
                    format!(" · {failed} unavailable")
                } else {
                    String::new()
                }
            )
            .into(),
        );
    }

    fn next_thumbnail(&self, ui: &PrismStudio) {
        if let Some((index, path)) = self.thumbnail_queue.borrow_mut().pop_front() {
            self.thumbnail_current.set(Some(index));
            self.thumbnail_preview.request(path, RationalTime::ZERO);
        } else {
            self.thumbnail_current.set(None);
        }
        self.update_media_task_state(ui);
    }

    fn update_history(&self, ui: &PrismStudio) {
        let undo = self.undo.borrow();
        let redo = self.redo.borrow();
        ui.set_can_undo(!undo.is_empty());
        ui.set_can_redo(!redo.is_empty());
        let items = undo
            .iter()
            .map(|record| HistoryItem {
                label: record.label().into(),
                applied: true,
            })
            .chain(redo.iter().rev().map(|record| HistoryItem {
                label: record.label().into(),
                applied: false,
            }))
            .collect::<Vec<_>>();
        ui.set_history_items(slint::ModelRc::new(slint::VecModel::from(items)));
    }

    fn apply_history(&self, ui: &PrismStudio, redo_action: bool) -> Result<(), String> {
        let mut updated = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let path = self
            .project_path
            .borrow()
            .clone()
            .ok_or_else(|| "Project destination unavailable".to_owned())?;
        let (from, to) = if redo_action {
            (&self.redo, &self.undo)
        } else {
            (&self.undo, &self.redo)
        };
        let record = from
            .borrow_mut()
            .pop()
            .ok_or_else(|| "No edit available".to_owned())?;
        let label = record.label().to_owned();
        let inverse = record.apply(&mut updated).map_err(|e| e.to_string())?;
        if let Err(error) = prism_persistence::save_atomic(&updated, &path) {
            // Restore the history entry if disk persistence failed.
            let restored = inverse.apply(&mut updated).map_err(|e| e.to_string())?;
            from.borrow_mut().push(restored);
            return Err(error.to_string());
        }
        to.borrow_mut().push(inverse);
        apply_project_summary(ui, &updated, &path);
        *self.project.borrow_mut() = Some(updated);
        if let Some(project) = self.project.borrow().as_ref() {
            self.queue_autosave(ui, project);
        }
        ui.set_selected_clip_index(-1);
        self.update_history(ui);
        self.update_inspector(ui);
        self.update_ruler(ui);
        self.seek_time(ui, self.playhead.get());
        if label == "Relink source media" {
            let media_index = ui.get_selected_media_index();
            self.reset_thumbnails(ui);
            self.select_media(ui, media_index);
            self.update_inspector(ui);
        }
        ui.set_feedback(
            format!(
                "{} {label} · saved",
                if redo_action { "Redid" } else { "Undid" }
            )
            .into(),
        );
        Ok(())
    }

    fn update_inspector(&self, ui: &PrismStudio) {
        let project = self.project.borrow();
        let selected = project.as_ref().and_then(|p| {
            let sequence = p.sequences.values().next()?;
            let (row, track, clip) = sequence
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(row, track)| track.clips.iter().map(move |clip| (row, track, clip)))
                .nth(usize::try_from(ui.get_selected_clip_index()).ok()?)?;
            Some((p, sequence, row, track, clip))
        });
        if let Some((project, sequence, row, track, clip)) = selected {
            let path = project
                .media
                .get(&clip.media_id)
                .map(|m| m.source_path.as_str())
                .unwrap_or("Offline media");
            ui.set_inspector_name(
                Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path)
                    .into(),
            );
            ui.set_inspector_path(path.into());
            ui.set_inspector_track(
                format!(
                    "{}{}",
                    if track.kind == TrackKind::Audio {
                        "A"
                    } else {
                        "V"
                    },
                    row + 1
                )
                .into(),
            );
            ui.set_inspector_locked(track.locked);
            ui.set_selected_track_index(i32::try_from(row).unwrap_or(-1));
            ui.set_clip_start(timing_text(clip.timeline_start));
            ui.set_clip_source_in(timing_text(clip.source_start));
            ui.set_clip_duration(timing_text(clip.source_duration));
            let next = track
                .clips
                .iter()
                .position(|item| item.id == clip.id)
                .and_then(|index| track.clips.get(index + 1));
            ui.set_selected_has_next(track.kind == TrackKind::Video && next.is_some());
            ui.set_selected_has_transition(next.is_some_and(|next| {
                sequence.transitions.iter().any(|transition| {
                    transition.outgoing_clip == clip.id && transition.incoming_clip == next.id
                })
            }));
        } else {
            ui.set_inspector_name("".into());
            ui.set_inspector_path("".into());
            ui.set_inspector_locked(false);
            ui.set_selected_track_index(-1);
            ui.set_selected_has_next(false);
            ui.set_selected_has_transition(false);
        }
    }

    fn apply_timing(&self, ui: &PrismStudio) -> Result<(), String> {
        let start = parse_timing(ui.get_clip_start().as_str())?;
        let source_start = parse_timing(ui.get_clip_source_in().as_str())?;
        let duration = parse_timing(ui.get_clip_duration().as_str())?;
        if !duration.is_positive() {
            return Err("Duration must be greater than zero".to_owned());
        }
        let command = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "No project available".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip) = sequence
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                .nth(
                    usize::try_from(ui.get_selected_clip_index())
                        .map_err(|_| "Select a clip first".to_owned())?,
                )
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let asset = project
                .media
                .get(&clip.media_id)
                .ok_or_else(|| "Media unavailable".to_owned())?;
            let metadata = FfprobeInspector::new("ffprobe")
                .inspect(Path::new(&asset.source_path))
                .map_err(|e| e.to_string())?;
            let available = metadata
                .duration
                .ok_or_else(|| "Source duration unavailable".to_owned())?;
            if source_start
                .checked_add(duration)
                .map_err(|e| e.to_string())?
                > available
            {
                return Err("The requested trim exceeds the source duration".to_owned());
            }
            Command::TrimClip {
                sequence_id: sequence.id,
                track_id: track.id,
                clip_id: clip.id,
                timeline_start: start,
                source_start,
                source_duration: duration,
            }
        };
        self.commit(ui, command, "Change clip timing")
    }

    fn adjust_clip_frame(&self, ui: &PrismStudio, operation: &str) -> Result<(), String> {
        let frame = RationalTime::new(1, 30).map_err(|error| error.to_string())?;
        if matches!(operation, "timeline.move_left" | "timeline.move_right") {
            let current = parse_timing(ui.get_clip_start().as_str())?;
            let timeline_start = if operation == "timeline.move_left" {
                current
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?
            } else {
                current
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?
            };
            if timeline_start.is_negative() {
                return Err("A clip cannot move before sequence start".to_owned());
            }
            let command = {
                let project = self.project.borrow();
                let sequence = project
                    .as_ref()
                    .and_then(|project| project.sequences.values().next())
                    .ok_or_else(|| "No sequence available".to_owned())?;
                let (track, clip) = sequence
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
                    .nth(
                        usize::try_from(ui.get_selected_clip_index())
                            .map_err(|_| "Select a clip first".to_owned())?,
                    )
                    .ok_or_else(|| "Selected clip unavailable".to_owned())?;
                Command::MoveClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    timeline_start,
                }
            };
            return self.commit(ui, command, "Move clip one frame");
        }

        let original_start = ui.get_clip_start();
        let original_source = ui.get_clip_source_in();
        let original_duration = ui.get_clip_duration();
        let mut start = parse_timing(original_start.as_str())?;
        let mut source = parse_timing(original_source.as_str())?;
        let mut duration = parse_timing(original_duration.as_str())?;
        match operation {
            "timeline.trim_in_left" => {
                start = start
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                if start.is_negative() || source.is_negative() {
                    return Err("No earlier source frame is available".to_owned());
                }
            }
            "timeline.trim_in_right" => {
                start = start
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
            }
            "timeline.trim_out_left" => {
                duration = duration
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
            }
            "timeline.trim_out_right" => {
                duration = duration
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("Unknown clip adjustment".to_owned()),
        }
        if !duration.is_positive() {
            return Err("A clip must remain at least one frame long".to_owned());
        }
        ui.set_clip_start(timing_text(start));
        ui.set_clip_source_in(timing_text(source));
        ui.set_clip_duration(timing_text(duration));
        if let Err(error) = self.apply_timing(ui) {
            ui.set_clip_start(original_start);
            ui.set_clip_source_in(original_source);
            ui.set_clip_duration(original_duration);
            return Err(error);
        }
        Ok(())
    }

    fn precision_edit_frame(&self, ui: &PrismStudio, operation: &str) -> Result<(), String> {
        let frame = RationalTime::new(1, 30).map_err(|error| error.to_string())?;
        let delta = if operation.ends_with("_left") {
            RationalTime::ZERO
                .checked_sub(frame)
                .map_err(|error| error.to_string())?
        } else {
            frame
        };
        let kind = if operation.contains("slip") {
            "slip"
        } else if operation.contains("roll_in") {
            "roll_in"
        } else if operation.contains("roll_out") {
            "roll_out"
        } else {
            "slide"
        };
        self.precision_edit_delta(ui, kind, delta)
    }

    fn precision_edit_delta(
        &self,
        ui: &PrismStudio,
        operation: &str,
        delta: RationalTime,
    ) -> Result<(), String> {
        let command = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let selected = usize::try_from(ui.get_selected_clip_index())
                .map_err(|_| "Select a clip first".to_owned())?;
            let (track, clip_index) = sequence
                .tracks
                .iter()
                .flat_map(|track| (0..track.clips.len()).map(move |index| (track, index)))
                .nth(selected)
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let clip = &track.clips[clip_index];
            match operation {
                "slip" => Command::SlipClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    source_delta: delta,
                },
                "roll_in" => {
                    let previous = clip_index
                        .checked_sub(1)
                        .and_then(|index| track.clips.get(index))
                        .ok_or_else(|| {
                            "Roll In requires an adjacent clip before this clip".to_owned()
                        })?;
                    Command::RollEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        left_clip_id: previous.id,
                        right_clip_id: clip.id,
                        delta,
                    }
                }
                "roll_out" => {
                    let next = track.clips.get(clip_index + 1).ok_or_else(|| {
                        "Roll Out requires an adjacent clip after this clip".to_owned()
                    })?;
                    Command::RollEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        left_clip_id: clip.id,
                        right_clip_id: next.id,
                        delta,
                    }
                }
                "slide" => {
                    let previous = clip_index
                        .checked_sub(1)
                        .and_then(|index| track.clips.get(index))
                        .ok_or_else(|| "Slide requires clips on both sides".to_owned())?;
                    let next = track
                        .clips
                        .get(clip_index + 1)
                        .ok_or_else(|| "Slide requires clips on both sides".to_owned())?;
                    Command::SlideEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        previous_clip_id: previous.id,
                        clip_id: clip.id,
                        next_clip_id: next.id,
                        delta,
                    }
                }
                _ => return Err("Unknown precision edit".to_owned()),
            }
        };
        let label = if operation == "slip" {
            "Slip clip"
        } else if operation.starts_with("roll") {
            "Roll edit"
        } else {
            "Slide clip"
        };
        self.commit(ui, command, label)
    }

    fn drag_clip(
        &self,
        ui: &PrismStudio,
        index: i32,
        delta_ratio: f32,
        edge: &str,
    ) -> Result<(), String> {
        if !delta_ratio.is_finite() {
            return Err("Invalid timeline drag".to_owned());
        }
        ui.set_selected_clip_index(index);
        self.update_inspector(ui);
        let sequence_duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next()?.duration().ok())
            .ok_or_else(|| "No sequence available".to_owned())?;
        let timescale = if ui.get_snap_enabled() { 30 } else { 300 };
        #[allow(clippy::cast_possible_truncation)]
        let ticks =
            (sequence_duration.as_seconds_f64() * f64::from(delta_ratio) * f64::from(timescale))
                .round() as i64;
        if ticks == 0 {
            return Ok(());
        }
        let delta = RationalTime::new(ticks, timescale).map_err(|error| error.to_string())?;
        if matches!(edge, "slip" | "slide" | "roll_in" | "roll_out") {
            return self.precision_edit_delta(ui, edge, delta);
        }
        if edge == "move" {
            let current = parse_timing(ui.get_clip_start().as_str())?;
            let target = current
                .checked_add(delta)
                .map_err(|error| error.to_string())?;
            if target.is_negative() {
                return Err("A clip cannot move before sequence start".to_owned());
            }
            let command = {
                let project = self.project.borrow();
                let sequence = project
                    .as_ref()
                    .and_then(|project| project.sequences.values().next())
                    .ok_or_else(|| "No sequence available".to_owned())?;
                let (track, clip) = sequence
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
                    .nth(usize::try_from(index).map_err(|_| "Select a clip first".to_owned())?)
                    .ok_or_else(|| "Selected clip unavailable".to_owned())?;
                Command::MoveClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    timeline_start: target,
                }
            };
            return self.commit(ui, command, "Drag clip");
        }

        let original_start = ui.get_clip_start();
        let original_source = ui.get_clip_source_in();
        let original_duration = ui.get_clip_duration();
        let mut start = parse_timing(original_start.as_str())?;
        let mut source = parse_timing(original_source.as_str())?;
        let mut duration = parse_timing(original_duration.as_str())?;
        match edge {
            "in" => {
                start = start
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_sub(delta)
                    .map_err(|error| error.to_string())?;
            }
            "out" => {
                duration = duration
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("Unknown timeline drag edge".to_owned()),
        }
        if start.is_negative() || source.is_negative() || !duration.is_positive() {
            return Err("The drag would create an invalid clip range".to_owned());
        }
        ui.set_clip_start(timing_text(start));
        ui.set_clip_source_in(timing_text(source));
        ui.set_clip_duration(timing_text(duration));
        if let Err(error) = self.apply_timing(ui) {
            ui.set_clip_start(original_start);
            ui.set_clip_source_in(original_source);
            ui.set_clip_duration(original_duration);
            return Err(error);
        }
        Ok(())
    }

    fn update_ruler(&self, ui: &PrismStudio) {
        let duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next()?.duration().ok())
            .unwrap_or(RationalTime::ZERO);
        let zoom = f64::from(ui.get_timeline_zoom()).clamp(1.0, 8.0);
        let offset = f64::from(ui.get_timeline_offset()).clamp(0.0, 1.0 - 1.0 / zoom);
        let labels = (0..6)
            .map(|index| {
                #[allow(clippy::cast_possible_truncation)]
                let frame =
                    (duration.as_seconds_f64() * (offset + f64::from(index) / (5.0 * zoom)) * 30.0)
                        .floor() as i64;
                RationalTime::new(frame, 30)
                    .map(display_timecode)
                    .unwrap_or_default()
                    .into()
            })
            .collect::<Vec<slint::SharedString>>();
        ui.set_ruler_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
    }

    fn update_source_labels(&self, ui: &PrismStudio) {
        ui.set_source_timecode(display_timecode(self.source_position.get()).into());
        ui.set_source_duration_label(display_timecode(self.source_duration.get()).into());
        ui.set_source_in_label(display_timecode(self.source_in.get()).into());
        ui.set_source_out_label(display_timecode(self.source_out.get()).into());
        #[allow(clippy::cast_possible_truncation)]
        let ratio = if self.source_duration.get().is_positive() {
            (self.source_position.get().as_seconds_f64()
                / self.source_duration.get().as_seconds_f64()) as f32
        } else {
            0.0
        };
        ui.set_source_ratio(ratio);
    }

    fn seek_source(&self, ui: &PrismStudio, ratio: f32) {
        if !ratio.is_finite() || !self.source_duration.get().is_positive() {
            return;
        }
        let asset = self.project.borrow().as_ref().and_then(|p| {
            p.media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).ok()?)
                .cloned()
        });
        if let Some(asset) = asset {
            #[allow(clippy::cast_possible_truncation)]
            let frame = (self.source_duration.get().as_seconds_f64()
                * f64::from(ratio.clamp(0.0, 1.0))
                * 30.0)
                .floor() as i64;
            if let Ok(time) = RationalTime::new(frame, 30) {
                let last_frame = self
                    .source_duration
                    .get()
                    .checked_sub(RationalTime::new(1, 30).unwrap_or(RationalTime::ZERO))
                    .unwrap_or(RationalTime::ZERO)
                    .max(RationalTime::ZERO);
                let time = time.min(last_frame);
                self.source_position.set(time);
                self.update_source_labels(ui);
                self.source_preview
                    .request(PathBuf::from(&asset.source_path), time);
            }
        }
    }

    fn append_source(&self, ui: &PrismStudio) -> Result<(), String> {
        if !ui.get_source_ready() {
            return Err("Select an available source clip first".to_owned());
        }
        let duration = self
            .source_out
            .get()
            .checked_sub(self.source_in.get())
            .map_err(|e| e.to_string())?;
        if !duration.is_positive() {
            return Err("Mark a positive source range".to_owned());
        }
        let commands = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let media = project
                .media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).map_err(|e| e.to_string())?)
                .ok_or_else(|| "Select source footage".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "Import footage to create a sequence first".to_owned())?;
            let mut commands = Vec::new();
            let track_id = if let Some(track) = sequence
                .tracks
                .iter()
                .find(|t| t.kind == TrackKind::Video && !t.locked)
            {
                track.id
            } else {
                let id = TrackId::new();
                commands.push(Command::CreateTrack {
                    sequence_id: sequence.id,
                    id,
                    kind: TrackKind::Video,
                });
                id
            };
            let timeline_start = sequence
                .track_duration(track_id)
                .ok()
                .flatten()
                .unwrap_or(RationalTime::ZERO);
            commands.push(Command::InsertClip {
                sequence_id: sequence.id,
                track_id,
                id: ClipId::new(),
                media_id: media.id,
                timeline_start,
                source_start: self.source_in.get(),
                source_duration: duration,
            });
            commands
        };
        self.commit_commands(ui, commands, "Append source range")
    }

    fn poll(&self, ui: &PrismStudio) {
        let cache_result = self
            .cache_eviction
            .borrow()
            .as_ref()
            .and_then(|worker| worker.try_result().ok());
        if let Some(result) = cache_result {
            self.cache_eviction.borrow_mut().take();
            match result.result {
                Ok(report) => ui.set_feedback(
                    format!(
                        "Cache cleanup removed {} artifacts ({})",
                        report.removed_files,
                        format_bytes(report.removed_bytes)
                    )
                    .into(),
                ),
                Err(error) => ui.set_feedback(format!("Cache cleanup failed: {error}").into()),
            }
            self.update_cache_state(ui);
        }
        loop {
            let result = self
                .autosave_scheduler
                .borrow()
                .as_ref()
                .and_then(prism_persistence::AutosaveScheduler::try_result);
            let Some(result) = result else {
                break;
            };
            match result.result {
                Ok(()) => {
                    ui.set_autosave_status(format!("SAVED R{}", result.revision).into());
                    if let Some(path) = self.project_path.borrow().as_deref() {
                        if let Err(error) = prism_persistence::retain_autosaves(path, 5) {
                            ui.set_feedback(format!("Autosave retention failed: {error}").into());
                        }
                    }
                    self.refresh_recovery(ui);
                }
                Err(error) => {
                    ui.set_autosave_status("FAILED".into());
                    ui.set_feedback(format!("Autosave failed: {error}").into());
                }
            }
        }
        self.retry_pending_autosave(ui);
        let proxy_result = self
            .proxy_generation
            .borrow()
            .as_ref()
            .and_then(|generation| {
                generation
                    .receiver
                    .try_recv()
                    .ok()
                    .map(|result| (generation.media_id, result))
            });
        if let Some((media_id, result)) = proxy_result {
            self.proxy_generation.borrow_mut().take();
            ui.set_proxy_generating(false);
            match result {
                Ok(path) => {
                    let command = Command::SetMediaProxy {
                        media_id,
                        proxy_path: Some(path.display().to_string()),
                    };
                    if let Err(error) = self.commit(ui, command, "Generate media proxy") {
                        ui.set_feedback(error.into());
                    } else {
                        self.use_proxy.set(true);
                        ui.set_proxy_active(true);
                        let index = self
                            .project
                            .borrow()
                            .as_ref()
                            .and_then(|project| project.media.keys().position(|id| *id == media_id))
                            .and_then(|index| i32::try_from(index).ok())
                            .unwrap_or(-1);
                        self.select_media(ui, index);
                    }
                }
                Err(error) => ui.set_feedback(error.into()),
            }
        }
        if self.playing.get() {
            if let Some(started) = self.playback_started.get() {
                let elapsed = started.elapsed();
                let milliseconds = i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX);
                if let Ok(delta) = RationalTime::new(milliseconds, 1_000) {
                    if let Ok(target) = self.playback_origin.get().checked_add(delta) {
                        self.seek_time(ui, target);
                        if ui.get_playhead_ratio() >= 1.0 {
                            self.stop_playback(ui);
                        }
                    }
                }
            }
        }
        if let Some(result) = self.thumbnail_preview.poll() {
            if let Some(index) = self.thumbnail_current.take() {
                if let Some(card) = self.cards.borrow_mut().get_mut(index) {
                    if let Ok((frame, _)) = result {
                        card.frame = frame;
                        card.has_frame = true;
                        card.status = "".into();
                    } else {
                        card.status = "Preview unavailable".into();
                        self.thumbnail_failed
                            .set(self.thumbnail_failed.get().saturating_add(1));
                    }
                }
                self.thumbnail_completed
                    .set(self.thumbnail_completed.get().saturating_add(1));
                self.filter_cards(ui);
                self.next_thumbnail(ui);
            }
        }
        if let Some(frame) = self.program_preview.poll() {
            match frame {
                Ok((frame, _)) => {
                    ui.set_program_frame(frame);
                    ui.set_has_program_frame(true);
                }
                Err(error) => {
                    ui.set_has_program_frame(false);
                    ui.set_preview_status(
                        "Preview unavailable · check FFmpeg and source media".into(),
                    );
                    ui.set_feedback(error.into());
                }
            }
        }
        if let Some(frame) = self.source_preview.poll() {
            match frame {
                Ok((frame, duration)) => {
                    ui.set_source_frame(frame);
                    ui.set_has_source_frame(true);
                    if self.source_duration.get() != duration {
                        self.source_duration.set(duration);
                        self.source_out.set(duration);
                    }
                    ui.set_source_ready(duration.is_positive());
                    self.update_source_labels(ui);
                }
                Err(error) => {
                    ui.set_has_source_frame(false);
                    ui.set_source_ready(false);
                    ui.set_source_detail(
                        "Preview unavailable · check FFmpeg and source media".into(),
                    );
                    ui.set_feedback(error.into());
                }
            }
        }
        let event = self
            .worker
            .borrow()
            .as_ref()
            .and_then(|worker| worker.try_next().ok().flatten());
        let Some(event) = event else {
            return;
        };
        let terminal = matches!(
            event,
            ExportWorkerEvent::Completed
                | ExportWorkerEvent::Cancelled
                | ExportWorkerEvent::Failed(_)
        );
        let mut controller = self.controller.borrow_mut();
        let already_cancelled = matches!(
            (controller.state(), &event),
            (
                Some(prism_export::ExportJobState::Cancelled),
                ExportWorkerEvent::Cancelled
            )
        );
        if !already_cancelled {
            let _ = controller.apply_worker_event(event);
        }
        if terminal {
            *self.worker.borrow_mut() = None;
        }
        apply_export_status(ui, controller.status().as_ref());
    }
}

fn import_video_into_project(project: &mut Project, path: &Path) -> Result<(), String> {
    let metadata = FfprobeInspector::new("ffprobe")
        .inspect(path)
        .map_err(|error| error.to_string())?;
    if !metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
    {
        return Err(format!("{} has no video stream", path.display()));
    }
    let duration = metadata
        .duration
        .filter(|duration| duration.is_positive())
        .ok_or_else(|| format!("{} has no valid duration", path.display()))?;
    let has_audio = metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Audio(_)));
    insert_media_into_project(project, path, duration, has_audio)
}

#[cfg(test)]
fn insert_video_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
) -> Result<(), String> {
    insert_media_into_project(project, path, duration, false)
}

fn insert_media_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
    has_audio: bool,
) -> Result<(), String> {
    let media_id = MediaAssetId::new();
    let (sequence_id, track_id, timeline_start, mut commands) =
        if let Some((sequence_id, sequence)) = project.sequences.iter().next() {
            let sequence_id = *sequence_id;
            if let Some(track) = sequence
                .tracks
                .iter()
                .find(|track| track.kind == TrackKind::Video && !track.locked)
            {
                let timeline_start = sequence
                    .track_duration(track.id)
                    .map_err(|error| error.to_string())?
                    .unwrap_or(RationalTime::ZERO);
                (sequence_id, track.id, timeline_start, Vec::new())
            } else {
                let track_id = TrackId::new();
                (
                    sequence_id,
                    track_id,
                    RationalTime::ZERO,
                    vec![Command::CreateTrack {
                        sequence_id,
                        id: track_id,
                        kind: TrackKind::Video,
                    }],
                )
            }
        } else {
            let sequence_id = SequenceId::new();
            let track_id = TrackId::new();
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Sequence 01")
                .to_owned();
            (
                sequence_id,
                track_id,
                RationalTime::ZERO,
                vec![
                    Command::CreateSequence {
                        id: sequence_id,
                        name,
                    },
                    Command::CreateTrack {
                        sequence_id,
                        id: track_id,
                        kind: TrackKind::Video,
                    },
                ],
            )
        };
    commands.insert(
        0,
        Command::ImportMedia {
            id: media_id,
            source_path: path.to_string_lossy().into_owned(),
        },
    );
    commands.push(Command::InsertClip {
        sequence_id,
        track_id,
        id: ClipId::new(),
        media_id,
        timeline_start,
        source_start: RationalTime::ZERO,
        source_duration: duration,
    });
    if has_audio {
        let audio_track_id = project
            .sequences
            .get(&sequence_id)
            .and_then(|sequence| {
                sequence
                    .tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Audio && !track.locked)
            })
            .map(|track| track.id)
            .unwrap_or_else(|| {
                let id = TrackId::new();
                commands.push(Command::CreateTrack {
                    sequence_id,
                    id,
                    kind: TrackKind::Audio,
                });
                id
            });
        commands.push(Command::InsertClip {
            sequence_id,
            track_id: audio_track_id,
            id: ClipId::new(),
            media_id,
            timeline_start,
            source_start: RationalTime::ZERO,
            source_duration: duration,
        });
    }
    CommandEngine::execute_transaction(project, "Import media", commands)
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn apply_project_summary(ui: &PrismStudio, project: &Project, project_path: &Path) {
    let summary = prism_studio::project_summary(project);
    ui.set_has_project(true);
    ui.set_project_name(summary.name.into());
    ui.set_project_path(project_path.display().to_string().into());
    ui.set_media_count(i32::try_from(summary.media_count).unwrap_or(i32::MAX));
    ui.set_clip_count(i32::try_from(summary.clip_count).unwrap_or(i32::MAX));
    let media_items = project
        .media
        .values()
        .map(|asset| {
            Path::new(&asset.source_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&asset.source_path)
                .into()
        })
        .collect::<Vec<slint::SharedString>>();
    ui.set_media_items(slint::ModelRc::new(slint::VecModel::from(media_items)));
    let timeline_items = project
        .sequences
        .values()
        .next()
        .map(|sequence| {
            let total_seconds = sequence
                .duration()
                .map(|duration| duration.as_seconds_f64().max(0.001))
                .unwrap_or(1.0);
            sequence
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(row, track)| {
                    track.clips.iter().map(move |clip| {
                        let name = project
                            .media
                            .get(&clip.media_id)
                            .and_then(|asset| Path::new(&asset.source_path).file_name())
                            .and_then(|name| name.to_str())
                            .unwrap_or("Offline Media");
                        TimelineClipItem {
                            name: name.into(),
                            start_ratio: (clip.timeline_start.as_seconds_f64() / total_seconds)
                                as f32,
                            width_ratio: (clip.source_duration.as_seconds_f64() / total_seconds)
                                as f32,
                            row: i32::try_from(row).unwrap_or(3),
                            audio: track.kind == TrackKind::Audio,
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_clips(slint::ModelRc::new(slint::VecModel::from(timeline_items)));
    let sequence = project.sequences.values().next();
    let transition_items = sequence
        .map(|sequence| {
            let total_seconds = sequence
                .duration()
                .map(|duration| duration.as_seconds_f64().max(0.001))
                .unwrap_or(1.0);
            sequence
                .transitions
                .iter()
                .filter_map(|transition| {
                    sequence.tracks.iter().enumerate().find_map(|(row, track)| {
                        track
                            .clips
                            .iter()
                            .find(|clip| clip.id == transition.outgoing_clip)
                            .and_then(|clip| {
                                clip.timeline_start
                                    .checked_add(clip.source_duration)
                                    .ok()
                                    .map(|boundary| TimelineTransitionItem {
                                        ratio: (boundary.as_seconds_f64() / total_seconds) as f32,
                                        width_ratio: (transition.duration.as_seconds_f64()
                                            / total_seconds)
                                            as f32,
                                        row: i32::try_from(row).unwrap_or(3),
                                    })
                            })
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_transitions(slint::ModelRc::new(slint::VecModel::from(transition_items)));
    let tracks = sequence
        .map(|s| {
            s.tracks
                .iter()
                .enumerate()
                .map(|(index, track)| TrackItem {
                    name: format!(
                        "{}{}",
                        if track.kind == TrackKind::Audio {
                            "A"
                        } else {
                            "V"
                        },
                        index + 1
                    )
                    .into(),
                    audio: track.kind == TrackKind::Audio,
                    locked: track.locked,
                    muted: track.muted,
                    visible: track.visible,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_tracks(slint::ModelRc::new(slint::VecModel::from(tracks)));
    ui.set_sequence_name(
        sequence
            .map(|s| s.name.as_str())
            .unwrap_or("Main sequence")
            .into(),
    );
    let duration = sequence
        .and_then(|s| s.duration().ok())
        .unwrap_or(RationalTime::ZERO);
    let marker_items = sequence
        .map(|sequence| {
            sequence
                .markers
                .iter()
                .map(|marker| TimelineMarkerItem {
                    label: marker.label.clone().into(),
                    ratio: if duration.is_positive() {
                        (marker.time.as_seconds_f64() / duration.as_seconds_f64()) as f32
                    } else {
                        0.0
                    },
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_markers(slint::ModelRc::new(slint::VecModel::from(marker_items)));
    ui.set_selected_marker_index(-1);
    ui.set_duration_label(display_timecode(duration).into());
    let labels = (0..6)
        .map(|index| {
            duration
                .checked_mul_i64(index)
                .and_then(|t| t.checked_div_i64(5))
                .map(display_timecode)
                .unwrap_or_else(|_| "--:--:--:--".to_owned())
                .into()
        })
        .collect::<Vec<slint::SharedString>>();
    ui.set_ruler_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
    ui.set_project_summary(
        format!(
            "{} media · {} sequences · {} tracks · {} clips · revision {}",
            summary.media_count,
            summary.sequence_count,
            summary.track_count,
            summary.clip_count,
            summary.revision
        )
        .into(),
    );
}

fn load_project_into_ui(
    ui: &PrismStudio,
    project_path: Option<PathBuf>,
) -> Result<Option<Project>, prism_persistence::PersistenceError> {
    let Some(project_path) = project_path else {
        return Ok(None);
    };
    let project = prism_persistence::load(&project_path)?;
    apply_project_summary(ui, &project, &project_path);
    ui.set_export_output(
        project_path
            .with_extension("export.mp4")
            .display()
            .to_string()
            .into(),
    );
    Ok(Some(project))
}

fn bind_editor_callbacks(ui: &PrismStudio, runtime: &Rc<NativeExportRuntime>) {
    let weak_ui = ui.as_weak();
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_source_seek(move |ratio| {
        if let Some(ui) = weak.upgrade() {
            handler.seek_source(&ui, ratio);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_view_changed(move || {
        if let Some(ui) = weak.upgrade() {
            handler.update_ruler(&ui);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_filter_changed(move || {
        if let Some(ui) = weak.upgrade() {
            handler.filter_cards(&ui);
        }
    });
    ui.on_media_matches(|name, query| name.to_lowercase().contains(&query.to_lowercase()));
    let command_runtime = Rc::clone(runtime);
    ui.on_command_invoked(move |id| {
        if let Some(ui) = weak_ui.upgrade() {
            match command_runtime.handle(id.as_str(), &ui) {
                Err(error) => ui.set_feedback(error.into()),
                Ok(()) if id.starts_with("export.") => {
                    apply_export_status(&ui, command_runtime.controller.borrow().status().as_ref());
                }
                Ok(()) => {}
            }
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_media_selected(move |index| {
        if let Some(ui) = weak.upgrade() {
            handler.select_media(&ui, index);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_clip_selected(move |index| {
        if let Some(ui) = weak.upgrade() {
            handler.select_clip(&ui, index);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_clip_adjusted(move |index, delta, edge| {
        if let Some(ui) = weak.upgrade() {
            if let Err(error) = handler.drag_clip(&ui, index, delta, edge.as_str()) {
                ui.set_feedback(error.into());
            }
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_seek(move |ratio| {
        if let Some(ui) = weak.upgrade() {
            handler.seek_ratio(&ui, ratio);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_track_action(move |index, action| {
        if let Some(ui) = weak.upgrade() {
            if let Err(error) = handler.change_track(&ui, index, action.as_str()) {
                ui.set_feedback(error.into());
            }
        }
    });
}

/// Opens the main editor window and runs the native event loop.
///
/// # Errors
///
/// Returns a platform error if a window cannot be created or the native event
/// loop cannot run.
pub fn launch(project_path: Option<PathBuf>) -> Result<(), NativeUiError> {
    let ui = PrismStudio::new()?;
    let runtime_project_path = project_path.clone();
    let loaded_project = load_project_into_ui(&ui, project_path)?;
    let export_runtime = Rc::new(NativeExportRuntime::new(
        loaded_project,
        runtime_project_path,
    ));
    if export_runtime.project.borrow().is_some() {
        export_runtime.reset_view(&ui);
    }
    bind_editor_callbacks(&ui, &export_runtime);
    let poll_ui = ui.as_weak();
    let poll_runtime = Rc::clone(&export_runtime);
    let export_timer = slint::Timer::default();
    export_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            if let Some(ui) = poll_ui.upgrade() {
                poll_runtime.poll(&ui);
            }
        },
    );
    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Command, NativeExportStatus, PROXY_AVAILABLE, ProxyGeneration, TrackId, TrackKind,
        insert_media_into_project, insert_video_into_project, next_proxy_version,
    };
    use prism_cache::{CacheKey, CacheLayout};
    use prism_project::Project;
    use prism_studio::desktop::ExportController;
    use prism_timeline::RationalTime;
    use std::path::PathBuf;

    #[test]
    fn export_status_maps_to_graphical_task_properties() -> Result<(), Box<dyn std::error::Error>> {
        let mut controller = ExportController::new();
        controller.queue(PathBuf::from("deliver.mp4"));
        controller.start()?;
        controller.report_progress(42)?;
        let running = controller
            .status()
            .ok_or_else(|| std::io::Error::other("missing export status"))?;
        let running = NativeExportStatus::from(&running);
        assert_eq!(running.label, "EXPORTING");
        assert_eq!(running.output, "deliver.mp4");
        assert_eq!(running.progress, 42);
        assert!(running.can_cancel);
        assert!(!running.can_retry);

        controller.fail("encoder unavailable")?;
        let failed = controller
            .status()
            .ok_or_else(|| std::io::Error::other("missing export status"))?;
        let failed = NativeExportStatus::from(&failed);
        assert_eq!(failed.label, "FAILED");
        assert_eq!(failed.error, "encoder unavailable");
        assert!(failed.can_retry);
        assert!(failed.can_remove);
        Ok(())
    }

    #[test]
    fn imported_videos_create_and_extend_a_timeline() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Edit");
        insert_video_into_project(
            &mut project,
            std::path::Path::new("first.mp4"),
            RationalTime::new(5, 1)?,
        )?;
        insert_video_into_project(
            &mut project,
            std::path::Path::new("second.mp4"),
            RationalTime::new(3, 1)?,
        )?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or_else(|| std::io::Error::other("missing sequence"))?;
        let track = sequence
            .tracks
            .first()
            .ok_or_else(|| std::io::Error::other("missing track"))?;
        assert_eq!(project.media.len(), 2);
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[1].timeline_start, RationalTime::new(5, 1)?);
        Ok(())
    }

    #[test]
    fn imported_av_media_creates_synchronized_video_and_audio_tracks()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Edit");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("interview.mp4"),
            RationalTime::new(5, 1)?,
            true,
        )?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        let video = sequence
            .tracks
            .iter()
            .find(|track| track.kind == prism_project::TrackKind::Video)
            .ok_or("missing video track")?;
        let audio = sequence
            .tracks
            .iter()
            .find(|track| track.kind == prism_project::TrackKind::Audio)
            .ok_or("missing audio track")?;
        assert_eq!(video.clips.len(), 1);
        assert_eq!(audio.clips.len(), 1);
        assert_eq!(video.clips[0].timeline_start, audio.clips[0].timeline_start);
        assert_eq!(
            video.clips[0].source_duration,
            audio.clips[0].source_duration
        );
        Ok(())
    }
    #[test]
    fn native_workspace_renders_and_edits_are_persisted() -> Result<(), Box<dyn std::error::Error>>
    {
        use super::{
            NativeExportRuntime, PrismStudio, apply_project_summary, bind_editor_callbacks,
            display_timecode,
        };
        use slint::platform::{Platform, WindowAdapter, software_renderer::MinimalSoftwareWindow};
        use slint::{ComponentHandle, Model};
        use std::rc::Rc;

        struct TestPlatform(Rc<MinimalSoftwareWindow>);
        impl Platform for TestPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(Default::default());
        slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
        let ui = PrismStudio::new()?;
        ui.on_media_matches(|name, query| name.to_lowercase().contains(&query.to_lowercase()));
        window.set_size(slint::PhysicalSize::new(1440, 900));
        ui.show()?;
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media/av-ntsc-48k.mp4");
        let mut project = Project::new("A different perspective");
        insert_video_into_project(&mut project, &fixture, RationalTime::new(2, 1)?)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "prism-ui-test-{}-{nonce}.prism",
            std::process::id()
        ));
        prism_persistence::save_atomic(&project, &path)?;
        let autosave_path = prism_persistence::save_autosave(&project, &path)?;
        let autosave_revision = project.revision();
        apply_project_summary(&ui, &project, &path);
        let runtime = Rc::new(NativeExportRuntime::new(Some(project), Some(path.clone())));
        bind_editor_callbacks(&ui, &runtime);
        runtime.reset_view(&ui);
        assert_eq!(ui.get_media_task_status(), "DECODING");
        assert_eq!(ui.get_media_task_pending(), 1);
        assert_eq!(ui.get_recovery_count(), 1);
        assert_eq!(ui.get_recovery_path(), autosave_path.display().to_string());
        assert_eq!(ui.get_recovery_revision(), autosave_revision.to_string());
        assert!(ui.get_snap_enabled());
        runtime.handle("timeline.toggle_snap", &ui)?;
        assert!(!ui.get_snap_enabled());
        runtime.handle("timeline.toggle_snap", &ui)?;
        runtime.add_marker(&ui)?;
        let marker_revision = runtime
            .project
            .borrow()
            .as_ref()
            .map(Project::revision)
            .ok_or("missing project")?;
        let autosave_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ui.get_autosave_status() != format!("SAVED R{marker_revision}")
            && std::time::Instant::now() < autosave_deadline
        {
            runtime.poll(&ui);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            ui.get_autosave_status(),
            format!("SAVED R{marker_revision}")
        );
        assert_eq!(
            runtime
                .project
                .borrow()
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .map(|sequence| sequence.markers.len()),
            Some(1)
        );
        assert_eq!(ui.get_timeline_markers().row_count(), 1);
        ui.set_selected_marker_index(0);
        runtime.delete_marker(&ui)?;
        assert_eq!(ui.get_timeline_markers().row_count(), 0);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_markers().row_count(), 1);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_markers().row_count(), 0);
        assert_eq!(ui.get_tracks().row_count(), 1);
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        assert_eq!(ui.get_duration_label(), "00:00:02:00");
        assert!(ui.invoke_media_matches("Interview.MP4".into(), "interview".into()));
        assert!(!ui.invoke_media_matches("Interview.MP4".into(), "music".into()));

        ui.set_selected_clip_index(0);
        runtime.seek_time(&ui, RationalTime::new(1, 1)?);
        ui.invoke_track_action(0, "lock".into());
        assert!(
            ui.get_tracks()
                .row_data(0)
                .is_some_and(|track| track.locked)
        );
        assert!(runtime.edit_clip("timeline.split", &ui).is_err());
        assert_eq!(
            prism_persistence::load(&path)?
                .sequences
                .values()
                .next()
                .map(|s| s.tracks[0].clips.len()),
            Some(1)
        );
        ui.invoke_track_action(0, "lock".into());
        ui.invoke_command_invoked("timeline.split".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        let saved = prism_persistence::load(&path)?;
        let sequence = saved.sequences.values().next().ok_or("missing sequence")?;
        assert_eq!(
            sequence.tracks[0].clips[1].source_start,
            RationalTime::new(1, 1)?
        );
        assert_eq!(
            sequence.tracks[0].clips[1].timeline_start,
            RationalTime::new(1, 1)?
        );
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        assert!(ui.get_selected_has_next());
        runtime.toggle_dissolve(&ui)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 1);
        assert!(ui.get_selected_has_transition());
        runtime.toggle_dissolve(&ui)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 0);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 1);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 0);

        if std::env::var_os("PRISM_TEST_FFMPEG").is_some() {
            runtime.seek_time(&ui, RationalTime::new(1, 2)?);
            ui.set_selected_media("av-ntsc-48k.mp4".into());
            runtime.select_media(&ui, 0);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while (!ui.get_has_program_frame()
                || !ui.get_has_source_frame()
                || !runtime
                    .cards
                    .borrow()
                    .first()
                    .is_some_and(|card| card.has_frame))
                && std::time::Instant::now() < deadline
            {
                runtime.poll(&ui);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                ui.get_has_program_frame(),
                "Program preview did not decode: {}",
                ui.get_feedback()
            );
            assert!(
                ui.get_has_source_frame(),
                "Source preview did not decode: {}",
                ui.get_feedback()
            );
        }
        let media_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ui.get_media_task_pending() > 0 && std::time::Instant::now() < media_deadline {
            runtime.poll(&ui);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ui.get_media_task_pending(), 0);
        assert!(matches!(
            ui.get_media_task_status().as_str(),
            "COMPLETE" | "COMPLETE WITH ERRORS"
        ));

        assert_eq!(ui.get_media_cards().row_count(), 1);
        ui.set_search("no match".into());
        ui.invoke_filter_changed();
        assert_eq!(ui.get_media_cards().row_count(), 0);
        ui.set_search("AV-NTSC".into());
        ui.invoke_filter_changed();
        assert_eq!(
            ui.get_media_cards().row_data(0).map(|card| card.index),
            Some(0)
        );
        ui.set_search("".into());
        ui.invoke_filter_changed();
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        assert!(!ui.get_inspector_name().is_empty());
        assert!(ui.get_can_undo());
        assert_eq!(ui.get_history_items().row_count(), 5);

        // Render all four workspaces and the minimum supported desktop size.
        // Optional PPM artifacts support visual review without a live desktop session.
        let snapshot_directory = std::env::var_os("PRISM_UI_SNAPSHOTS");
        for (width, height) in [(1440, 900), (1100, 720)] {
            window.set_size(slint::PhysicalSize::new(width, height));
            for workspace in [
                "HOME",
                "IMPORT",
                "EDIT",
                "EXPORT",
                "HISTORY",
                "JOBS",
                "ASSEMBLY",
                "COLLAPSED",
                "ZOOM",
            ] {
                ui.set_layout_preset(
                    if workspace == "ASSEMBLY" {
                        "Assembly"
                    } else {
                        "Editing"
                    }
                    .into(),
                );
                ui.set_inspector_open(workspace != "COLLAPSED");
                ui.set_inspector_tab(
                    match workspace {
                        "HISTORY" => "History",
                        "JOBS" => "Jobs",
                        _ => "Properties",
                    }
                    .into(),
                );
                ui.set_timeline_zoom(if workspace == "ZOOM" { 2.0 } else { 1.0 });
                ui.set_timeline_offset(if workspace == "ZOOM" { 0.25 } else { 0.0 });
                runtime.update_ruler(&ui);
                ui.set_active_workspace(
                    match workspace {
                        "HOME" | "IMPORT" | "EXPORT" => workspace,
                        _ => "EDIT",
                    }
                    .into(),
                );
                let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
                window.request_redraw();
                assert!(window.draw_if_needed(|renderer| {
                    renderer.render(buffer.make_mut_slice(), width as usize);
                }));
                assert!(buffer.as_bytes().iter().any(|byte| *byte > 80));
                if let Some(directory) = &snapshot_directory {
                    use std::io::Write;
                    std::fs::create_dir_all(directory)?;
                    let mut file = std::fs::File::create(
                        std::path::Path::new(directory).join(format!("{workspace}-{width}.ppm")),
                    )?;
                    write!(file, "P6\n{width} {height}\n255\n")?;
                    file.write_all(buffer.as_bytes())?;
                }
            }
        }
        ui.set_selected_clip_index(1);
        ui.invoke_command_invoked("timeline.delete".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        runtime.seek_ratio(&ui, 1.0);
        assert!(!ui.get_has_program_frame()); // Half-open clip interval at sequence end.
        assert_eq!(ui.get_timecode(), "00:00:01:00");
        assert_eq!(display_timecode(RationalTime::new(3661, 1)?), "01:01:01:00");
        ui.invoke_command_invoked("edit.undo".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        ui.invoke_command_invoked("edit.redo".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        assert!(ui.get_can_undo());
        let undo_count = runtime.undo.borrow().len();
        *runtime.project_path.borrow_mut() = Some(path.join("cannot-save-here.prism"));
        assert!(runtime.apply_history(&ui, false).is_err());
        assert_eq!(runtime.undo.borrow().len(), undo_count);
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        *runtime.project_path.borrow_mut() = Some(path.clone());

        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        ui.set_clip_start("0".into());
        ui.set_clip_source_in("0".into());
        ui.set_clip_duration("0.5".into());
        runtime.apply_timing(&ui)?;
        assert_eq!(ui.get_duration_label(), "00:00:00:15");
        assert!(!ui.get_can_redo());
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_duration_label(), "00:00:01:00");

        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.adjust_clip_frame(&ui, "timeline.move_right")?;
        assert_eq!(ui.get_clip_start(), "1/30");
        runtime.apply_history(&ui, false)?;
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.adjust_clip_frame(&ui, "timeline.trim_out_left")?;
        assert_eq!(ui.get_clip_duration(), "29/30");
        runtime.apply_history(&ui, false)?;
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.drag_clip(&ui, 0, 1.0 / 30.0, "slip")?;
        assert_eq!(ui.get_clip_source_in(), "1/30");
        runtime.apply_history(&ui, false)?;
        assert!(
            runtime
                .precision_edit_frame(&ui, "timeline.roll_out_right")
                .is_err()
        );

        runtime.drag_clip(&ui, 0, 1.0 / 30.0, "move")?;
        assert_eq!(ui.get_clip_start(), "1/30");
        runtime.apply_history(&ui, false)?;
        runtime.drag_clip(&ui, 0, -1.0 / 30.0, "out")?;
        assert_eq!(ui.get_clip_duration(), "29/30");
        runtime.apply_history(&ui, false)?;

        runtime.seek_time(&ui, RationalTime::ZERO);
        runtime.handle("playback.play_pause", &ui)?;
        assert!(ui.get_playing());
        runtime.playback_started.set(Some(
            std::time::Instant::now() - std::time::Duration::from_millis(100),
        ));
        runtime.poll(&ui);
        assert!(runtime.playhead.get() > RationalTime::ZERO);
        runtime.handle("playback.play_pause", &ui)?;
        assert!(!ui.get_playing());

        let sequence_position = runtime.playhead.get();
        runtime.source_duration.set(RationalTime::new(1, 1)?);
        runtime.source_in.set(RationalTime::new(1, 4)?);
        runtime.source_out.set(RationalTime::new(1, 2)?);
        ui.set_source_ready(true);
        ui.set_selected_media_index(0);
        runtime.seek_source(&ui, 0.4);
        assert_eq!(runtime.playhead.get(), sequence_position);
        runtime.append_source(&ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        ui.set_selected_clip_index(0);
        runtime.edit_clip("timeline.ripple_delete", &ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        ui.set_selected_clip_index(0);
        runtime.drag_clip(&ui, 0, 1.0 / 30.0, "roll_out")?;
        runtime.apply_history(&ui, false)?;
        runtime.append_source(&ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 3);
        ui.set_selected_clip_index(1);
        runtime.drag_clip(&ui, 1, 1.0 / 30.0, "slide")?;
        runtime.apply_history(&ui, false)?;
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        let saved = prism_persistence::load(&path)?;
        let sequence = saved.sequences.values().next().ok_or("missing sequence")?;
        assert_eq!(
            sequence.tracks[0].clips[1].source_start,
            RationalTime::new(1, 4)?
        );
        assert_eq!(
            sequence.tracks[0].clips[1].source_duration,
            RationalTime::new(1, 4)?
        );
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 1);

        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.duplicate_clip(&ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        let duplicated = prism_persistence::load(&path)?;
        let duplicated_track = &duplicated
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?
            .tracks[0];
        assert_eq!(
            duplicated_track.clips[1].source_start,
            duplicated_track.clips[0].source_start
        );
        assert_eq!(
            duplicated_track.clips[1].source_duration,
            duplicated_track.clips[0].source_duration
        );
        assert_eq!(
            duplicated_track.clips[1].timeline_start,
            duplicated_track.clips[0]
                .timeline_start
                .checked_add(duplicated_track.clips[0].source_duration)?
        );
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 1);

        let replacement = std::env::temp_dir().join(format!(
            "prism-relink-test-{}-{nonce}.mp4",
            std::process::id()
        ));
        std::fs::copy(&fixture, &replacement)?;
        ui.set_selected_media_index(0);
        runtime.relink_source_to(&ui, replacement.clone())?;
        assert_eq!(ui.get_source_detail(), replacement.display().to_string());
        assert_eq!(
            prism_persistence::load(&path)?
                .media
                .values()
                .next()
                .map(|asset| asset.source_path.as_str()),
            replacement.to_str()
        );
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_source_detail(), fixture.display().to_string());

        let media_id = runtime
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.keys().next().copied())
            .ok_or("missing media")?;
        let proxy_path =
            std::env::temp_dir().join(format!("prism-ui-proxy-test-{}.mp4", std::process::id()));
        std::fs::write(&proxy_path, b"proxy")?;
        let (proxy_sender, proxy_receiver) = std::sync::mpsc::sync_channel(1);
        proxy_sender.send(Ok(proxy_path.clone()))?;
        *runtime.proxy_generation.borrow_mut() = Some(ProxyGeneration {
            receiver: proxy_receiver,
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            media_id,
        });
        ui.set_proxy_generating(true);
        runtime.poll(&ui);
        assert_eq!(ui.get_selected_proxy(), proxy_path.display().to_string());
        assert_eq!(ui.get_proxy_status(), PROXY_AVAILABLE);
        assert!(ui.get_proxy_active());
        assert!(!ui.get_proxy_generating());
        runtime.clear_proxy(&ui)?;
        assert_eq!(ui.get_selected_proxy(), "");
        assert!(!ui.get_proxy_active());
        let sequence_id = runtime
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .map(|sequence| sequence.id)
            .ok_or("missing sequence")?;
        let audio_track_id = TrackId::new();
        runtime.commit(
            &ui,
            Command::CreateTrack {
                sequence_id,
                id: audio_track_id,
                kind: TrackKind::Audio,
            },
            "Create test audio track",
        )?;
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.change_track(&ui, 0, "down")?;
        assert_eq!(ui.get_selected_track_index(), 1);
        assert_eq!(
            prism_persistence::load(&path)?
                .sequences
                .values()
                .next()
                .and_then(|sequence| sequence.tracks.first())
                .map(|track| track.id),
            Some(audio_track_id)
        );
        runtime.apply_history(&ui, false)?;
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_tracks().row_count(), 1);
        let cache_root = path.with_extension("prism-cache");
        let cache_layout = CacheLayout::new(&cache_root);
        let cache_key = CacheKey::from_parts("thumbnail", b"fixture", "rgba", b"160x90");
        cache_layout.write_atomic(cache_key, "rgba", b"cached thumbnail")?;
        runtime.update_cache_state(&ui);
        assert_eq!(ui.get_cache_status(), "READY");
        assert!(ui.get_cache_detail().contains("1 artifacts"));
        runtime.clean_cache(&ui)?;
        assert!(ui.get_cache_cleaning());
        let cleanup_started = std::time::Instant::now();
        while ui.get_cache_cleaning()
            && cleanup_started.elapsed() < std::time::Duration::from_secs(2)
        {
            runtime.poll(&ui);
            std::thread::yield_now();
        }
        assert!(!ui.get_cache_cleaning());
        assert_eq!(ui.get_cache_detail(), "0 artifacts · 0 B");
        runtime.autosave_scheduler.borrow_mut().take();
        for candidate in prism_persistence::list_autosaves(&path)? {
            std::fs::remove_file(candidate.path)?;
        }
        std::fs::remove_file(proxy_path)?;
        std::fs::remove_file(replacement)?;
        std::fs::remove_file(path)?;
        std::fs::remove_dir_all(cache_root)?;
        ui.hide()?;
        Ok(())
    }

    #[test]
    fn proxy_versions_never_overwrite_existing_media() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("prism-proxy-version-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let base = directory.join("interview.proxy.mp4");
        let version_two = directory.join("interview.proxy.v2.mp4");
        std::fs::write(&base, b"original proxy")?;
        std::fs::write(&version_two, b"second proxy")?;
        assert_eq!(
            next_proxy_version(&base)?,
            directory.join("interview.proxy.v3.mp4")
        );
        assert_eq!(
            next_proxy_version(&version_two)?,
            directory.join("interview.proxy.v3.mp4")
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn inspector_timing_parser_is_exact_and_rejects_invalid_values()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::parse_timing;
        assert_eq!(parse_timing("1.25")?, RationalTime::new(5, 4)?);
        assert_eq!(
            parse_timing(" 1001/30000 ")?,
            RationalTime::new(1001, 30000)?
        );
        assert_eq!(parse_timing(".5")?, RationalTime::new(1, 2)?);
        for input in [
            "-1",
            "-0.5",
            "NaN",
            "inf",
            "1/0",
            "0.1234567",
            "1..2",
            "9223372036854775807.9",
            "",
        ] {
            assert!(parse_timing(input).is_err(), "accepted {input}");
        }
        Ok(())
    }
}
