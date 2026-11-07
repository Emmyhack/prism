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
    ui.on_command_invoked(|id| {
        if let Some(command) = CommandRegistry::find(id.as_str()) {
            eprintln!("Prism command: {} ({})", command.id, command.label);
        } else {
            eprintln!("Prism command is not registered: {id}");
        }
    });
    ui.run()
}
