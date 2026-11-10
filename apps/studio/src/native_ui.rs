//! Native Slint adapter for the Prism desktop workspace.

use prism_commands::CommandRegistry;
use prism_export::{ExportPreset, SingleTrackMp4ExportRequest};
use prism_project::{Project, TrackKind};
use prism_studio::desktop::{ExportController, ExportStatus, ExportWorker, ExportWorkerEvent};
use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};
use thiserror::Error;

slint::include_modules!();

fn command_status(id: &str, label: &'static str) -> &'static str {
    match id {
        "playback.play_pause" => "PLAYBACK COMMAND",
        "playback.step_backward" => "STEP BACK",
        "playback.step_forward" => "STEP FORWARD",
        "timeline.insert_clip" => "INSERT EDIT REQUESTED",
        "timeline.ripple_delete" => "RIPPLE DELETE REQUESTED",
        "timeline.add_transition" => "TRANSITION REQUESTED",
        "timeline.toggle_track_lock" => "TRACK LOCK TOGGLED",
        "timeline.toggle_track_mute" => "TRACK MUTE TOGGLED",
        "timeline.toggle_track_visibility" => "TRACK VISIBILITY TOGGLED",
        "export.queue" => "EXPORT QUEUED",
        "export.cancel" => "EXPORT CANCELLATION REQUESTED",
        "export.retry" => "EXPORT RETRY REQUESTED",
        "export.remove" => "EXPORT JOB REMOVED",
        _ => label,
    }
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
                .map(|track| (*sequence_id, track.id))
        });
    let Some((sequence_id, track_id)) = target else {
        controller
            .fail("project has no visible non-empty video track")
            .map_err(|error| error.to_string())?;
        return Ok(());
    };
    *worker = Some(ExportWorker::spawn_single_video_track(
        PathBuf::from("ffmpeg"),
        PathBuf::from("ffprobe"),
        project.clone(),
        sequence_id,
        track_id,
        SingleTrackMp4ExportRequest::from_preset(ExportPreset::Youtube1080p, output),
    ));
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

#[derive(Debug)]
struct NativeExportRuntime {
    project: Option<Project>,
    controller: RefCell<ExportController>,
    worker: RefCell<Option<ExportWorker>>,
}

impl NativeExportRuntime {
    fn new(project: Option<Project>) -> Self {
        Self {
            project,
            controller: RefCell::new(ExportController::new()),
            worker: RefCell::new(None),
        }
    }

    fn handle(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        match command {
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
            _ => Err("unsupported export command".to_owned()),
        }
    }

    fn queue(&self, ui: &PrismStudio) -> Result<(), String> {
        let project = self
            .project
            .as_ref()
            .ok_or_else(|| "open a project before exporting".to_owned())?;
        let output = PathBuf::from(ui.get_export_output().as_str());
        let mut controller = self.controller.borrow_mut();
        controller.queue(output.clone());
        launch_default_export(
            &mut controller,
            &mut self.worker.borrow_mut(),
            project,
            output,
        )
    }

    fn retry(&self) -> Result<(), String> {
        let project = self
            .project
            .as_ref()
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
            project,
            output,
        )
    }

    fn poll(&self, ui: &PrismStudio) {
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

fn load_project_into_ui(
    ui: &PrismStudio,
    project_path: Option<PathBuf>,
) -> Result<Option<Project>, prism_persistence::PersistenceError> {
    let Some(project_path) = project_path else {
        return Ok(None);
    };
    let project = prism_persistence::load(&project_path)?;
    let summary = prism_studio::project_summary(&project);
    ui.set_has_project(true);
    ui.set_project_name(summary.name.into());
    ui.set_project_path(project_path.display().to_string().into());
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
    ui.set_export_output(
        project_path
            .with_extension("export.mp4")
            .display()
            .to_string()
            .into(),
    );
    Ok(Some(project))
}

/// Opens the main editor window and runs the native event loop.
///
/// # Errors
///
/// Returns a platform error if a window cannot be created or the native event
/// loop cannot run.
pub fn launch(project_path: Option<PathBuf>) -> Result<(), NativeUiError> {
    let ui = PrismStudio::new()?;
    let loaded_project = load_project_into_ui(&ui, project_path)?;
    let weak_ui = ui.as_weak();
    let export_runtime = Rc::new(NativeExportRuntime::new(loaded_project));
    let command_runtime = Rc::clone(&export_runtime);
    ui.on_command_invoked(move |id| {
        if let Some(command) = CommandRegistry::find(id.as_str()) {
            eprintln!("Prism command: {} ({})", command.id, command.label);
            if let Some(ui) = weak_ui.upgrade() {
                if !command.id.starts_with("export.") {
                    ui.set_export_status(command_status(command.id, command.label).into());
                    return;
                }
                let export_result = command_runtime.handle(command.id, &ui);
                if let Err(error) = export_result {
                    ui.set_export_error(error.clone().into());
                } else {
                    let controller = command_runtime.controller.borrow();
                    apply_export_status(&ui, controller.status().as_ref());
                }
            }
        } else {
            eprintln!("Prism command is not registered: {id}");
        }
    });
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
    use super::{NativeExportStatus, command_status};
    use prism_studio::desktop::ExportController;
    use std::path::PathBuf;

    #[test]
    fn registered_commands_have_stable_feedback_labels() {
        assert_eq!(
            command_status("timeline.ripple_delete", "Ripple Delete"),
            "RIPPLE DELETE REQUESTED"
        );
        assert_eq!(
            command_status("export.queue", "Queue Export"),
            "EXPORT QUEUED"
        );
        assert_eq!(
            command_status("export.cancel", "Cancel Export"),
            "EXPORT CANCELLATION REQUESTED"
        );
        assert_eq!(
            command_status("export.retry", "Retry Export"),
            "EXPORT RETRY REQUESTED"
        );
        assert_eq!(
            command_status("export.remove", "Remove Export Job"),
            "EXPORT JOB REMOVED"
        );
        assert_eq!(command_status("unknown.command", "Unknown"), "Unknown");
    }

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
}
