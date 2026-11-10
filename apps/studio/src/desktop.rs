//! Toolkit-neutral desktop workspace model.
//!
//! Native adapters render this model with their platform toolkit. Keeping layout,
//! interaction state, and design tokens here makes the editing experience stable
//! across macOS, Windows, and Linux without coupling it to a renderer.

use prism_core::{SequenceId, TrackId};
use prism_export::{
    BasicMp4Exporter, ExportCancellation, ExportJobId, ExportJobState, ExportQueue,
    ExportQueueError, SingleTrackMp4ExportRequest,
};
use prism_project::Project;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    thread,
    time::Duration,
};

/// Minimum supported desktop viewport in logical pixels.
pub const MIN_VIEWPORT: Size = Size::new(1_024, 640);

/// A two-dimensional logical-pixel size.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Size {
    /// Horizontal extent.
    pub width: u32,
    /// Vertical extent.
    pub height: u32,
}

impl Size {
    /// Creates a logical-pixel size.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// A panel rectangle in logical pixels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    /// Distance from the viewport's left edge.
    pub x: u32,
    /// Distance from the viewport's top edge.
    pub y: u32,
    /// Rectangle width.
    pub width: u32,
    /// Rectangle height.
    pub height: u32,
}

impl Rect {
    const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// Stable names for the primary editing surfaces.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PanelId {
    /// Project media, bins, effects, and search.
    Browser,
    /// Source and program viewer region.
    Viewer,
    /// Context-sensitive clip, track, and effect properties.
    Inspector,
    /// Multitrack sequence editor.
    Timeline,
    /// Compact playback, cache, and background-task feedback.
    StatusBar,
}

/// Computed geometry for one desktop workspace frame.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLayout {
    /// Effective viewport, clamped to the supported minimum.
    pub viewport: Size,
    /// Primary panel rectangles in stable paint order.
    pub panels: Vec<(PanelId, Rect)>,
    /// Whether the inspector is collapsed into a drawer.
    pub inspector_drawer: bool,
}

/// A named workspace layout persisted independently from project media.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePreset {
    /// User-visible preset name.
    pub name: String,
    /// Serialized panel geometry and collapse state.
    pub layout: WorkspaceLayout,
}

/// Atomic JSON persistence for workspace presets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceStore {
    path: PathBuf,
}

impl WorkspaceStore {
    /// Creates a store rooted at `path`; no I/O occurs until save/load.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Loads presets, returning an empty list when no store exists.
    ///
    /// # Errors
    ///
    /// Returns filesystem or invalid-JSON errors.
    pub fn load(&self) -> Result<Vec<WorkspacePreset>, io::Error> {
        if !self.path.is_file() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&self.path)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// Atomically replaces the preset store after validating its JSON.
    ///
    /// # Errors
    ///
    /// Returns filesystem or serialization errors.
    pub fn save(&self, presets: &[WorkspacePreset]) -> Result<(), io::Error> {
        let bytes = serde_json::to_vec_pretty(presets)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = self.path.with_extension("tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, &self.path)
    }

    /// Saves or replaces a named preset while preserving deterministic order.
    ///
    /// # Errors
    ///
    /// Returns invalid-input, serialization, or filesystem errors.
    pub fn upsert(
        &self,
        presets: &mut Vec<WorkspacePreset>,
        preset: WorkspacePreset,
    ) -> Result<(), io::Error> {
        if preset.name.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace name is empty",
            ));
        }
        if let Some(existing) = presets.iter_mut().find(|item| item.name == preset.name) {
            *existing = preset;
        } else {
            presets.push(preset);
            presets.sort_by(|left, right| left.name.cmp(&right.name));
        }
        self.save(presets)
    }

    /// Deletes a named preset and persists the resulting collection.
    ///
    /// # Errors
    ///
    /// Returns serialization or filesystem errors.
    pub fn delete(
        &self,
        presets: &mut Vec<WorkspacePreset>,
        name: &str,
    ) -> Result<bool, io::Error> {
        let before = presets.len();
        presets.retain(|preset| preset.name != name);
        self.save(presets)?;
        Ok(before != presets.len())
    }

    /// Clears all custom presets and persists the empty collection.
    ///
    /// # Errors
    ///
    /// Returns serialization or filesystem errors.
    pub fn reset(&self, presets: &mut Vec<WorkspacePreset>) -> Result<(), io::Error> {
        presets.clear();
        self.save(presets)
    }
}

