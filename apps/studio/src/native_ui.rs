//! Native Slint adapter for the Prism desktop workspace.

use prism_commands::CommandRegistry;

slint::include_modules!();

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
                let status = match command.id {
                    "playback.play_pause" => "PLAYBACK COMMAND",
                    "playback.step_backward" => "STEP BACK",
                    "playback.step_forward" => "STEP FORWARD",
                    "timeline.insert_clip" => "INSERT EDIT REQUESTED",
                    "timeline.ripple_delete" => "RIPPLE DELETE REQUESTED",
                    "timeline.add_transition" => "TRANSITION REQUESTED",
                    "timeline.toggle_track_lock" => "TRACK LOCK TOGGLED",
                    "timeline.toggle_track_mute" => "TRACK MUTE TOGGLED",
                    "timeline.toggle_track_visibility" => "TRACK VISIBILITY TOGGLED",
                    _ => command.label,
                };
                ui.set_export_status(status.into());
            }
        } else {
            eprintln!("Prism command is not registered: {id}");
        }
    });
    ui.run()
}
