use super::{
    AtomicBool, BasicMp4Exporter, CliError, CliOutcome, ClipId, Command, CommandEngine, Duration,
    ExternalScriptRunner, FfmpegProxyGenerator, MarkerId, OsString, Path, PathBuf, ProcessLimits,
    Project, ProxyProfile, ProxyRequest, RecipeExecutionPlan, RecipeLanguage, RecipeWorkerOutput,
    RuntimeBundleManifest, ScriptPermissionFile, ScriptWorker, SequenceId,
    SequenceMp4ExportRequest, Serialize, TrackId, TransitionAlignment, TransitionId,
    TransitionKind, TypeScriptCompiler, Uuid, list_autosaves, load, parse_bool, parse_exact_time,
    parse_u32, parse_usize, parse_uuid, read_permissions, recipe_export_runtime, recover_autosave,
    save_atomic, save_autosave, save_backup,
};

pub(super) fn run_set_track_visible(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_set_track_boolean(arguments, "set-track-visible", "--visible", true)
}

pub(super) fn run_set_track_muted(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    run_set_track_boolean(arguments, "set-track-muted", "--muted", false)
}

pub(super) fn run_set_track_boolean(
    arguments: &[OsString],
    command_name: &str,
    state_flag: &'static str,
    visible_state: bool,
) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        flag,
        value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != command_name
        || project_flag != "--project"
        || track_flag != "--track"
        || flag != state_flag
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let state = parse_bool(value, state_flag)?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .iter()
        .find(|(_, sequence)| sequence.tracks.iter().any(|track| track.id == track_id))
        .map(|(id, _)| *id)
        .ok_or_else(|| CliError::Command("track not found".to_owned()))?;
    let command = if visible_state {
        Command::SetTrackVisible {
            sequence_id,
            track_id,
            visible: state,
        }
    } else {
        Command::SetTrackMuted {
            sequence_id,
            track_id,
            muted: state,
        }
    };
    CommandEngine::execute(&mut project, command)
        .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    if visible_state {
        Ok(CliOutcome::TrackVisibilityChanged {
            path: project_path,
            track_id: track_id.as_uuid().to_string(),
            visible: state,
        })
    } else {
        Ok(CliOutcome::TrackMuteChanged {
            path: project_path,
            track_id: track_id.as_uuid().to_string(),
            muted: state,
        })
    }
}

