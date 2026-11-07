//! Build-time compilation for the native Slint interface.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    slint_build::compile("ui/studio.slint")?;
    Ok(())
}