impl WorkspaceLayout {
    /// Computes a familiar NLE layout for the supplied viewport.
    ///
    /// Wide displays use browser/viewer/inspector columns. Compact displays
    /// preserve viewer and timeline space and move the inspector to a drawer.
    #[must_use]
    pub fn for_viewport(viewport: Size) -> Self {
        let viewport = Size::new(
            viewport.width.max(MIN_VIEWPORT.width),
            viewport.height.max(MIN_VIEWPORT.height),
        );
        let top = 48;
        let status = 28;
        let available = viewport.height - top - status;
        let timeline_height = (available * 42 / 100).clamp(260, 520);
        let upper_height = available - timeline_height;
        let browser_width = (viewport.width * 20 / 100).clamp(240, 360);
        let inspector_drawer = viewport.width < 1_280;
        let inspector_width = if inspector_drawer {
            0
        } else {
            (viewport.width * 20 / 100).clamp(280, 380)
        };
        let viewer_width = viewport.width - browser_width - inspector_width;

        let mut panels = vec![
            (
                PanelId::Browser,
                Rect::new(0, top, browser_width, upper_height),
            ),
            (
                PanelId::Viewer,
                Rect::new(browser_width, top, viewer_width, upper_height),
            ),
        ];
        if !inspector_drawer {
            panels.push((
                PanelId::Inspector,
                Rect::new(
                    viewport.width - inspector_width,
                    top,
                    inspector_width,
                    upper_height,
                ),
            ));
        }
        panels.extend([
            (
                PanelId::Timeline,
                Rect::new(0, top + upper_height, viewport.width, timeline_height),
            ),
            (
                PanelId::StatusBar,
                Rect::new(0, viewport.height - status, viewport.width, status),
            ),
        ]);
        Self {
            viewport,
            panels,
            inspector_drawer,
        }
    }

    /// Returns the rectangle assigned to a panel, if it is currently visible.
    #[must_use]
    pub fn panel(&self, panel: PanelId) -> Option<Rect> {
        self.panels
            .iter()
            .find_map(|(candidate, rect)| (*candidate == panel).then_some(*rect))
    }
}

/// RGBA color with eight-bit channels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

/// Semantic colors for Prism's low-glare professional theme.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Palette {
    /// Application background.
    pub canvas: Color,
    /// Raised panel background.
    pub surface: Color,
    /// Hovered or selected surface.
    pub surface_active: Color,
    /// Fine separators and input borders.
    pub border: Color,
    /// Primary text.
    pub text: Color,
    /// Secondary text and timecode labels.
    pub text_muted: Color,
    /// Brand and focus accent.
    pub accent: Color,
    /// Non-destructive warning state.
    pub warning: Color,
    /// Error and destructive state.
    pub danger: Color,
}

/// Complete shared design-system metrics.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignSystem {
    /// Semantic dark palette optimized for edit suites.
    pub palette: Palette,
    /// Base spacing unit in logical pixels.
    pub spacing_unit: u8,
    /// Default corner radius in logical pixels.
    pub corner_radius: u8,
    /// Minimum interactive target size.
    pub target_size: u8,
}

impl Default for DesignSystem {
    fn default() -> Self {
        Self {
            palette: Palette {
                canvas: Color(12, 14, 19, 255),
                surface: Color(22, 25, 33, 255),
                surface_active: Color(35, 40, 52, 255),
                border: Color(52, 58, 72, 255),
                text: Color(239, 242, 248, 255),
                text_muted: Color(155, 164, 184, 255),
                accent: Color(112, 92, 255, 255),
                warning: Color(245, 180, 66, 255),
                danger: Color(241, 91, 107, 255),
            },
            spacing_unit: 4,
            corner_radius: 6,
            target_size: 32,
        }
    }
}

/// Editing workspaces exposed in the top-level workspace switcher.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WorkspaceKind {
    /// Media organization and metadata.
    Media,
    /// Primary picture and sound editing.
    #[default]
    Edit,
    /// Color correction and grading.
    Color,
    /// Audio mixing and repair.
    Audio,
    /// Script authoring, permissions, execution, and diagnostics.
    Scripting,
    /// Export setup and delivery monitoring.
    Deliver,
}

