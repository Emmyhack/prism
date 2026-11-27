use super::{
    API_VERSION, ExecutionError, ExecutionLimits, ExternalScriptRunner, PermissionGrantError,
    PermissionGrantSet, ProcessError, ProcessLimits, ProtocolError, RecipePermission,
    RecipeRequest, Request, RuntimeBundleError, RuntimeBundleManifest, ScriptWorker,
    TransactionDebugger, TransactionRequest, TypeScriptCompiler,
};
use prism_project::Project;
use std::sync::atomic::AtomicBool;
use uuid::Uuid;

#[test]
fn rejects_unknown_api_version_before_command_execution() -> Result<(), serde_json::Error> {
    let json = r#"{
            "requestId":"req-1",
            "apiVersion":"999",
            "operation":{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}
        }"#;
    let request: Request = serde_json::from_str(json)?;
    assert!(matches!(
        request.into_command(),
        Err(ProtocolError::UnsupportedApiVersion { requested }) if requested == "999"
    ));
    Ok(())
}

#[test]
fn accepts_current_api_version() -> Result<(), serde_json::Error> {
    let json = format!(
        r#"{{"requestId":"req-1","apiVersion":"{API_VERSION}","operation":{{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}}}}"#
    );
    let request: Request = serde_json::from_str(&json)?;
    assert!(request.into_command().is_ok());
    Ok(())
}

#[test]
fn converts_atomic_batch_and_preserves_dry_run() -> Result<(), Box<dyn std::error::Error>> {
    let json = format!(
        r#"{{
                "requestId":"req-2",
                "apiVersion":"{API_VERSION}",
                "expectedRevision":7,
                "label":"Prepare timeline",
                "dryRun":true,
                "operations":[
                    {{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}},
                    {{"type":"createSequence","id":"00000000-0000-0000-0000-000000000002","name":"Social"}}
                ]
            }}"#
    );
    let request: TransactionRequest = serde_json::from_str(&json)?;
    let transaction = request.into_transaction()?;
    assert!(transaction.dry_run);
    assert_eq!(transaction.expected_revision, 7);
    assert_eq!(transaction.label, "Prepare timeline");
    assert_eq!(transaction.commands.len(), 2);
    Ok(())
}

#[test]
fn converts_track_reorder_operation() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_value(serde_json::json!({
        "requestId": "reorder",
        "apiVersion": API_VERSION,
        "operation": {
            "type": "reorderTrack",
            "sequenceId": "00000000-0000-0000-0000-000000000001",
            "trackId": "00000000-0000-0000-0000-000000000002",
            "destinationIndex": 1
        }
    }))?;
    assert!(request.into_command().is_ok());
    Ok(())
}

#[test]
fn converts_track_visibility_and_mute_operations() -> Result<(), Box<dyn std::error::Error>> {
    for operation in [
        serde_json::json!({"type": "setTrackVisible", "sequenceId": "00000000-0000-0000-0000-000000000001", "trackId": "00000000-0000-0000-0000-000000000002", "visible": false}),
        serde_json::json!({"type": "setTrackMuted", "sequenceId": "00000000-0000-0000-0000-000000000001", "trackId": "00000000-0000-0000-0000-000000000002", "muted": false}),
    ] {
        let request: Request = serde_json::from_value(serde_json::json!({
            "requestId": "track-state",
            "apiVersion": API_VERSION,
            "operation": operation
        }))?;
        assert!(request.into_command().is_ok());
    }
    Ok(())
}

#[test]
fn converts_marker_operations() -> Result<(), Box<dyn std::error::Error>> {
    for operation in [
        serde_json::json!({"type": "addMarker", "sequenceId": "00000000-0000-0000-0000-000000000001", "id": "00000000-0000-0000-0000-000000000002", "time": {"value": 3, "timescale": 2}, "label": "Beat"}),
        serde_json::json!({"type": "deleteMarker", "sequenceId": "00000000-0000-0000-0000-000000000001", "markerId": "00000000-0000-0000-0000-000000000002"}),
    ] {
        let request: Request = serde_json::from_value(serde_json::json!({
            "requestId": "marker",
            "apiVersion": API_VERSION,
            "operation": operation
        }))?;
        assert!(request.into_command().is_ok());
    }
    Ok(())
}

#[test]
fn converts_duplicate_clip_operation() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_value(serde_json::json!({
        "requestId": "duplicate",
        "apiVersion": API_VERSION,
        "operation": {
            "type": "duplicateClip",
            "sequenceId": "00000000-0000-0000-0000-000000000001",
            "trackId": "00000000-0000-0000-0000-000000000002",
            "clipId": "00000000-0000-0000-0000-000000000003",
            "newClipId": "00000000-0000-0000-0000-000000000004",
            "timelineStart": {"value": 4, "timescale": 1}
        }
    }))?;
    assert!(request.into_command().is_ok());
    Ok(())
}

