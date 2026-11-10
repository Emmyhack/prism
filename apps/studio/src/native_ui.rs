//! Native Slint adapter for the Prism desktop workspace.

use prism_commands::CommandRegistry;
use prism_studio::desktop::{ExportController, ExportStatus};
use std::{cell::RefCell, path::PathBuf, rc::Rc};

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

/// Opens the main editor window and runs the native event loop.
///
/// # Errors
///
/// Returns a platform error if a window cannot be created or the native event
/// loop cannot run.
pub fn launch() -> Result<(), slint::PlatformError> {
    let ui = PrismStudio::new()?;
    let weak_ui = ui.as_weak();
    let export_controller = Rc::new(RefCell::new(ExportController::new()));
    ui.on_command_invoked(move |id| {
        if let Some(command) = CommandRegistry::find(id.as_str()) {
            eprintln!("Prism command: {} ({})", command.id, command.label);
            if let Some(ui) = weak_ui.upgrade() {
                let export_result = match command.id {
                    "export.queue" => {
                        export_controller
                            .borrow_mut()
                            .queue(PathBuf::from(ui.get_export_output().as_str()));
                        Ok(())
                    }
                    "export.cancel" => export_controller.borrow_mut().cancel(),
                    "export.retry" => export_controller.borrow_mut().retry(),
                    "export.remove" => export_controller.borrow_mut().remove(),
                    _ => {
                        ui.set_export_status(command_status(command.id, command.label).into());
                        return;
                    }
                };
                if let Err(error) = export_result {
                    ui.set_export_error(error.to_string().into());
                } else {
                    let controller = export_controller.borrow();
                    apply_export_status(&ui, controller.status().as_ref());
                }
            }
        } else {
            eprintln!("Prism command is not registered: {id}");
        }
    });
    ui.run()
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