impl WorkspaceKind {
    /// Maps the stable native tab index to a workspace.
    #[must_use]
    pub const fn from_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Media),
            1 => Some(Self::Edit),
            2 => Some(Self::Color),
            3 => Some(Self::Audio),
            4 => Some(Self::Scripting),
            5 => Some(Self::Deliver),
            _ => None,
        }
    }

    /// Returns the stable native tab index.
    #[must_use]
    pub const fn index(self) -> i32 {
        match self {
            Self::Media => 0,
            Self::Edit => 1,
            Self::Color => 2,
            Self::Audio => 3,
            Self::Scripting => 4,
            Self::Deliver => 5,
        }
    }

    /// Returns the uppercase label shared with native adapters.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Media => "MEDIA",
            Self::Edit => "EDIT",
            Self::Color => "COLOR",
            Self::Audio => "AUDIO",
            Self::Scripting => "SCRIPTING",
            Self::Deliver => "DELIVER",
        }
    }
}

/// Familiar timeline tools with stable cursor/help labels.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EditTool {
    /// Select and move timeline items.
    #[default]
    Selection,
    /// Create cuts at the pointer.
    Razor,
    /// Adjust an edit boundary.
    Trim,
    /// Change source range without moving the clip.
    Slip,
    /// Move a clip while preserving surrounding duration.
    Slide,
    /// Move an edit point between adjacent clips.
    Roll,
    /// Move the timeline viewport.
    Hand,
    /// Adjust timeline magnification.
    Zoom,
}

/// Transport state shown consistently in viewer and timeline controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TransportState {
    /// The playhead is stationary.
    #[default]
    Stopped,
    /// Playback is moving forward.
    Playing,
    /// Playback is moving backward.
    Reversing,
}

/// User-facing desktop state owned by the UI adapter.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopState {
    /// Active task-oriented workspace.
    pub workspace: WorkspaceKind,
    /// Active timeline editing tool.
    pub tool: EditTool,
    /// Current transport state.
    pub transport: TransportState,
    /// Current frame displayed as editor-standard timecode.
    pub timecode: String,
    /// Whether snapping is enabled.
    pub snapping: bool,
    /// Whether linked selection is enabled.
    pub linked_selection: bool,
    /// Whether the inspector drawer is open on compact layouts.
    pub inspector_open: bool,
}

/// UI-facing adapter for the authoritative export queue.
#[derive(Clone, Debug, Default)]
pub struct ExportController {
    queue: ExportQueue,
    active_job: Option<ExportJobId>,
}

/// Read-only export state projected for native UI adapters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStatus {
    /// Destination of the selected job, formatted for display.
    pub output: String,
    /// Stable short lifecycle label.
    pub label: &'static str,
    /// Encoder-reported progress while the job is running.
    pub progress_percent: Option<u8>,
    /// Actionable failure detail when the worker failed.
    pub error: Option<String>,
    /// Whether cancellation is currently a valid queue transition.
    pub can_cancel: bool,
    /// Whether retry is currently a valid queue transition.
    pub can_retry: bool,
    /// Whether removal is currently a valid queue transition.
    pub can_remove: bool,
}

/// One update emitted by a background Studio export worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportWorkerEvent {
    /// Encoder progress suitable for the authoritative queue.
    Progress(u8),
    /// The output was validated and atomically finalized.
    Completed,
    /// Cooperative cancellation stopped the encoder.
    Cancelled,
    /// Validation, encoder, or finalization failure safe for UI display.
    Failed(String),
}

/// Restricted progress channel supplied to a background export operation.
#[derive(Clone, Debug)]
pub struct ExportProgress {
    sender: Sender<ExportWorkerEvent>,
}

impl ExportProgress {
    /// Reports encoder progress to Studio's UI thread.
    pub fn report(&self, progress_percent: u8) {
        let _ = self
            .sender
            .send(ExportWorkerEvent::Progress(progress_percent));
    }
}

/// Detached background export operation with cooperative cancellation.
#[derive(Debug)]
pub struct ExportWorker {
    cancellation: ExportCancellation,
    receiver: Receiver<ExportWorkerEvent>,
}

