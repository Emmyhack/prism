//! Executable-level project creation and no-clobber verification.

use std::{fs, process::Command};
use uuid::Uuid;

#[test]
fn creates_inspects_and_refuses_to_replace_project() -> Result<(), Box<dyn std::error::Error>> {
    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-create-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("created.prism");
    let binary = env!("CARGO_BIN_EXE_prism-cli");

    let created = Command::new(binary)
        .arg("create-project")
        .arg("--project")
        .arg(&project_path)
        .arg("--name")
        .arg("Created by CLI")
        .output()?;
    assert!(created.status.success());
    assert!(project_path.is_file());

    let inspected = Command::new(binary)
        .arg("inspect")
        .arg("--project")
        .arg(&project_path)
        .output()?;
    assert!(inspected.status.success());
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(json["name"], "Created by CLI");
    assert_eq!(json["revision"], 0);
    assert_eq!(json["media"].as_array().map(Vec::len), Some(0));
    assert_eq!(json["sequences"].as_array().map(Vec::len), Some(0));

    let refused = Command::new(binary)
        .arg("create-project")
        .arg("--project")
        .arg(&project_path)
        .arg("--name")
        .arg("Replacement")
        .output()?;
    assert!(!refused.status.success());
    let reinspected = Command::new(binary)
        .arg("inspect")
        .arg("--project")
        .arg(&project_path)
        .output()?;
    let json: serde_json::Value = serde_json::from_slice(&reinspected.stdout)?;
    assert_eq!(json["name"], "Created by CLI");

    fs::remove_file(&project_path)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}