#[test]
fn converts_ripple_delete_operation() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_value(serde_json::json!({
        "requestId": "ripple",
        "apiVersion": API_VERSION,
        "operation": {
            "type": "rippleDelete",
            "sequenceId": "00000000-0000-0000-0000-000000000001",
            "trackId": "00000000-0000-0000-0000-000000000002",
            "clipId": "00000000-0000-0000-0000-000000000003"
        }
    }))?;
    assert!(request.into_command().is_ok());
    Ok(())
}

#[test]
fn converts_transition_operations() -> Result<(), Box<dyn std::error::Error>> {
    for operation in [
        serde_json::json!({"type":"addTransition","sequenceId":"00000000-0000-0000-0000-000000000001","id":"00000000-0000-0000-0000-000000000002","outgoingClip":"00000000-0000-0000-0000-000000000003","incomingClip":"00000000-0000-0000-0000-000000000004","duration":{"value":1,"timescale":2},"kind":"dissolve"}),
        serde_json::json!({"type":"deleteTransition","sequenceId":"00000000-0000-0000-0000-000000000001","transitionId":"00000000-0000-0000-0000-000000000002"}),
    ] {
        let request: Request = serde_json::from_value(
            serde_json::json!({"requestId":"transition","apiVersion":API_VERSION,"operation":operation}),
        )?;
        assert!(request.into_command().is_ok());
    }
    Ok(())
}

#[test]
fn worker_honors_limits_cancellation_and_dry_run() -> Result<(), Box<dyn std::error::Error>> {
    let request = serde_json::json!({
        "requestId": "worker-test", "apiVersion": API_VERSION,
        "expectedRevision": 0, "label": "Create sequence", "dryRun": true,
        "operations": [{"type":"createSequence","id":"00000000-0000-0000-0000-000000000001","name":"Main"}]
    });
    let payload = serde_json::to_vec(&request)?;
    let worker = ScriptWorker {
        limits: ExecutionLimits {
            max_operations: 1,
            max_payload_bytes: 4096,
        },
    };
    let mut project = Project::new("Worker");
    let cancelled = AtomicBool::new(false);
    let result = worker.execute_json(&mut project, &payload, &cancelled)?;
    assert!(result.dry_run);
    assert_eq!(result.event_count, 1);
    assert_eq!(project.revision(), 0);
    let stale_request = serde_json::json!({
        "requestId": "worker-stale", "apiVersion": API_VERSION,
        "expectedRevision": 1, "label": "Create sequence", "dryRun": true,
        "operations": [{"type":"createSequence","id":"00000000-0000-0000-0000-000000000002","name":"Other"}]
    });
    let stale_payload = serde_json::to_vec(&stale_request)?;
    assert!(matches!(
        worker.execute_json(&mut project, &stale_payload, &AtomicBool::new(false)),
        Err(ExecutionError::Command(message)) if message.contains("revision conflict")
    ));
    cancelled.store(true, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        worker.execute_json(&mut project, &payload, &cancelled),
        Err(ExecutionError::Cancelled)
    ));
    let limited = ScriptWorker {
        limits: ExecutionLimits {
            max_operations: 0,
            max_payload_bytes: payload.len(),
        },
    };
    cancelled.store(false, std::sync::atomic::Ordering::Release);
    assert!(matches!(
        limited.execute_json(&mut project, &payload, &cancelled),
        Err(ExecutionError::TooManyOperations { limit: 0 })
    ));
    Ok(())
}