impl ExportWorker {
    /// Starts an arbitrary export operation on a background thread.
    ///
    /// The operation is responsible for reporting meaningful intermediate
    /// progress. Studio always emits the terminal event from its result.
    #[must_use]
    pub fn spawn<F>(operation: F) -> Self
    where
        F: FnOnce(ExportCancellation, ExportProgress) -> Result<(), String> + Send + 'static,
    {
        let cancellation = ExportCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let progress = ExportProgress {
                sender: sender.clone(),
            };
            let result = operation(worker_cancellation.clone(), progress);
            let event = if worker_cancellation.is_cancelled() {
                ExportWorkerEvent::Cancelled
            } else {
                match result {
                    Ok(()) => ExportWorkerEvent::Completed,
                    Err(message) => ExportWorkerEvent::Failed(message),
                }
            };
            let _ = sender.send(event);
        });
        Self {
            cancellation,
            receiver,
        }
    }

    /// Starts the real single-video-track exporter on a background thread.
    #[must_use]
    pub fn spawn_single_video_track(
        ffmpeg: PathBuf,
        ffprobe: PathBuf,
        project: Project,
        sequence_id: SequenceId,
        track_id: TrackId,
        request: SingleTrackMp4ExportRequest,
    ) -> Self {
        Self::spawn(move |cancellation, progress| {
            progress.report(0);
            BasicMp4Exporter::new(ffmpeg, ffprobe)
                .export_single_video_track_with_cancellation(
                    &project,
                    sequence_id,
                    track_id,
                    &request,
                    Some(&cancellation),
                )
                .map_err(|error| error.to_string())?;
            progress.report(100);
            Ok(())
        })
    }

    /// Requests cooperative cancellation of the running encoder.
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Returns the next pending worker event without blocking the UI thread.
    ///
    /// # Errors
    ///
    /// Returns [`TryRecvError::Disconnected`] after the worker exits and all
    /// emitted events have been consumed.
    pub fn try_next(&self) -> Result<Option<ExportWorkerEvent>, TryRecvError> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(error @ TryRecvError::Disconnected) => Err(error),
        }
    }

    /// Waits for one worker event for bounded headless coordination.
    ///
    /// # Errors
    ///
    /// Returns a timeout or disconnection error when no event can be received.
    pub fn next_timeout(&self, timeout: Duration) -> Result<ExportWorkerEvent, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
}

impl ExportController {
    /// Creates a controller with no queued exports.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a destination and selects it as the active job.
    pub fn queue(&mut self, output: PathBuf) -> ExportJobId {
        let id = self.queue.enqueue(output);
        self.active_job = Some(id);
        id
    }

    /// Starts the selected job when a worker is ready.
    ///
    /// # Errors
    ///
    /// Returns the queue transition error when no job is selected or the job
    /// is not queued.
    pub fn start(&mut self) -> Result<(), ExportQueueError> {
        self.queue
            .start(self.active_job.ok_or(ExportQueueError::UnknownJob)?)
    }

    /// Requests cancellation of the selected job.
    ///
    /// # Errors
    ///
    /// Returns the queue transition error when no job is selected or the job
    /// is not active.
    pub fn cancel(&mut self) -> Result<(), ExportQueueError> {
        self.queue
            .cancel(self.active_job.ok_or(ExportQueueError::UnknownJob)?)
    }

    /// Requeues the selected failed/cancelled job.
    ///
    /// # Errors
    ///
    /// Returns the queue transition error when no job is selected or the job
    /// is not terminal and retryable.
    pub fn retry(&mut self) -> Result<(), ExportQueueError> {
        self.queue
            .retry(self.active_job.ok_or(ExportQueueError::UnknownJob)?)
    }

    /// Reports progress received from the active encoder worker.
    ///
    /// # Errors
    ///
    /// Returns the queue transition error when no job is selected or progress
    /// is invalid for the active state.
    pub fn report_progress(&mut self, progress_percent: u8) -> Result<(), ExportQueueError> {
        self.queue.report_progress(
            self.active_job.ok_or(ExportQueueError::UnknownJob)?,
            progress_percent,
        )
    }

    /// Marks the active export as atomically finalized.
    ///
    /// # Errors
    ///
    /// Returns the queue transition error when no job is selected or the
    /// worker has not reported 100 percent progress.
    pub fn complete(&mut self) -> Result<(), ExportQueueError> {
        self.queue
            .complete(self.active_job.ok_or(ExportQueueError::UnknownJob)?)
    }

    /// Records an encoder failure for the active export.
    ///
    /// # Errors
    ///
    /// Returns the queue transition error when no job is selected or the job
    /// is already terminal.
    pub fn fail(&mut self, message: impl Into<String>) -> Result<(), ExportQueueError> {
        self.queue.fail(
            self.active_job.ok_or(ExportQueueError::UnknownJob)?,
            message,
        )
    }