pub(super) fn run_reorder_track(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        index_flag,
        index_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "reorder-track"
        || project_flag != "--project"
        || track_flag != "--track"
        || index_flag != "--index"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let destination_index = parse_usize(index_value, "--index")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .iter()
        .find(|(_, sequence)| sequence.tracks.iter().any(|track| track.id == track_id))
        .map(|(id, _)| *id)
        .ok_or_else(|| CliError::Command("track not found".to_owned()))?;
    CommandEngine::execute(
        &mut project,
        Command::ReorderTrack {
            sequence_id,
            track_id,
            destination_index,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TrackReordered {
        path: project_path,
        track_id: track_id.as_uuid().to_string(),
        destination_index,
    })
}

pub(super) fn run_add_marker(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        marker_flag,
        marker_value,
        time_flag,
        time_value,
        label_flag,
        label_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-marker"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || marker_flag != "--marker"
        || time_flag != "--time"
        || label_flag != "--label"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let marker_id = MarkerId::from_uuid(parse_uuid(marker_value.clone(), "--marker")?);
    let time = parse_exact_time(time_value.clone(), "--time")?;
    let label = label_value
        .to_str()
        .ok_or(CliError::NonUtf8Value("--label"))?
        .to_owned();
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::AddMarker {
            sequence_id,
            id: marker_id,
            time,
            label,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::MarkerAdded {
        path: project_path,
        marker_id: marker_id.as_uuid().to_string(),
    })
}

pub(super) fn run_add_transition(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        transition_flag,
        transition_value,
        outgoing_flag,
        outgoing_value,
        incoming_flag,
        incoming_value,
        duration_flag,
        duration_value,
        kind_flag,
        kind_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-transition"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || transition_flag != "--transition"
        || outgoing_flag != "--outgoing-clip"
        || incoming_flag != "--incoming-clip"
        || duration_flag != "--duration"
        || kind_flag != "--kind"
    {
        return Err(CliError::Usage);
    }
    if kind_value != "dissolve" {
        return Err(CliError::InvalidValue {
            option: "--kind",
            value: kind_value.to_string_lossy().into_owned(),
        });
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let transition_id =
        TransitionId::from_uuid(parse_uuid(transition_value.clone(), "--transition")?);
    let outgoing_clip = ClipId::from_uuid(parse_uuid(outgoing_value.clone(), "--outgoing-clip")?);
    let incoming_clip = ClipId::from_uuid(parse_uuid(incoming_value.clone(), "--incoming-clip")?);
    let duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::AddTransition {
            sequence_id,
            id: transition_id,
            outgoing_clip,
            incoming_clip,
            duration,
            alignment: TransitionAlignment::Centered,
            kind: TransitionKind::Dissolve,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TransitionAdded {
        path: project_path,
        transition_id: transition_id.as_uuid().to_string(),
    })
}

pub(super) fn run_delete_transition(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        transition_flag,
        transition_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "delete-transition"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || transition_flag != "--transition"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let transition_id =
        TransitionId::from_uuid(parse_uuid(transition_value.clone(), "--transition")?);
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::DeleteTransition {
            sequence_id,
            transition_id,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TransitionDeleted {
        path: project_path,
        transition_id: transition_id.as_uuid().to_string(),
    })
}

pub(super) fn run_generate_proxy(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let (source, output, width, height, video_codec, audio_codec, ffmpeg) = match arguments {
        [
            command,
            source_flag,
            source,
            output_flag,
            output,
            width_flag,
            width,
            height_flag,
            height,
            video_codec_flag,
            video_codec,
            ffmpeg_flag,
            ffmpeg,
        ] if command == "generate-proxy"
            && source_flag == "--source"
            && output_flag == "--output"
            && width_flag == "--width"
            && height_flag == "--height"
            && video_codec_flag == "--video-codec"
            && ffmpeg_flag == "--ffmpeg" =>
        {
            (
                PathBuf::from(source),
                PathBuf::from(output),
                parse_u32(width.clone(), "--width")?,
                parse_u32(height.clone(), "--height")?,
                video_codec
                    .clone()
                    .into_string()
                    .map_err(|_| CliError::NonUtf8Value("--video-codec"))?,
                None,
                PathBuf::from(ffmpeg),
            )
        }
        [
            command,
            source_flag,
            source,
            output_flag,
            output,
            width_flag,
            width,
            height_flag,
            height,
            video_codec_flag,
            video_codec,
            audio_codec_flag,
            audio_codec,
            ffmpeg_flag,
            ffmpeg,
        ] if command == "generate-proxy"
            && source_flag == "--source"
            && output_flag == "--output"
            && width_flag == "--width"
            && height_flag == "--height"
            && video_codec_flag == "--video-codec"
            && audio_codec_flag == "--audio-codec"
            && ffmpeg_flag == "--ffmpeg" =>
        {
            (
                PathBuf::from(source),
                PathBuf::from(output),
                parse_u32(width.clone(), "--width")?,
                parse_u32(height.clone(), "--height")?,
                video_codec
                    .clone()
                    .into_string()
                    .map_err(|_| CliError::NonUtf8Value("--video-codec"))?,
                Some(
                    audio_codec
                        .clone()
                        .into_string()
                        .map_err(|_| CliError::NonUtf8Value("--audio-codec"))?,
                ),
                PathBuf::from(ffmpeg),
            )
        }
        _ => return Err(CliError::Usage),
    };
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("proxy");
    let profile = ProxyProfile::new(width, height, video_codec, audio_codec, extension)?;
    let request = ProxyRequest {
        source,
        output: output.clone(),
        profile,
    };
    FfmpegProxyGenerator::new(ffmpeg).generate(&request, None)?;
    Ok(CliOutcome::ProxyGenerated { path: output })
}

pub(super) fn run_delete_marker(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        sequence_flag,
        sequence_value,
        marker_flag,
        marker_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "delete-marker"
        || project_flag != "--project"
        || sequence_flag != "--sequence"
        || marker_flag != "--marker"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let sequence_id = SequenceId::from_uuid(parse_uuid(sequence_value.clone(), "--sequence")?);
    let marker_id = MarkerId::from_uuid(parse_uuid(marker_value.clone(), "--marker")?);
    let mut project = load(&project_path)?;
    CommandEngine::execute(
        &mut project,
        Command::DeleteMarker {
            sequence_id,
            marker_id,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::MarkerDeleted {
        path: project_path,
        marker_id: marker_id.as_uuid().to_string(),
    })
}

pub(super) fn run_set_track_locked(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        locked_flag,
        locked_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "set-track-locked"
        || project_flag != "--project"
        || track_flag != "--track"
        || locked_flag != "--locked"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let locked_text = locked_value
        .to_str()
        .ok_or(CliError::NonUtf8Value("--locked"))?;
    let locked = match locked_text {
        "true" => true,
        "false" => false,
        value => {
            return Err(CliError::InvalidValue {
                option: "--locked",
                value: value.to_owned(),
            });
        }
    };
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .iter()
        .find(|(_, sequence)| sequence.tracks.iter().any(|track| track.id == track_id))
        .map(|(id, _)| *id)
        .ok_or_else(|| CliError::Command("track not found".to_owned()))?;
    CommandEngine::execute(
        &mut project,
        Command::SetTrackLocked {
            sequence_id,
            track_id,
            locked,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::TrackLockChanged {
        path: project_path,
        track_id: track_id.as_uuid().to_string(),
        locked,
    })
}

pub(super) fn run_validate_recipe(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let (manifest_path, inputs_path) = match arguments {
        [command, manifest_flag, manifest_path]
            if command == "validate-recipe" && manifest_flag == "--manifest" =>
        {
            (PathBuf::from(manifest_path), None)
        }
        [
            command,
            manifest_flag,
            manifest_path,
            inputs_flag,
            inputs_path,
        ] if command == "validate-recipe"
            && manifest_flag == "--manifest"
            && inputs_flag == "--inputs" =>
        {
            (
                PathBuf::from(manifest_path),
                Some(PathBuf::from(inputs_path)),
            )
        }
        _ => return Err(CliError::Usage),
    };
    let manifest: prism_recipe::RecipeManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|error| CliError::Command(error.to_string()))?,
    )?;
    let inputs = match inputs_path {
        Some(path) => serde_json::from_slice(
            &std::fs::read(path).map_err(|error| CliError::Command(error.to_string()))?,
        )?,
        None => serde_json::Map::new(),
    };
    let request = prism_script::RecipeRequest { manifest, inputs };
    let plan = request
        .into_execution_plan()
        .map_err(|error| CliError::Command(error.to_string()))?;
    Ok(CliOutcome::RecipeValidated {
        recipe_id: plan.manifest.id,
        version: plan.manifest.version,
        input_count: plan.inputs.len(),
    })
}

pub(super) fn run_recipe(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        manifest_flag,
        manifest_path,
        inputs_flag,
        inputs_path,
        permissions_flag,
        permissions_path,
        runtime_flag,
        runtime_path,
        remaining @ ..,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "run-recipe"
        || project_flag != "--project"
        || manifest_flag != "--manifest"
        || inputs_flag != "--inputs"
        || permissions_flag != "--permissions"
    {
        return Err(CliError::Usage);
    }

    let manifest_path = PathBuf::from(manifest_path);
    let manifest: prism_recipe::RecipeManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|error| CliError::Command(error.to_string()))?,
    )?;
    let inputs = serde_json::from_slice(
        &std::fs::read(inputs_path).map_err(|error| CliError::Command(error.to_string()))?,
    )?;
    let permissions = read_permissions(permissions_path)?;
    if permissions.requested != manifest.permissions {
        return Err(CliError::Command(
            "permission review does not match the recipe manifest".to_owned(),
        ));
    }
    let plan = (prism_script::RecipeRequest { manifest, inputs })
        .authorize_and_plan(&permissions.granted)
        .map_err(|error| CliError::Command(error.to_string()))?;
    let recipe_directory = manifest_path
        .parent()
        .ok_or_else(|| CliError::Command("recipe manifest has no parent directory".to_owned()))?;
    let script_path = recipe_directory.join(&plan.manifest.entry);
    if !script_path.is_file() {
        return Err(CliError::Command(format!(
            "recipe entry does not exist: {}",
            script_path.display()
        )));
    }

    execute_recipe_plan(
        &PathBuf::from(project_path),
        &plan,
        &permissions,
        runtime_flag,
        runtime_path,
        remaining,
        &script_path,
    )
}