#[test]
fn rejects_zero_timescale_at_protocol_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let json = format!(
        r#"{{
                "requestId":"req-3",
                "apiVersion":"{API_VERSION}",
                "operation":{{
                    "type":"moveClip",
                    "sequenceId":"00000000-0000-0000-0000-000000000001",
                    "trackId":"00000000-0000-0000-0000-000000000002",
                    "clipId":"00000000-0000-0000-0000-000000000003",
                    "timelineStart":{{"value":1,"timescale":0}}
                }}
            }}"#
    );
    let request: Request = serde_json::from_str(&json)?;
    assert!(matches!(
        request.into_command(),
        Err(ProtocolError::InvalidTimebase)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn external_runner_captures_output_and_enforces_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    let runner = ExternalScriptRunner::new(
        "/bin/sh",
        ProcessLimits {
            timeout: std::time::Duration::from_secs(2),
            max_output_bytes: 128,
        },
    );
    let cancelled = AtomicBool::new(false);
    let output = runner.run(
        PathBuf::from("-c").as_path(),
        &[PathBuf::from("printf prism")],
        &cancelled,
    )?;
    assert_eq!(output.stdout, b"prism");
    assert!(output.stderr.is_empty());

    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        runner.run(
            PathBuf::from("-c").as_path(),
            &[PathBuf::from("printf never")],
            &cancelled,
        ),
        Err(ProcessError::Cancelled)
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn external_runner_rejects_unbounded_output() {
    let runner = ExternalScriptRunner::new(
        "/bin/sh",
        ProcessLimits {
            timeout: std::time::Duration::from_secs(2),
            max_output_bytes: 4,
        },
    );
    let cancelled = AtomicBool::new(false);
    assert!(matches!(
        runner.run(
            std::path::Path::new("-c"),
            &[std::path::PathBuf::from("printf 12345")],
            &cancelled,
        ),
        Err(ProcessError::OutputTooLarge {
            limit: 4,
            stream: "stdout"
        })
    ));
}

#[test]
fn external_runner_denies_capability_before_spawn() {
    let runner = ExternalScriptRunner::new("/path/that/does/not/exist", ProcessLimits::default());
    let cancelled = AtomicBool::new(false);
    assert!(matches!(
        runner.run_authorized(
            std::path::Path::new("script.py"),
            &[],
            &[],
            &[RecipePermission::NetworkAccess],
            &[],
            &cancelled,
        ),
        Err(ProcessError::PermissionDenied {
            permission: RecipePermission::NetworkAccess
        })
    ));
}

#[cfg(unix)]
#[test]
fn typescript_compiler_uses_explicit_output_directory() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("prism-ts-{}", Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let compiler_path = root.join("tsc");
    let source = root.join("main.ts");
    let output = root.join("out");
    std::fs::write(
        &compiler_path,
        "#!/bin/sh\nout=''\nsrc=''\nprevious=''\nfor arg in \"$@\"; do if [ \"$previous\" = \"--outDir\" ]; then out=\"$arg\"; fi; src=\"$arg\"; previous=\"$arg\"; done\nbase=$(basename \"$src\" .ts)\nprintf '%s' 'console.log(\"compiled\")' > \"$out/$base.js\"\nprintf '%s' '{\"version\":3}' > \"$out/$base.js.map\"\n",
    )?;
    std::fs::set_permissions(&compiler_path, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(&source, "const value: number = 1;\n")?;
    let compiled_path = TypeScriptCompiler::new(&compiler_path, ProcessLimits::default()).compile(
        &source,
        &output,
        &[],
        &[],
        &AtomicBool::new(false),
    )?;
    assert_eq!(compiled_path, output.join("main.js"));
    assert!(compiled_path.is_file());
    let (compiled_path, source_map) = TypeScriptCompiler::new(
        &compiler_path,
        ProcessLimits::default(),
    )
    .compile_with_source_map(&source, &output, &[], &[], &AtomicBool::new(false))?;
    assert_eq!(compiled_path, output.join("main.js"));
    assert_eq!(source_map, output.join("main.js.map"));
    assert!(source_map.is_file());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn checked_in_transaction_example_matches_protocol() -> Result<(), Box<dyn std::error::Error>> {
    let request: TransactionRequest = serde_json::from_str(include_str!(
        "../../../examples/script-transaction-dry-run.json"
    ))?;
    let transaction = request.into_transaction()?;
    assert_eq!(transaction.expected_revision, 1);
    assert!(transaction.dry_run);
    assert_eq!(transaction.commands.len(), 1);
    Ok(())
}

#[test]
fn recipe_request_becomes_a_validated_execution_plan() -> Result<(), Box<dyn std::error::Error>> {
    let request: RecipeRequest = serde_json::from_value(serde_json::json!({
        "manifest": {
            "id": "prism.example.recipe",
            "version": "1.0.0",
            "apiVersion": 1,
            "language": "typescript",
            "entry": "main.ts",
            "permissions": ["project.read"],
            "parameters": {
                "clipCount": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "default": 5
                }
            }
        },
        "inputs": {"clipCount": 3}
    }))?;
    let plan = request.into_execution_plan()?;
    assert_eq!(plan.manifest.id, "prism.example.recipe");
    assert_eq!(plan.inputs["clipCount"], serde_json::json!(3));
    let authorized_request = RecipeRequest {
        manifest: plan.manifest.clone(),
        inputs: plan.inputs.clone(),
    };
    assert!(matches!(
        authorized_request.authorize_and_plan(&[]),
        Err(ProtocolError::InvalidRecipe(_))
    ));
    let authorized_request = RecipeRequest {
        manifest: plan.manifest,
        inputs: plan.inputs,
    };
    assert!(
        authorized_request
            .authorize_and_plan(&[RecipePermission::ProjectRead])
            .is_ok()
    );
    Ok(())
}

#[test]
fn recipe_execution_plan_materializes_declared_defaults() -> Result<(), Box<dyn std::error::Error>>
{
    let request: RecipeRequest = serde_json::from_value(serde_json::json!({
        "manifest": {
            "id": "prism.example.defaults",
            "version": "1.0.0",
            "apiVersion": 1,
            "language": "python",
            "entry": "main.py",
            "permissions": [],
            "parameters": {
                "clipCount": {
                    "type": "integer",
                    "minimum": 1,
                    "default": 5
                },
                "label": {
                    "type": "string",
                    "default": "Review"
                }
            }
        },
        "inputs": {}
    }))?;
    let plan = request.into_execution_plan()?;
    assert_eq!(plan.inputs["clipCount"], serde_json::json!(5));
    assert_eq!(plan.inputs["label"], serde_json::json!("Review"));
    Ok(())
}

#[test]
fn invalid_recipe_inputs_are_rejected_before_planning() -> Result<(), Box<dyn std::error::Error>> {
    let request: RecipeRequest = serde_json::from_value(serde_json::json!({
        "manifest": {
            "id": "prism.example.recipe",
            "version": "1.0.0",
            "apiVersion": 1,
            "language": "python",
            "entry": "main.py",
            "permissions": [],
            "parameters": {}
        },
        "inputs": {"unexpected": true}
    }))?;
    assert!(matches!(
        request.into_execution_plan(),
        Err(ProtocolError::InvalidRecipe(_))
    ));
    Ok(())
}

#[test]
fn permission_reviews_normalize_and_reject_undeclared_grants()
-> Result<(), Box<dyn std::error::Error>> {
    let review = PermissionGrantSet {
        requested: vec![
            RecipePermission::ProjectWrite,
            RecipePermission::ProjectRead,
            RecipePermission::ProjectWrite,
        ],
        granted: vec![RecipePermission::ProjectRead],
    }
    .normalize()?;
    assert_eq!(
        review.requested,
        vec![
            RecipePermission::ProjectRead,
            RecipePermission::ProjectWrite
        ]
    );
    assert_eq!(review.granted, vec![RecipePermission::ProjectRead]);

    assert_eq!(
        PermissionGrantSet {
            requested: vec![RecipePermission::ProjectRead],
            granted: vec![RecipePermission::NetworkAccess],
        }
        .normalize(),
        Err(PermissionGrantError::UndeclaredGrant {
            permission: RecipePermission::NetworkAccess
        })
    );
    Ok(())
}

#[test]
fn runtime_bundle_resolves_only_contained_existing_components()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("prism-runtime-bundle-{}", Uuid::new_v4()));
    std::fs::create_dir_all(root.join("bin"))?;
    std::fs::write(root.join("bin/python"), b"runtime")?;
    let bundle = RuntimeBundleManifest {
        api_version: 1,
        version: "1.0.0".to_owned(),
        python: Some("bin/python".into()),
        node: None,
        typescript_compiler: None,
    }
    .resolve(&root)?;
    assert_eq!(bundle.python, Some(root.join("bin/python")));

    let unsafe_manifest = RuntimeBundleManifest {
        api_version: 1,
        version: "1.0.0".to_owned(),
        python: Some("../python".into()),
        node: None,
        typescript_compiler: None,
    };
    assert!(matches!(
        unsafe_manifest.resolve(&root),
        Err(RuntimeBundleError::UnsafePath(_))
    ));
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn transaction_debugger_steps_between_validated_breakpoints()
-> Result<(), Box<dyn std::error::Error>> {
    let project = Project::new("Debug");
    let request: TransactionRequest = serde_json::from_value(serde_json::json!({
        "requestId": "debug-session",
        "apiVersion": "1",
        "expectedRevision": 0,
        "label": "Debug plan",
        "dryRun": true,
        "operations": [
            {
                "type": "createSequence",
                "id": "11111111-1111-4111-8111-111111111111",
                "name": "One"
            },
            {
                "type": "createSequence",
                "id": "22222222-2222-4222-8222-222222222222",
                "name": "Two"
            }
        ]
    }))?;
    let mut debugger = TransactionDebugger::start(&project, request, &[1])?;
    assert_eq!(debugger.snapshot().current.map(|step| step.index), Some(0));
    let paused = debugger.continue_to_breakpoint();
    assert_eq!(paused.current.map(|step| step.index), Some(1));
    assert!(!paused.complete);
    assert!(debugger.step().complete);
    assert_eq!(project.revision(), 0);
    Ok(())
}
