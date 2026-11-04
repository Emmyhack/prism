//! Command-line entry point for headless Prism workflows.

fn main() {
    let project = prism_project::Project::new("Untitled");
    println!(
        "Prism project: {} (revision {})",
        project.name,
        project.revision()
    );
}