pub(super) fn execute_recipe_plan(
    project_path: &Path,
    plan: &RecipeExecutionPlan,
    permissions: &ScriptPermissionFile,
    runtime_flag: &OsString,
    runtime_path: &OsString,
    remaining: &[OsString],
    script_path: &Path,
) -> Result<CliOutcome, CliError> {
    let mut project = if project_path.exists() {
        load(project_path)?
    } else {
        Project::new(&plan.manifest.id)
    };
    let revision = project.revision().to_string();
    let input_json = serde_json::to_string(&plan.inputs)?;
    let cancelled = AtomicBool::new(false);
    let limits = ProcessLimits {
        timeout: Duration::from_secs(30),
        max_output_bytes: 8 * 1024 * 1024,
    };
    let output_directory = std::env::temp_dir().join(format!("prism-recipe-{}", Uuid::new_v4()));
    let result = (|| {
        let (interpreter, compiler) = resolve_recipe_runtime(
            plan.manifest.language,
            runtime_flag,
            runtime_path,
            remaining,
        )?;
        let executable_script = match compiler {
            Some(compiler) => TypeScriptCompiler::new(compiler, limits).compile(
                script_path,
                &output_directory,
                &plan.manifest.permissions,
                &permissions.granted,
                &cancelled,
            )?,
            None => script_path.to_path_buf(),
        };
        let output = ExternalScriptRunner::new(interpreter, limits).run_authorized(
            &executable_script,
            &[],
            &[
                ("PRISM_PROJECT_REVISION", revision),
                ("PRISM_RECIPE_INPUTS", input_json),
            ],
            &plan.manifest.permissions,
            &permissions.granted,
            &cancelled,
        )?;
        let worker_output: RecipeWorkerOutput = serde_json::from_slice(&output.stdout)?;
        let (request, export) = match worker_output {
            RecipeWorkerOutput::Transaction(request) => (request, None),
            RecipeWorkerOutput::Complete(output) => (output.transaction, Some(output.export)),
        };
        let export_runtime = recipe_export_runtime(export.as_ref(), &request, permissions)?;
        let execution = ScriptWorker::default()
            .execute_authorized(&mut project, request, &cancelled, &permissions.granted)
            .map_err(|error| CliError::Command(error.to_string()))?;
        if !execution.dry_run {
            save_atomic(&project, project_path)?;
        }
        if let (Some(export), Some((ffmpeg, ffprobe, preset))) = (export, export_runtime) {
            return Ok(CliOutcome::Exported(
                BasicMp4Exporter::new(ffmpeg, ffprobe).export_sequence(
                    &project,
                    SequenceId::from_uuid(export.sequence_id),
                    &SequenceMp4ExportRequest::from_preset(preset, export.output),
                )?,
            ));
        }
        Ok(CliOutcome::ScriptTransaction {
            path: project_path.to_path_buf(),
            revision: execution.revision,
            dry_run: execution.dry_run,
            event_count: execution.event_count,
        })
    })();
    if plan.manifest.language == RecipeLanguage::TypeScript {
        let _ = std::fs::remove_dir_all(output_directory);
    }
    result
}