    /// Removes the selected terminal job and clears selection.
    ///
    /// # Errors
    ///
    /// Returns the queue transition error when no job is selected or the job
    /// is still active.
    pub fn remove(&mut self) -> Result<(), ExportQueueError> {
        let id = self.active_job.ok_or(ExportQueueError::UnknownJob)?;
        self.queue.remove(id)?;
        self.active_job = None;
        Ok(())
    }

    /// Returns the selected job's authoritative state.
    #[must_use]
    pub fn state(&self) -> Option<&ExportJobState> {
        self.active_job
            .and_then(|id| self.queue.get(id))
            .map(|job| &job.state)
    }

    /// Returns an immutable UI projection of the selected queue job.
    #[must_use]
    pub fn status(&self) -> Option<ExportStatus> {
        let job = self.active_job.and_then(|id| self.queue.get(id))?;
        let (label, progress_percent, error, can_cancel, can_retry, can_remove) = match &job.state {
            ExportJobState::Queued => ("QUEUED", None, None, true, false, false),
            ExportJobState::Running { progress_percent } => (
                "EXPORTING",
                Some(*progress_percent),
                None,
                true,
                false,
                false,
            ),
            ExportJobState::Completed => ("COMPLETED", Some(100), None, false, false, true),
            ExportJobState::Cancelled => ("CANCELLED", None, None, false, true, true),
            ExportJobState::Failed { message } => {
                ("FAILED", None, Some(message.clone()), false, true, true)
            }
        };
        Some(ExportStatus {
            output: job.output.display().to_string(),
            label,
            progress_percent,
            error,
            can_cancel,
            can_retry,
            can_remove,
        })
    }

    /// Applies one background worker update to the authoritative queue.
    ///
    /// # Errors
    ///
    /// Returns a queue transition error for stale or out-of-order events.
    pub fn apply_worker_event(&mut self, event: ExportWorkerEvent) -> Result<(), ExportQueueError> {
        match event {
            ExportWorkerEvent::Progress(progress_percent) => self.report_progress(progress_percent),
            ExportWorkerEvent::Completed => {
                self.report_progress(100)?;
                self.complete()
            }
            ExportWorkerEvent::Cancelled => self.cancel(),
            ExportWorkerEvent::Failed(message) => self.fail(message),
        }
    }
}

impl Default for DesktopState {
    fn default() -> Self {
        Self {
            workspace: WorkspaceKind::Edit,
            tool: EditTool::Selection,
            transport: TransportState::Stopped,
            timecode: "00:00:00:00".to_owned(),
            snapping: true,
            linked_selection: true,
            inspector_open: false,
        }
    }
}

impl DesktopState {
    /// Toggles between playing and stopped states.
    pub fn toggle_playback(&mut self) {
        self.transport = match self.transport {
            TransportState::Playing => TransportState::Stopped,
            TransportState::Stopped | TransportState::Reversing => TransportState::Playing,
        };
    }

    /// Selects an editing tool and returns focus to timeline interaction.
    pub fn select_tool(&mut self, tool: EditTool) {
        self.tool = tool;
    }

    /// Selects a workspace by the stable native tab index.
    ///
    /// Invalid adapter values are ignored so malformed platform events cannot
    /// move the editor into an undefined state.
    pub fn select_workspace_index(&mut self, index: i32) {
        if let Some(workspace) = WorkspaceKind::from_index(index) {
            self.workspace = workspace;
        }
    }

    /// Selects a timeline tool by the stable native toolbar index.
    pub fn select_tool_index(&mut self, index: i32) {
        let tool = match index {
            0 => Some(EditTool::Selection),
            1 => Some(EditTool::Razor),
            2 => Some(EditTool::Trim),
            3 => Some(EditTool::Slip),
            4 => Some(EditTool::Slide),
            5 => Some(EditTool::Roll),
            6 => Some(EditTool::Hand),
            7 => Some(EditTool::Zoom),
            _ => None,
        };
        if let Some(tool) = tool {
            self.tool = tool;
        }
    }

