//! Native Slint adapter for the Prism desktop workspace.

use prism_commands::CommandRegistry;

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

/// Opens the main editor window and runs the native event loop.
///
/// # Errors
///
/// Returns a platform error if a window cannot be created or the native event
/// loop cannot run.
pub fn launch() -> Result<(), slint::PlatformError> {
    let ui = PrismStudio::new()?;
    let weak_ui = ui.as_weak();
    ui.on_command_invoked(move |id| {
        if let Some(command) = CommandRegistry::find(id.as_str()) {
            eprintln!("Prism command: {} ({})", command.id, command.label);
            if let Some(ui) = weak_ui.upgrade() {
                ui.set_export_status(command_status(command.id, command.label).into());
            }
        } else {
            eprintln!("Prism command is not registered: {id}");
        }
    });
    ui.run()
}

#[cfg(test)]
mod tests {
    use super::command_status;

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
}