pub(super) fn resolve_recipe_runtime(
    language: RecipeLanguage,
    runtime_flag: &OsString,
    runtime_path: &OsString,
    remaining: &[OsString],
) -> Result<(PathBuf, Option<PathBuf>), CliError> {
    match language {
        RecipeLanguage::Python if runtime_flag == "--python" && remaining.is_empty() => {
            Ok((PathBuf::from(runtime_path), None))
        }
        RecipeLanguage::JavaScript if runtime_flag == "--node" && remaining.is_empty() => {
            Ok((PathBuf::from(runtime_path), None))
        }
        RecipeLanguage::TypeScript
            if runtime_flag == "--tsc" && remaining.len() == 2 && remaining[0] == "--node" =>
        {
            Ok((
                PathBuf::from(&remaining[1]),
                Some(PathBuf::from(runtime_path)),
            ))
        }
        _ if runtime_flag == "--runtime-bundle" && remaining.is_empty() => {
            let manifest_path = PathBuf::from(runtime_path);
            let manifest: RuntimeBundleManifest = serde_json::from_slice(
                &std::fs::read(&manifest_path)
                    .map_err(|error| CliError::Command(error.to_string()))?,
            )?;
            let directory = manifest_path.parent().ok_or_else(|| {
                CliError::Command("runtime bundle manifest has no parent directory".to_owned())
            })?;
            let bundle = manifest
                .resolve(directory)
                .map_err(|error| CliError::Command(error.to_string()))?;
            let (interpreter, compiler) = bundle
                .for_language(language)
                .map_err(|error| CliError::Command(error.to_string()))?;
            Ok((interpreter.to_path_buf(), compiler.map(Path::to_path_buf)))
        }
        _ => Err(CliError::Usage),
    }
}

