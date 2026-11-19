//! Generates deterministic third-party notices from Cargo's locked metadata.

use serde::Deserialize;
use std::{
    env,
    error::Error,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    process::Command,
};

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    license: Option<String>,
    license_file: Option<String>,
    source: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let output_path = PathBuf::from(
        arguments
            .next()
            .ok_or("usage: prism-license-inventory <output-path>")?,
    );
    if arguments.next().is_some() {
        return Err("usage: prism-license-inventory <output-path>".into());
    }

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--locked", "--format-version", "1"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let mut metadata: Metadata = serde_json::from_slice(&output.stdout)?;
    metadata.packages.retain(|package| package.source.is_some());
    metadata
        .packages
        .sort_by(|left, right| (&left.name, &left.version).cmp(&(&right.name, &right.version)));

    let file = File::create(output_path)?;
    let mut writer = BufWriter::new(file);
    writeln!(writer, "Prism third-party dependency inventory")?;
    writeln!(
        writer,
        "Generated from the locked Cargo dependency graph.\n"
    )?;
    for package in metadata.packages {
        let license = package
            .license
            .or_else(|| {
                package
                    .license_file
                    .map(|path| format!("License file: {path}"))
            })
            .ok_or_else(|| {
                format!(
                    "dependency {} {} has no declared license",
                    package.name, package.version
                )
            })?;
        writeln!(writer, "{} {}", package.name, package.version)?;
        writeln!(writer, "  License: {license}")?;
        writeln!(
            writer,
            "  Source: {}\n",
            package.source.as_deref().unwrap_or("unknown")
        )?;
    }
    writer.flush()?;
    Ok(())
}