    /// Applies one resolved keyboard shortcut to desktop interaction state.
    pub fn apply_shortcut(&mut self, action: crate::ShortcutAction) {
        use crate::ShortcutAction;
        match action {
            ShortcutAction::SelectTool => self.tool = EditTool::Selection,
            ShortcutAction::RazorTool => self.tool = EditTool::Razor,
            ShortcutAction::TrimTool => self.tool = EditTool::Trim,
            ShortcutAction::SlipTool => self.tool = EditTool::Slip,
            ShortcutAction::SlideTool => self.tool = EditTool::Slide,
            ShortcutAction::RollTool => self.tool = EditTool::Roll,
            ShortcutAction::PlayPause => self.toggle_playback(),
            ShortcutAction::ToggleSnap => self.snapping = !self.snapping,
            ShortcutAction::Insert
            | ShortcutAction::RippleDelete
            | ShortcutAction::StepBackward
            | ShortcutAction::StepForward
            | ShortcutAction::AddMarker
            | ShortcutAction::Undo
            | ShortcutAction::Redo => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DesktopState, EditTool, ExportController, ExportStatus, ExportWorker, ExportWorkerEvent,
        MIN_VIEWPORT, PanelId, Size, TransportState, WorkspaceKind, WorkspaceLayout,
        WorkspacePreset, WorkspaceStore,
    };
    use prism_export::ExportJobState;
    use std::path::PathBuf;

    #[test]
    fn wide_layout_matches_familiar_nle_regions_without_overlap() {
        let layout = WorkspaceLayout::for_viewport(Size::new(1_920, 1_080));
        assert_eq!(
            layout
                .panel(PanelId::Browser)
                .map(|rect| rect.x + rect.width),
            layout.panel(PanelId::Viewer).map(|rect| rect.x)
        );
        assert_eq!(
            layout
                .panel(PanelId::Viewer)
                .map(|rect| rect.x + rect.width),
            layout.panel(PanelId::Inspector).map(|rect| rect.x)
        );
        assert_eq!(
            layout
                .panel(PanelId::Inspector)
                .map(|rect| rect.x + rect.width),
            Some(layout.viewport.width)
        );
        assert_eq!(
            layout
                .panel(PanelId::Browser)
                .map(|rect| rect.y + rect.height),
            layout.panel(PanelId::Timeline).map(|rect| rect.y)
        );
        assert!(!layout.inspector_drawer);
    }

    #[test]
    fn workspace_presets_round_trip_atomically() -> Result<(), Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("prism-workspaces-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = WorkspaceStore::new(&path);
        let presets = vec![WorkspacePreset {
            name: "Edit".to_owned(),
            layout: WorkspaceLayout::for_viewport(Size::new(1_440, 900)),
        }];
        store.save(&presets)?;
        assert_eq!(store.load()?, presets);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn workspace_preset_lifecycle_is_deterministic() -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!(
            "prism-workspace-lifecycle-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = WorkspaceStore::new(&path);
        let mut presets = Vec::new();
        store.upsert(
            &mut presets,
            WorkspacePreset {
                name: "Zed".to_owned(),
                layout: WorkspaceLayout::for_viewport(Size::new(1_024, 640)),
            },
        )?;
        store.upsert(
            &mut presets,
            WorkspacePreset {
                name: "Edit".to_owned(),
                layout: WorkspaceLayout::for_viewport(Size::new(1_440, 900)),
            },
        )?;
        assert_eq!(
            presets
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Edit", "Zed"]
        );
        assert!(store.delete(&mut presets, "Edit")?);
        assert!(!store.delete(&mut presets, "Missing")?);
        store.reset(&mut presets)?;
        assert!(presets.is_empty());
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn compact_layout_prioritizes_viewer_and_uses_inspector_drawer() {
        let layout = WorkspaceLayout::for_viewport(Size::new(1_100, 700));
        assert!(layout.inspector_drawer);
        assert_eq!(layout.panel(PanelId::Inspector), None);
        assert!(layout.panel(PanelId::Viewer).is_some());
        assert!(layout.panel(PanelId::Timeline).is_some());
    }

    #[test]
    fn undersized_viewport_is_clamped_to_usable_dimensions() {
        let layout = WorkspaceLayout::for_viewport(Size::new(320, 240));
        assert_eq!(layout.viewport, MIN_VIEWPORT);
    }

    #[test]
    fn desktop_interactions_have_predictable_defaults() {
        let mut state = DesktopState::default();
        assert!(state.snapping);
        assert_eq!(state.transport, TransportState::Stopped);
        state.toggle_playback();
        state.select_tool(EditTool::Razor);
        assert_eq!(state.transport, TransportState::Playing);
        assert_eq!(state.tool, EditTool::Razor);
    }

    #[test]
    fn export_controller_tracks_authoritative_queue_lifecycle()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut controller = ExportController::new();
        assert!(controller.state().is_none());
        controller.queue(PathBuf::from("deliver.mp4"));
        assert_eq!(controller.state(), Some(&ExportJobState::Queued));
        controller.start()?;
        controller.report_progress(100)?;
        controller.complete()?;
        assert_eq!(controller.state(), Some(&ExportJobState::Completed));
        controller.remove()?;
        controller.queue(PathBuf::from("retry.mp4"));
        controller.start()?;
        controller.fail("encoder unavailable")?;
        controller.retry()?;
        controller.cancel()?;
        controller.remove()?;
        assert!(controller.state().is_none());
        Ok(())
    }

    #[test]
    fn export_controller_projects_worker_state_for_native_ui()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut controller = ExportController::new();
        controller.queue(PathBuf::from("deliver.mp4"));
        assert_eq!(
            controller.status(),
            Some(ExportStatus {
                output: "deliver.mp4".to_owned(),
                label: "QUEUED",
                progress_percent: None,
                error: None,
                can_cancel: true,
                can_retry: false,
                can_remove: false,
            })
        );

        controller.start()?;
        controller.report_progress(42)?;
        let running = controller
            .status()
            .ok_or_else(|| std::io::Error::other("missing export status"))?;
        assert_eq!(running.label, "EXPORTING");
        assert_eq!(running.progress_percent, Some(42));
        assert!(running.can_cancel);

        controller.fail("encoder unavailable")?;
        let failed = controller
            .status()
            .ok_or_else(|| std::io::Error::other("missing export status"))?;
        assert_eq!(failed.label, "FAILED");
        assert_eq!(failed.error.as_deref(), Some("encoder unavailable"));
        assert!(failed.can_retry);
        assert!(failed.can_remove);
        assert!(!failed.can_cancel);
        Ok(())
    }

    #[test]
    fn background_export_events_drive_authoritative_queue_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut controller = ExportController::new();
        controller.queue(PathBuf::from("deliver.mp4"));
        controller.start()?;
        let worker = ExportWorker::spawn(|_cancellation, progress| {
            progress.report(35);
            Ok(())
        });

        loop {
            let event = worker.next_timeout(std::time::Duration::from_secs(2))?;
            let completed = event == ExportWorkerEvent::Completed;
            controller.apply_worker_event(event)?;
            if completed {
                break;
            }
        }
        assert_eq!(controller.state(), Some(&ExportJobState::Completed));
        Ok(())
    }

