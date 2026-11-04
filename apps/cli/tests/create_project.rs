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

    let backup_path = test_directory.join("created-revision-0.prism");
    let backup = Command::new(binary)
        .args(["backup-project", "--project"])
        .arg(&project_path)
        .args(["--output"])
        .arg(&backup_path)
        .output()?;
    assert!(backup.status.success());
    assert!(backup_path.is_file());
    let backup_inspection = Command::new(binary)
        .arg("inspect")
        .arg("--project")
        .arg(&backup_path)
        .output()?;
    assert!(backup_inspection.status.success());
    let backup_json: serde_json::Value = serde_json::from_slice(&backup_inspection.stdout)?;
    assert_eq!(backup_json["name"], "Created by CLI");
    let backup_refused = Command::new(binary)
        .args(["backup-project", "--project"])
        .arg(&project_path)
        .args(["--output"])
        .arg(&backup_path)
        .output()?;
    assert!(!backup_refused.status.success());

    let autosave = Command::new(binary)
        .args(["autosave-project", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(autosave.status.success());
    let autosave_path = test_directory.join("created.prism.autosave.0");
    assert!(autosave_path.is_file());
    let autosave_refused = Command::new(binary)
        .args(["autosave-project", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(!autosave_refused.status.success());

    let recovered_path = test_directory.join("recovered.prism");
    let recovered = Command::new(binary)
        .args(["recover-autosave", "--source"])
        .arg(&autosave_path)
        .args(["--output"])
        .arg(&recovered_path)
        .output()?;
    assert!(recovered.status.success());
    let recovered_inspection = Command::new(binary)
        .args(["inspect", "--project"])
        .arg(&recovered_path)
        .output()?;
    assert!(recovered_inspection.status.success());
    let recovered_json: serde_json::Value = serde_json::from_slice(&recovered_inspection.stdout)?;
    assert_eq!(recovered_json["name"], "Created by CLI");
    let recovery_refused = Command::new(binary)
        .args(["recover-autosave", "--source"])
        .arg(&autosave_path)
        .args(["--output"])
        .arg(&recovered_path)
        .output()?;
    assert!(!recovery_refused.status.success());

    fs::remove_file(&project_path)?;
    fs::remove_file(&backup_path)?;
    fs::remove_file(autosave_path)?;
    fs::remove_file(recovered_path)?;
    fs::remove_dir(&test_directory)?;
    Ok(())
}

#[test]
fn validates_recipe_manifest_and_inputs_without_execution() -> Result<(), Box<dyn std::error::Error>>
{
    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-recipe-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let manifest_path = test_directory.join("recipe.json");
    let inputs_path = test_directory.join("inputs.json");
    fs::write(
        &manifest_path,
        r#"{
            "id":"prism.example.recipe",
            "version":"1.0.0",
            "apiVersion":1,
            "language":"typescript",
            "entry":"main.ts",
            "permissions":["project.read"],
            "parameters":{"clipCount":{"type":"integer","minimum":1,"maximum":20,"default":5}}
        }"#,
    )?;
    fs::write(&inputs_path, r#"{"clipCount":3}"#)?;
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let validated = Command::new(binary)
        .args(["validate-recipe", "--manifest"])
        .arg(&manifest_path)
        .args(["--inputs"])
        .arg(&inputs_path)
        .output()?;
    assert!(validated.status.success());
    assert!(String::from_utf8_lossy(&validated.stdout).contains("prism.example.recipe"));

    fs::write(&inputs_path, r#"{"clipCount":21}"#)?;
    let rejected = Command::new(binary)
        .args(["validate-recipe", "--manifest"])
        .arg(&manifest_path)
        .args(["--inputs"])
        .arg(&inputs_path)
        .output()?;
    assert!(!rejected.status.success());

    fs::remove_file(manifest_path)?;
    fs::remove_file(inputs_path)?;
    fs::remove_dir(test_directory)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn generates_proxy_with_explicit_ffmpeg_boundary() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-proxy-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let source_path = test_directory.join("source.mp4");
    let output_path = test_directory.join("proxy.mp4");
    let ffmpeg_path = test_directory.join("ffmpeg");
    fs::write(&source_path, b"source")?;
    fs::write(
        &ffmpeg_path,
        "#!/bin/sh\nlast=''\nfor arg in \"$@\"; do last=\"$arg\"; done\nprintf proxy > \"$last\"\n",
    )?;
    fs::set_permissions(&ffmpeg_path, fs::Permissions::from_mode(0o755))?;

    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let generated = Command::new(binary)
        .args(["generate-proxy", "--source"])
        .arg(&source_path)
        .args(["--output"])
        .arg(&output_path)
        .args([
            "--width",
            "320",
            "--height",
            "180",
            "--video-codec",
            "h264",
            "--audio-codec",
            "aac",
            "--ffmpeg",
        ])
        .arg(&ffmpeg_path)
        .output()?;
    if !generated.status.success() {
        return Err(String::from_utf8_lossy(&generated.stderr)
            .into_owned()
            .into());
    }
    assert_eq!(fs::read(&output_path)?, b"proxy");

    fs::remove_file(source_path)?;
    fs::remove_file(output_path)?;
    fs::remove_file(ffmpeg_path)?;
    fs::remove_dir(test_directory)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn adds_audio_clip_after_probe_validation() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-audio-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("audio.prism");
    let media_path = test_directory.join("speech.wav");
    let probe_path = test_directory.join("ffprobe");
    let request_path = test_directory.join("build.json");
    fs::write(&media_path, b"audio")?;
    fs::write(
        &probe_path,
        "#!/bin/sh\nprintf '%s' '{\"streams\":[{\"index\":0,\"codec_name\":\"pcm_s16le\",\"codec_type\":\"audio\",\"time_base\":\"1/48000\",\"sample_rate\":\"48000\",\"channels\":2,\"channel_layout\":\"stereo\"}],\"format\":{\"format_name\":\"wav\",\"duration\":\"30.0\"}}'\n",
    )?;
    fs::set_permissions(&probe_path, fs::Permissions::from_mode(0o755))?;
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let created = Command::new(binary)
        .args(["create-project", "--project"])
        .arg(&project_path)
        .args(["--name", "Audio CLI"])
        .output()?;
    assert!(created.status.success());
    let request = serde_json::json!({
        "requestId": "audio-build",
        "apiVersion": "1",
        "expectedRevision": 0,
        "label": "Create audio track",
        "dryRun": false,
        "operations": [
            {"type": "createSequence", "id": "11111111-1111-4111-8111-111111111111", "name": "Main"},
            {"type": "createTrack", "sequenceId": "11111111-1111-4111-8111-111111111111", "id": "22222222-2222-4222-8222-222222222222", "kind": "audio"}
        ]
    });
    fs::write(&request_path, serde_json::to_vec(&request)?)?;
    let built = Command::new(binary)
        .args(["script-transaction", "--project"])
        .arg(&project_path)
        .args(["--request"])
        .arg(&request_path)
        .output()?;
    assert!(built.status.success());

    let added = Command::new(binary)
        .args(["add-audio-clip", "--project"])
        .arg(&project_path)
        .args(["--track", "22222222-2222-4222-8222-222222222222"])
        .args(["--media"])
        .arg(&media_path)
        .args([
            "--source-start",
            "0/1",
            "--timeline-start",
            "0/1",
            "--duration",
            "5/1",
            "--ffprobe",
        ])
        .arg(&probe_path)
        .output()?;
    if !added.status.success() {
        return Err(String::from_utf8_lossy(&added.stderr).into_owned().into());
    }
    let inspected = Command::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(inspected.status.success());
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(json["sequences"][0]["tracks"][0]["kind"], "audio");
    assert_eq!(
        json["sequences"][0]["tracks"][0]["clips"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    fs::remove_file(project_path)?;
    fs::remove_file(media_path)?;
    fs::remove_file(probe_path)?;
    fs::remove_file(request_path)?;
    fs::remove_dir(test_directory)?;
    Ok(())
}

#[test]
fn runs_python_transaction_through_supervised_host() -> Result<(), Box<dyn std::error::Error>> {
    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-python-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("python.prism");
    let script_path = test_directory.join("edit.py");
    let permissions_path = test_directory.join("permissions.json");
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let created = Command::new(binary)
        .args(["create-project", "--project"])
        .arg(&project_path)
        .args(["--name", "Python CLI"])
        .output()?;
    assert!(created.status.success());
    fs::write(
        &script_path,
        "import json, os\nprint(json.dumps({\"requestId\": \"python-test\", \"apiVersion\": \"1\", \"expectedRevision\": int(os.environ[\"PRISM_PROJECT_REVISION\"]), \"label\": \"Python sequence\", \"dryRun\": False, \"operations\": [{\"type\": \"createSequence\", \"id\": \"11111111-1111-4111-8111-111111111111\", \"name\": \"Python Main\"}]}))\n",
    )?;
    fs::write(
        &permissions_path,
        r#"{"requested":["project.read","project.write"],"granted":["project.read","project.write"]}"#,
    )?;
    let executed = Command::new(binary)
        .args(["run-python-script", "--project"])
        .arg(&project_path)
        .args(["--script"])
        .arg(&script_path)
        .args(["--python", "python3", "--permissions"])
        .arg(&permissions_path)
        .output()?;
    if !executed.status.success() {
        return Err(String::from_utf8_lossy(&executed.stderr)
            .into_owned()
            .into());
    }
    let inspected = Command::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(inspected.status.success());
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(json["revision"], 1);
    assert_eq!(json["sequences"][0]["name"], "Python Main");

    fs::remove_file(project_path)?;
    fs::remove_file(script_path)?;
    fs::remove_file(permissions_path)?;
    fs::remove_dir(test_directory)?;
    Ok(())
}

#[test]
fn runs_javascript_transaction_through_same_host_boundary() -> Result<(), Box<dyn std::error::Error>>
{
    let test_directory = std::env::temp_dir().join(format!("prism-cli-js-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("javascript.prism");
    let script_path = test_directory.join("edit.mjs");
    let permissions_path = test_directory.join("permissions.json");
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let created = Command::new(binary)
        .args(["create-project", "--project"])
        .arg(&project_path)
        .args(["--name", "JavaScript CLI"])
        .output()?;
    assert!(created.status.success());
    fs::write(
        &script_path,
        "const request = { requestId: 'javascript-test', apiVersion: '1', expectedRevision: Number(process.env.PRISM_PROJECT_REVISION), label: 'JavaScript sequence', dryRun: false, operations: [{ type: 'createSequence', id: '11111111-1111-4111-8111-111111111111', name: 'JavaScript Main' }] };\nconsole.log(JSON.stringify(request));\n",
    )?;
    fs::write(
        &permissions_path,
        r#"{"requested":["project.read","project.write"],"granted":["project.read","project.write"]}"#,
    )?;
    let executed = Command::new(binary)
        .args(["run-javascript-script", "--project"])
        .arg(&project_path)
        .args(["--script"])
        .arg(&script_path)
        .args(["--node", "node", "--permissions"])
        .arg(&permissions_path)
        .output()?;
    if !executed.status.success() {
        return Err(String::from_utf8_lossy(&executed.stderr)
            .into_owned()
            .into());
    }
    let inspected = Command::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(inspected.status.success());
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(json["revision"], 1);
    assert_eq!(json["sequences"][0]["name"], "JavaScript Main");

    fs::remove_file(project_path)?;
    fs::remove_file(script_path)?;
    fs::remove_file(permissions_path)?;
    fs::remove_dir(test_directory)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn runs_typescript_through_explicit_compiler_and_node() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let test_directory = std::env::temp_dir().join(format!("prism-cli-ts-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("typescript.prism");
    let script_path = test_directory.join("edit.ts");
    let compiler_path = test_directory.join("tsc");
    let permissions_path = test_directory.join("permissions.json");
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let created = Command::new(binary)
        .args(["create-project", "--project"])
        .arg(&project_path)
        .args(["--name", "TypeScript CLI"])
        .output()?;
    assert!(created.status.success());
    fs::write(&script_path, "const value: number = 1;\n")?;
    fs::write(
        &compiler_path,
        "#!/bin/sh\nout=''\nprevious=''\nfor arg in \"$@\"; do if [ \"$previous\" = \"--outDir\" ]; then out=\"$arg\"; fi; previous=\"$arg\"; done\nprintf '%s' 'console.log(JSON.stringify({requestId:\"typescript-test\",apiVersion:\"1\",expectedRevision:Number(process.env.PRISM_PROJECT_REVISION),label:\"TypeScript sequence\",dryRun:false,operations:[{type:\"createSequence\",id:\"11111111-1111-4111-8111-111111111111\",name:\"TypeScript Main\"}]}))' > \"$out/edit.js\"\n",
    )?;
    fs::set_permissions(&compiler_path, fs::Permissions::from_mode(0o755))?;
    fs::write(
        &permissions_path,
        r#"{"requested":["project.read","project.write"],"granted":["project.read","project.write"]}"#,
    )?;
    let executed = Command::new(binary)
        .args(["run-typescript-script", "--project"])
        .arg(&project_path)
        .args(["--script"])
        .arg(&script_path)
        .args(["--tsc"])
        .arg(&compiler_path)
        .args(["--node", "node", "--permissions"])
        .arg(&permissions_path)
        .output()?;
    if !executed.status.success() {
        return Err(String::from_utf8_lossy(&executed.stderr)
            .into_owned()
            .into());
    }
    let inspected = Command::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(inspected.status.success());
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(json["revision"], 1);
    assert_eq!(json["sequences"][0]["name"], "TypeScript Main");

    fs::remove_file(project_path)?;
    fs::remove_file(script_path)?;
    fs::remove_file(compiler_path)?;
    fs::remove_file(permissions_path)?;
    fs::remove_dir(test_directory)?;
    Ok(())
}

#[test]
fn sets_track_lock_through_shared_command_engine() -> Result<(), Box<dyn std::error::Error>> {
    let test_directory =
        std::env::temp_dir().join(format!("prism-cli-lock-test-{}", Uuid::new_v4()));
    fs::create_dir(&test_directory)?;
    let project_path = test_directory.join("lock.prism");
    let request_path = test_directory.join("build.json");
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let sequence_id = "11111111-1111-4111-8111-111111111111";
    let track_id = "22222222-2222-4222-8222-222222222222";
    let second_track_id = "33333333-3333-4333-8333-333333333333";

    let created = Command::new(binary)
        .args(["create-project", "--project"])
        .arg(&project_path)
        .args(["--name", "Track lock test"])
        .output()?;
    assert!(created.status.success());

    let request = serde_json::json!({
        "requestId": "lock-build",
        "apiVersion": "1",
        "expectedRevision": 0,
        "label": "Create track",
        "dryRun": false,
        "operations": [
            {"type": "createSequence", "id": sequence_id, "name": "Main"},
            {"type": "createTrack", "sequenceId": sequence_id, "id": track_id, "kind": "video"},
            {"type": "createTrack", "sequenceId": sequence_id, "id": second_track_id, "kind": "audio"}
        ]
    });
    fs::write(&request_path, serde_json::to_vec(&request)?)?;
    let built = Command::new(binary)
        .args(["script-transaction", "--project"])
        .arg(&project_path)
        .args(["--request"])
        .arg(&request_path)
        .output()?;
    if !built.status.success() {
        return Err(String::from_utf8_lossy(&built.stderr).into_owned().into());
    }

    let locked = Command::new(binary)
        .args(["set-track-locked", "--project"])
        .arg(&project_path)
        .args(["--track", track_id, "--locked", "true"])
        .output()?;
    if !locked.status.success() {
        return Err(String::from_utf8_lossy(&locked.stderr).into_owned().into());
    }
    let reordered = Command::new(binary)
        .args(["reorder-track", "--project"])
        .arg(&project_path)
        .args(["--track", track_id, "--index", "1"])
        .output()?;
    if !reordered.status.success() {
        return Err(String::from_utf8_lossy(&reordered.stderr)
            .into_owned()
            .into());
    }
    let hidden = Command::new(binary)
        .args(["set-track-visible", "--project"])
        .arg(&project_path)
        .args(["--track", track_id, "--visible", "false"])
        .output()?;
    assert!(hidden.status.success());
    let muted = Command::new(binary)
        .args(["set-track-muted", "--project"])
        .arg(&project_path)
        .args(["--track", second_track_id, "--muted", "true"])
        .output()?;
    assert!(muted.status.success());

    let inspected = Command::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    assert!(inspected.status.success());
    let project: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(project["sequences"][0]["tracks"][0]["id"], second_track_id);
    assert_eq!(project["sequences"][0]["tracks"][1]["id"], track_id);
    assert_eq!(project["sequences"][0]["tracks"][1]["locked"], true);
    assert_eq!(project["sequences"][0]["tracks"][1]["visible"], false);
    assert_eq!(project["sequences"][0]["tracks"][0]["muted"], true);

    fs::remove_file(project_path)?;
    fs::remove_file(request_path)?;
    fs::remove_dir(test_directory)?;
    Ok(())
}

#[test]
fn adds_and_deletes_sequence_markers() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("prism-cli-marker-test-{}", Uuid::new_v4()));
    fs::create_dir(&directory)?;
    let project_path = directory.join("markers.prism");
    let binary = env!("CARGO_BIN_EXE_prism-cli");
    let sequence_id = "11111111-1111-4111-8111-111111111111";
    let marker_id = "22222222-2222-4222-8222-222222222222";
    assert!(
        Command::new(binary)
            .args(["create-project", "--project"])
            .arg(&project_path)
            .args(["--name", "Marker test"])
            .output()?
            .status
            .success()
    );
    let request = serde_json::json!({"requestId":"sequence","apiVersion":"1","expectedRevision":0,"label":"Create sequence","dryRun":false,"operations":[{"type":"createSequence","id":sequence_id,"name":"Main"}]});
    let request_path = directory.join("sequence.json");
    fs::write(&request_path, serde_json::to_vec(&request)?)?;
    assert!(
        Command::new(binary)
            .args(["script-transaction", "--project"])
            .arg(&project_path)
            .args(["--request"])
            .arg(&request_path)
            .output()?
            .status
            .success()
    );
    assert!(
        Command::new(binary)
            .args(["add-marker", "--project"])
            .arg(&project_path)
            .args([
                "--sequence",
                sequence_id,
                "--marker",
                marker_id,
                "--time",
                "3/2",
                "--label",
                "Beat"
            ])
            .output()?
            .status
            .success()
    );
    let inspected = Command::new(binary)
        .args(["inspect", "--project"])
        .arg(&project_path)
        .output()?;
    let json: serde_json::Value = serde_json::from_slice(&inspected.stdout)?;
    assert_eq!(json["sequences"][0]["markers"][0]["label"], "Beat");
    assert!(
        Command::new(binary)
            .args(["delete-marker", "--project"])
            .arg(&project_path)
            .args(["--sequence", sequence_id, "--marker", marker_id])
            .output()?
            .status
            .success()
    );
    fs::remove_dir_all(directory)?;
    Ok(())
}
