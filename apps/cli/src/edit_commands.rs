use super::{
    CliError, CliOutcome, ClipId, Command, CommandEngine, FfprobeInspector, MediaAssetId, OsString,
    PathBuf, Project, RationalTime, SequenceId, StreamKind, TrackId, TrackKind, load,
    parse_exact_time, parse_uuid, save_atomic,
};

pub(super) fn run_delete_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "delete-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute_transaction(
        &mut project,
        "Delete video clip",
        vec![Command::DeleteClip {
            sequence_id,
            track_id,
            clip_id,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipDeleted {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

pub(super) fn run_ripple_delete_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "ripple-delete-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute(
        &mut project,
        Command::RippleDelete {
            sequence_id,
            track_id,
            clip_id,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipRippleDeleted {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

pub(super) fn run_duplicate_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        new_flag,
        new_value,
        timeline_flag,
        timeline_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "duplicate-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || new_flag != "--new-clip"
        || timeline_flag != "--timeline-start"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let new_clip_id = ClipId::from_uuid(parse_uuid(new_value.clone(), "--new-clip")?);
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute(
        &mut project,
        Command::DuplicateClip {
            sequence_id,
            track_id,
            clip_id,
            new_clip_id,
            timeline_start,
        },
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipDuplicated {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
        new_clip_id: new_clip_id.as_uuid().to_string(),
    })
}

pub(super) fn run_split_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        position_flag,
        position_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "split-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || position_flag != "--timeline-position"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let timeline_position = parse_exact_time(position_value.clone(), "--timeline-position")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    let right_clip_id = ClipId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Split video clip",
        vec![Command::SplitClip {
            sequence_id,
            track_id,
            clip_id,
            right_clip_id,
            timeline_position,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipSplit {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
        right_clip_id: right_clip_id.as_uuid().to_string(),
    })
}

pub(super) fn run_trim_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        timeline_flag,
        timeline_value,
        source_flag,
        source_value,
        duration_flag,
        duration_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "trim-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || timeline_flag != "--timeline-start"
        || source_flag != "--source-start"
        || duration_flag != "--duration"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let source_start = parse_exact_time(source_value.clone(), "--source-start")?;
    let source_duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute_transaction(
        &mut project,
        "Trim video clip",
        vec![Command::TrimClip {
            sequence_id,
            track_id,
            clip_id,
            timeline_start,
            source_start,
            source_duration,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipTrimmed {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

pub(super) fn run_move_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        clip_flag,
        clip_value,
        timeline_flag,
        timeline_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "move-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || clip_flag != "--clip"
        || timeline_flag != "--timeline-start"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let clip_id = ClipId::from_uuid(parse_uuid(clip_value.clone(), "--clip")?);
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence.tracks.iter().any(|track| {
                track.id == track_id
                    && track.kind == TrackKind::Video
                    && track.clips.iter().any(|clip| clip.id == clip_id)
            })
        })
        .ok_or_else(|| CliError::Command("video track or clip not found".to_owned()))?
        .id;
    CommandEngine::execute_transaction(
        &mut project,
        "Move video clip",
        vec![Command::MoveClip {
            sequence_id,
            track_id,
            clip_id,
            timeline_start,
        }],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipMoved {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
        timeline_start,
    })
}

pub(super) fn run_add_video_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        media_flag,
        media_value,
        source_flag,
        source_value,
        timeline_flag,
        timeline_value,
        duration_flag,
        duration_value,
        probe_flag,
        probe_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-video-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || media_flag != "--media"
        || source_flag != "--source-start"
        || timeline_flag != "--timeline-start"
        || duration_flag != "--duration"
        || probe_flag != "--ffprobe"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let media_path = PathBuf::from(media_value);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let source_start = parse_exact_time(source_value.clone(), "--source-start")?;
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let metadata = FfprobeInspector::new(probe_value).inspect(&media_path)?;
    if !metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
    {
        return Err(CliError::NoVideoStream);
    }
    let requested_end = source_start.checked_add(duration)?;
    if !source_start.is_non_negative()
        || !duration.is_positive()
        || metadata
            .duration
            .is_none_or(|available| requested_end > available)
    {
        return Err(CliError::SourceRangeOutOfBounds);
    }
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence
                .tracks
                .iter()
                .any(|track| track.id == track_id && track.kind == TrackKind::Video)
        })
        .ok_or_else(|| CliError::Command("video track not found".to_owned()))?
        .id;
    let source_path = media_path.to_string_lossy().into_owned();
    let existing_media = project
        .media
        .values()
        .find(|asset| asset.source_path == source_path)
        .map(|asset| asset.id);
    let media_id = existing_media.unwrap_or_else(MediaAssetId::new);
    let clip_id = ClipId::new();
    let mut commands = Vec::with_capacity(2);
    if existing_media.is_none() {
        commands.push(Command::ImportMedia {
            id: media_id,
            source_path,
        });
    }
    commands.push(Command::InsertClip {
        sequence_id,
        track_id,
        id: clip_id,
        media_id,
        timeline_start,
        source_start,
        source_duration: duration,
    });
    CommandEngine::execute_transaction(&mut project, "Add video clip", commands)
        .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipAdded {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

pub(super) fn run_add_audio_clip(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        track_flag,
        track_value,
        media_flag,
        media_value,
        source_flag,
        source_value,
        timeline_flag,
        timeline_value,
        duration_flag,
        duration_value,
        probe_flag,
        probe_value,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "add-audio-clip"
        || project_flag != "--project"
        || track_flag != "--track"
        || media_flag != "--media"
        || source_flag != "--source-start"
        || timeline_flag != "--timeline-start"
        || duration_flag != "--duration"
        || probe_flag != "--ffprobe"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let media_path = PathBuf::from(media_value);
    let track_id = TrackId::from_uuid(parse_uuid(track_value.clone(), "--track")?);
    let source_start = parse_exact_time(source_value.clone(), "--source-start")?;
    let timeline_start = parse_exact_time(timeline_value.clone(), "--timeline-start")?;
    let duration = parse_exact_time(duration_value.clone(), "--duration")?;
    let metadata = FfprobeInspector::new(probe_value).inspect(&media_path)?;
    if !metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Audio(_)))
    {
        return Err(CliError::NoAudioStream);
    }
    let requested_end = source_start.checked_add(duration)?;
    if !source_start.is_non_negative()
        || !duration.is_positive()
        || metadata
            .duration
            .is_some_and(|available| requested_end > available)
    {
        return Err(CliError::SourceRangeOutOfBounds);
    }
    let mut project = load(&project_path)?;
    let sequence_id = project
        .sequences
        .values()
        .find(|sequence| {
            sequence
                .tracks
                .iter()
                .any(|track| track.id == track_id && track.kind == TrackKind::Audio)
        })
        .ok_or_else(|| CliError::Command("audio track not found".to_owned()))?
        .id;
    let source_path = media_path.to_string_lossy().into_owned();
    let existing_media = project
        .media
        .values()
        .find(|asset| asset.source_path == source_path)
        .map(|asset| asset.id);
    let media_id = existing_media.unwrap_or_else(MediaAssetId::new);
    let clip_id = ClipId::new();
    let mut commands = Vec::with_capacity(2);
    if existing_media.is_none() {
        commands.push(Command::ImportMedia {
            id: media_id,
            source_path,
        });
    }
    commands.push(Command::InsertClip {
        sequence_id,
        track_id,
        id: clip_id,
        media_id,
        timeline_start,
        source_start,
        source_duration: duration,
    });
    CommandEngine::execute_transaction(&mut project, "Add audio clip", commands)
        .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::ClipAdded {
        path: project_path,
        clip_id: clip_id.as_uuid().to_string(),
    })
}

pub(super) fn run_create_video_sequence(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let [
        command,
        project_flag,
        project_path,
        media_flag,
        media_path,
        name_flag,
        name,
        start_flag,
        start,
        duration_flag,
        duration,
        probe_flag,
        probe,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if command != "create-video-sequence"
        || project_flag != "--project"
        || media_flag != "--media"
        || name_flag != "--sequence-name"
        || start_flag != "--source-start"
        || duration_flag != "--duration"
        || probe_flag != "--ffprobe"
    {
        return Err(CliError::Usage);
    }
    let project_path = PathBuf::from(project_path);
    let media_path = PathBuf::from(media_path);
    let sequence_name = name
        .to_str()
        .ok_or(CliError::NonUtf8Value("--sequence-name"))?
        .to_owned();
    let source_start = parse_exact_time(start.clone(), "--source-start")?;
    let duration = parse_exact_time(duration.clone(), "--duration")?;
    let metadata = FfprobeInspector::new(probe).inspect(&media_path)?;
    if !metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
    {
        return Err(CliError::NoVideoStream);
    }
    let requested_end = source_start.checked_add(duration)?;
    if !source_start.is_non_negative()
        || !duration.is_positive()
        || metadata
            .duration
            .is_none_or(|available| requested_end > available)
    {
        return Err(CliError::SourceRangeOutOfBounds);
    }
    let mut project = load(&project_path)?;
    let media_id = MediaAssetId::new();
    let sequence_id = SequenceId::new();
    let track_id = TrackId::new();
    CommandEngine::execute_transaction(
        &mut project,
        "Create video sequence",
        vec![
            Command::ImportMedia {
                id: media_id,
                source_path: media_path.to_string_lossy().into_owned(),
            },
            Command::CreateSequence {
                id: sequence_id,
                name: sequence_name,
            },
            Command::CreateTrack {
                sequence_id,
                id: track_id,
                kind: TrackKind::Video,
            },
            Command::InsertClip {
                sequence_id,
                track_id,
                id: ClipId::new(),
                media_id,
                timeline_start: RationalTime::ZERO,
                source_start,
                source_duration: duration,
            },
        ],
    )
    .map_err(|error| CliError::Command(error.to_string()))?;
    save_atomic(&project, &project_path)?;
    Ok(CliOutcome::VideoSequenceCreated {
        path: project_path,
        sequence_id: sequence_id.as_uuid().to_string(),
        track_id: track_id.as_uuid().to_string(),
    })
}

pub(super) fn run_create_project(arguments: &[OsString]) -> Result<CliOutcome, CliError> {
    let parsed = parse_create_project(arguments)?;
    if parsed.path.exists() {
        return Err(CliError::ProjectExists(parsed.path));
    }
    let project = Project::new(parsed.name);
    project.validate().map_err(|error| CliError::InvalidValue {
        option: "--name",
        value: error.to_string(),
    })?;
    save_atomic(&project, &parsed.path)?;
    Ok(CliOutcome::ProjectCreated {
        path: parsed.path,
        project_id: project.id.as_uuid().to_string(),
    })
}

pub(super) struct CreateProjectArgs {
    path: PathBuf,
    name: String,
}

pub(super) fn parse_create_project(arguments: &[OsString]) -> Result<CreateProjectArgs, CliError> {
    let Some(command) = arguments.first() else {
        return Err(CliError::Usage);
    };
    if command != "create-project" || arguments.len() != 5 {
        return Err(CliError::Usage);
    }
    let mut path = None;
    let mut name = None;
    for pair in arguments[1..].chunks_exact(2) {
        let flag = pair[0].to_str().ok_or(CliError::NonUtf8Value("option"))?;
        match flag {
            "--project" if path.is_none() => path = Some(PathBuf::from(&pair[1])),
            "--name" if name.is_none() => {
                name = Some(
                    pair[1]
                        .clone()
                        .into_string()
                        .map_err(|_| CliError::NonUtf8Value("--name"))?,
                );
            }
            _ => return Err(CliError::InvalidOption(flag.to_owned())),
        }
    }
    Ok(CreateProjectArgs {
        path: path.ok_or(CliError::MissingOption("--project"))?,
        name: name.ok_or(CliError::MissingOption("--name"))?,
    })
}