pub(super) fn run_backup_project(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        output_flag,
        output_path,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "backup-project" || project_flag != "--project" || output_flag != "--output" {
        return Err(CliError::Usage);
    }
    let project = load(PathBuf::from(project_path).as_path())?;
    let revision = project.revision();
    let output_path = PathBuf::from(output_path);
    save_backup(&project, &output_path)?;
    Ok(CliOutcome::BackupCreated {
        path: output_path,
        revision,
    })
}

pub(super) fn run_autosave_project(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [command, project_flag, project_path] = arguments else {
        return Err(CliError::Usage);
    };
    if command != "autosave-project" || project_flag != "--project" {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let project = load(&project_path)?;
    let revision = project.revision();
    let path = save_autosave(&project, &project_path)?;
    Ok(CliOutcome::AutosaveCreated { path, revision })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AutosaveSummary {
    revision: u64,
    path: String,
}

pub(super) fn run_list_autosaves(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [command, project_flag, project_path] = arguments else {
        return Err(CliError::Usage);
    };
    if command != "list-autosaves" || project_flag != "--project" {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let candidates = list_autosaves(&project_path)?;
    let summaries = candidates
        .into_iter()
        .map(|candidate| AutosaveSummary {
            revision: candidate.revision,
            path: candidate.path.to_string_lossy().into_owned(),
        })
        .collect::<Vec<_>>();
    Ok(CliOutcome::AutosavesListed {
        json: serde_json::to_string_pretty(&summaries)?,
    })
}

pub(super) fn run_recover_autosave(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [command, source_flag, source_path, output_flag, output_path] = arguments else {
        return Err(CliError::Usage);
    };
    if command != "recover-autosave" || source_flag != "--source" || output_flag != "--output" {
        return Err(CliError::Usage);
    }
    let output = PathBuf::from(output_path);
    let revision = recover_autosave(PathBuf::from(source_path).as_path(), &output)?;
    Ok(CliOutcome::AutosaveRecovered {
        path: output,
        revision,
    })
}
