//! Desktop Prism Studio entry point.

// Slint's generated item tree uses internal unwraps and invariant panics. Keep
// these exceptions scoped to generated adapter code; hand-written Studio code
// retains the workspace's strict lint policy.
#[allow(clippy::panic, clippy::unwrap_used)]
mod native_ui;

fn main() {
    let command = match prism_studio::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("Prism Studio: {error}");
            std::process::exit(2);
        }
    };
    let graphical_project = match &command {
        prism_studio::StudioCommand::Start => Some(None),
        prism_studio::StudioCommand::Open { project } => Some(Some(project.clone())),
        _ => None,
    };
    if let Some(project) = graphical_project {
        if let Err(error) = native_ui::launch(project) {
            eprintln!("Prism Studio: could not start desktop interface: {error}");
            std::process::exit(1);
        }
        return;
    }

    match prism_studio::run(command) {
        Ok(Some(summary)) => println!("{summary}"),
        Ok(None) => {}
        Err(error) => {
            eprintln!("Prism Studio: {error}");
            std::process::exit(1);
        }
    }
}
