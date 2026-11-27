use super::{
    AtomicBool, CliError, CliOutcome, Duration, ExternalScriptRunner, OsString, PathBuf,
    ProcessLimits, ScriptPermissionFile, ScriptWorker, TransactionRequest, TypeScriptCompiler,
    Uuid, load, read_permissions, save_atomic,
};

pub(super) fn run_script_transaction(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        request_flag,
        request_path,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "script-transaction" || project_flag != "--project" || request_flag != "--request"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let request_path = PathBuf::from(request_path);
    let payload =
        std::fs::read(&request_path).map_err(|error| CliError::Command(error.to_string()))?;
    let mut project = load(&project_path)?;
    let result = prism_script::ScriptWorker::default()
        .execute_json(&mut project, &payload, &AtomicBool::new(false))
        .map_err(|error| CliError::Command(error.to_string()))?;
    if !result.dry_run {
        save_atomic(&project, &project_path)?;
    }
    Ok(CliOutcome::ScriptTransaction {
        path: project_path,
        revision: result.revision,
        dry_run: result.dry_run,
        event_count: result.event_count,
    })
}

pub(super) fn run_python_script(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_language_script(arguments, "run-python-script", "--python")
}

pub(super) fn run_javascript_script(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_language_script(arguments, "run-javascript-script", "--node")
}

pub(super) fn run_typescript_script(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        script_flag,
        script_path,
        compiler_flag,
        compiler_path,
        node_flag,
        node_path,
        permissions_flag,
        permissions_path,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "run-typescript-script"
        || project_flag != "--project"
        || script_flag != "--script"
        || compiler_flag != "--tsc"
        || node_flag != "--node"
        || permissions_flag != "--permissions"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let script_path = PathBuf::from(script_path);
    let permissions = read_permissions(permissions_path)?;
    let mut project = load(&project_path)?;
    let revision = project.revision().to_string();
    let cancelled = AtomicBool::new(false);
    let output_directory =
        std::env::temp_dir().join(format!("prism-typescript-{}", Uuid::new_v4()));
    let result = (|| {
        let compiler = TypeScriptCompiler::new(
            PathBuf::from(compiler_path),
            ProcessLimits {
                timeout: Duration::from_secs(30),
                max_output_bytes: 8 * 1024 * 1024,
            },
        );
        let compiled_path = compiler.compile(
            &script_path,
            &output_directory,
            &permissions.requested,
            &permissions.granted,
            &cancelled,
        )?;
        let runner = ExternalScriptRunner::new(
            PathBuf::from(node_path),
            ProcessLimits {
                timeout: Duration::from_secs(30),
                max_output_bytes: 8 * 1024 * 1024,
            },
        );
        let output = runner.run_authorized(
            &compiled_path,
            &[],
            &[("PRISM_PROJECT_REVISION", revision)],
            &permissions.requested,
            &permissions.granted,
            &cancelled,
        )?;
        let request: TransactionRequest = serde_json::from_slice(&output.stdout)?;
        let execution = ScriptWorker::default()
            .execute_authorized(&mut project, request, &cancelled, &permissions.granted)
            .map_err(|error| CliError::Command(error.to_string()))?;
        if !execution.dry_run {
            save_atomic(&project, &project_path)?;
        }
        Ok::<_, CliError>(CliOutcome::ScriptTransaction {
            path: project_path.clone(),
            revision: execution.revision,
            dry_run: execution.dry_run,
            event_count: execution.event_count,
        })
    })();
    let _ = std::fs::remove_dir_all(&output_directory);
    result
}

pub(super) fn run_language_script(
    arguments: &[OsString],
    command_name: &str,
    interpreter_flag: &str,
) -> Result<CliOutcome, CliError> {
    let (project_path, script_path, interpreter_path, permission_path) = match arguments {
        [
            command,
            project_flag,
            project_path,
            script_flag,
            script_path,
            interpreter_flag_arg,
            interpreter_path,
        ] if command == command_name
            && project_flag == "--project"
            && script_flag == "--script"
            && interpreter_flag_arg == interpreter_flag =>
        {
            (project_path, script_path, interpreter_path, None)
        }
        [
            command,
            project_flag,
            project_path,
            script_flag,
            script_path,
            interpreter_flag_arg,
            interpreter_path,
            permissions_flag,
            permissions_path,
        ] if command == command_name
            && project_flag == "--project"
            && script_flag == "--script"
            && interpreter_flag_arg == interpreter_flag
            && permissions_flag == "--permissions" =>
        {
            (
                project_path,
                script_path,
                interpreter_path,
                Some(PathBuf::from(permissions_path)),
            )
        }
        _ => return Err(CliError::Usage),
    };
    let project_path = PathBuf::from(project_path);
    let script_path = PathBuf::from(script_path);
    let mut project = load(&project_path)?;
    let revision = project.revision().to_string();
    let permissions = match permission_path {
        Some(path) => read_permissions(path)?,
        None => ScriptPermissionFile {
            requested: Vec::new(),
            granted: Vec::new(),
        },
    };
    let runner = ExternalScriptRunner::new(
        PathBuf::from(interpreter_path),
        ProcessLimits {
            timeout: Duration::from_secs(30),
            max_output_bytes: 8 * 1024 * 1024,
        },
    );
    let cancelled = AtomicBool::new(false);
    let output = runner.run_authorized(
        &script_path,
        &[],
        &[("PRISM_PROJECT_REVISION", revision)],
        &permissions.requested,
        &permissions.granted,
        &cancelled,
    )?;
    let request: TransactionRequest = serde_json::from_slice(&output.stdout)?;
    let result = ScriptWorker::default()
        .execute_authorized(&mut project, request, &cancelled, &permissions.granted)
        .map_err(|error| CliError::Command(error.to_string()))?;
    if !result.dry_run {
        save_atomic(&project, &project_path)?;
    }
    Ok(CliOutcome::ScriptTransaction {
        path: project_path,
        revision: result.revision,
        dry_run: result.dry_run,
        event_count: result.event_count,
    })
}