    #[test]
    fn background_export_cancellation_is_reported() -> Result<(), Box<dyn std::error::Error>> {
        let worker = ExportWorker::spawn(|cancellation, _progress| {
            while !cancellation.is_cancelled() {
                std::thread::yield_now();
            }
            Err("export cancelled".to_owned())
        });
        worker.cancel();
        assert_eq!(
            worker.next_timeout(std::time::Duration::from_secs(2))?,
            ExportWorkerEvent::Cancelled
        );
        Ok(())
    }

    #[test]
    fn native_indices_map_to_stable_workspaces_and_tools() {
        let mut state = DesktopState::default();
        state.select_workspace_index(4);
        state.select_tool_index(5);
        assert_eq!(state.workspace, WorkspaceKind::Scripting);
        assert_eq!(state.workspace.index(), 4);
        assert_eq!(state.workspace.label(), "SCRIPTING");
        assert_eq!(state.tool, EditTool::Roll);

        state.select_workspace_index(99);
        state.select_tool_index(-1);
        assert_eq!(state.workspace, WorkspaceKind::Scripting);
        assert_eq!(state.tool, EditTool::Roll);
    }

    #[test]
    fn resolved_shortcuts_drive_the_same_desktop_state_as_pointer_actions() {
        let mut state = DesktopState::default();
        state.apply_shortcut(crate::ShortcutAction::SlipTool);
        state.apply_shortcut(crate::ShortcutAction::PlayPause);
        assert_eq!(state.tool, EditTool::Slip);
        assert_eq!(state.transport, TransportState::Playing);
    }
}
