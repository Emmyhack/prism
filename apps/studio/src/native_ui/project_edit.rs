use super::*;

#[cfg(test)]
pub(super) fn insert_video_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
) -> Result<UndoRecord, String> {
    insert_media_into_project(project, path, duration, false)
}

#[cfg(test)]
pub(super) fn insert_media_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
    has_audio: bool,
) -> Result<UndoRecord, String> {
    insert_stream_media_into_project(project, path, duration, true, has_audio, None)
}

#[allow(clippy::too_many_lines)]
pub(super) fn insert_stream_media_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
    has_video: bool,
    has_audio: bool,
    source_frame_rate: Option<RationalTime>,
) -> Result<UndoRecord, String> {
    if !has_video && !has_audio {
        return Err("media must contain a supported video or audio stream".to_owned());
    }
    let media_id = MediaAssetId::new();
    let primary_kind = if has_video {
        TrackKind::Video
    } else {
        TrackKind::Audio
    };
    let (sequence_id, track_id, timeline_start, mut commands) =
        if let Some((sequence_id, sequence)) = project.sequences.iter().next() {
            let sequence_id = *sequence_id;
            if let Some(track) = sequence
                .tracks
                .iter()
                .find(|track| track.kind == primary_kind && !track.locked)
            {
                let timeline_start = sequence.duration().unwrap_or(RationalTime::ZERO);
                (sequence_id, track.id, timeline_start, Vec::new())
            } else {
                let track_id = TrackId::new();
                (
                    sequence_id,
                    track_id,
                    sequence.duration().unwrap_or(RationalTime::ZERO),
                    vec![Command::CreateTrack {
                        sequence_id,
                        id: track_id,
                        kind: primary_kind,
                    }],
                )
            }
        } else {
            let sequence_id = SequenceId::new();
            let track_id = TrackId::new();
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Sequence 01")
                .to_owned();
            let mut commands = vec![Command::CreateSequence {
                id: sequence_id,
                name,
            }];
            if let Some(frame_rate) = source_frame_rate {
                commands.push(Command::SetSequenceFrameRate {
                    sequence_id,
                    frame_rate,
                });
            }
            commands.push(Command::CreateTrack {
                sequence_id,
                id: track_id,
                kind: primary_kind,
            });
            (sequence_id, track_id, RationalTime::ZERO, commands)
        };
    commands.insert(
        0,
        Command::ImportMedia {
            id: media_id,
            source_path: path.to_string_lossy().into_owned(),
        },
    );
    commands.push(Command::InsertClip {
        sequence_id,
        track_id,
        id: ClipId::new(),
        media_id,
        timeline_start,
        source_start: RationalTime::ZERO,
        source_duration: duration,
    });
    if has_video && has_audio {
        let audio_track_id = project
            .sequences
            .get(&sequence_id)
            .and_then(|sequence| {
                sequence
                    .tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Audio && !track.locked)
            })
            .map_or_else(
                || {
                    let id = TrackId::new();
                    commands.push(Command::CreateTrack {
                        sequence_id,
                        id,
                        kind: TrackKind::Audio,
                    });
                    id
                },
                |track| track.id,
            );
        commands.push(Command::InsertClip {
            sequence_id,
            track_id: audio_track_id,
            id: ClipId::new(),
            media_id,
            timeline_start,
            source_start: RationalTime::ZERO,
            source_duration: duration,
        });
    }
    CommandEngine::execute_transaction(project, "Import media", commands)
        .map(|outcome| outcome.undo)
        .map_err(|error| error.to_string())
}

#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
pub(super) fn apply_project_summary(ui: &PrismStudio, project: &Project, project_path: &Path) {
    let summary = prism_studio::project_summary(project);
    ui.set_has_project(true);
    ui.set_project_name(summary.name.into());
    ui.set_project_path(project_path.display().to_string().into());
    ui.set_media_count(i32::try_from(summary.media_count).unwrap_or(i32::MAX));
    ui.set_clip_count(i32::try_from(summary.clip_count).unwrap_or(i32::MAX));
    let media_items = project
        .media
        .values()
        .map(|asset| {
            Path::new(&asset.source_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&asset.source_path)
                .into()
        })
        .collect::<Vec<slint::SharedString>>();
    ui.set_media_items(slint::ModelRc::new(slint::VecModel::from(media_items)));
    let timeline_items = project
        .sequences
        .values()
        .next()
        .map(|sequence| {
            let total_seconds = sequence
                .duration()
                .map(|duration| duration.as_seconds_f64().max(0.001))
                .unwrap_or(1.0);
            let mut source_ends = BTreeMap::<MediaAssetId, f64>::new();
            for clip in sequence
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Audio)
                .flat_map(|track| &track.clips)
            {
                if let Ok(source_end) = clip.source_start.checked_add(clip.source_duration) {
                    source_ends
                        .entry(clip.media_id)
                        .and_modify(|current| *current = current.max(source_end.as_seconds_f64()))
                        .or_insert_with(|| source_end.as_seconds_f64());
                }
            }
            let source_ends = &source_ends;
            sequence
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(row, track)| {
                    track.clips.iter().map(move |clip| {
                        let name = project
                            .media
                            .get(&clip.media_id)
                            .and_then(|asset| Path::new(&asset.source_path).file_name())
                            .and_then(|name| name.to_str())
                            .unwrap_or("Offline Media");
                        TimelineClipItem {
                            name: name.into(),
                            source_path: project
                                .media
                                .get(&clip.media_id)
                                .map_or("", |asset| asset.source_path.as_str())
                                .into(),
                            start_ratio: (clip.timeline_start.as_seconds_f64() / total_seconds)
                                as f32,
                            width_ratio: (clip.source_duration.as_seconds_f64() / total_seconds)
                                as f32,
                            row: i32::try_from(row).unwrap_or(3),
                            audio: track.kind == TrackKind::Audio,
                            waveform_start_ratio: source_ends
                                .get(&clip.media_id)
                                .map_or(0.0, |end| {
                                    (clip.source_start.as_seconds_f64() / end.max(0.001)) as f32
                                }),
                            waveform_width_ratio: source_ends.get(&clip.media_id).map_or(
                                0.0,
                                |end| {
                                    (clip.source_duration.as_seconds_f64() / end.max(0.001)) as f32
                                },
                            ),
                            waveform: slint::ModelRc::new(slint::VecModel::from(Vec::<f32>::new())),
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_clips(slint::ModelRc::new(slint::VecModel::from(timeline_items)));
    let sequence = project.sequences.values().next();
    let transition_items = sequence
        .map(|sequence| {
            let total_seconds = sequence
                .duration()
                .map(|duration| duration.as_seconds_f64().max(0.001))
                .unwrap_or(1.0);
            sequence
                .transitions
                .iter()
                .filter_map(|transition| {
                    sequence.tracks.iter().enumerate().find_map(|(row, track)| {
                        track
                            .clips
                            .iter()
                            .find(|clip| clip.id == transition.outgoing_clip)
                            .and_then(|clip| {
                                clip.timeline_start
                                    .checked_add(clip.source_duration)
                                    .ok()
                                    .map(|boundary| TimelineTransitionItem {
                                        ratio: (boundary.as_seconds_f64() / total_seconds) as f32,
                                        width_ratio: (transition.duration.as_seconds_f64()
                                            / total_seconds)
                                            as f32,
                                        row: i32::try_from(row).unwrap_or(3),
                                    })
                            })
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_transitions(slint::ModelRc::new(slint::VecModel::from(transition_items)));
    let tracks = sequence
        .map(|s| {
            s.tracks
                .iter()
                .enumerate()
                .map(|(index, track)| TrackItem {
                    name: format!(
                        "{}{}",
                        if track.kind == TrackKind::Audio {
                            "A"
                        } else {
                            "V"
                        },
                        index + 1
                    )
                    .into(),
                    audio: track.kind == TrackKind::Audio,
                    locked: track.locked,
                    muted: track.muted,
                    visible: track.visible,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_tracks(slint::ModelRc::new(slint::VecModel::from(tracks)));
    ui.set_sequence_name(sequence.map_or("Main sequence", |s| s.name.as_str()).into());
    let frame_rate = sequence
        .and_then(|sequence| project.sequence_frame_rate(sequence.id))
        .unwrap_or(RationalTime::from_integer(30));
    let duration = sequence
        .and_then(|s| s.duration().ok())
        .unwrap_or(RationalTime::ZERO);
    let marker_items = sequence
        .map(|sequence| {
            sequence
                .markers
                .iter()
                .map(|marker| TimelineMarkerItem {
                    timecode: display_timecode_at_rate(marker.time, frame_rate).into(),
                    label: marker.label.clone().into(),
                    ratio: if duration.is_positive() {
                        (marker.time.as_seconds_f64() / duration.as_seconds_f64()) as f32
                    } else {
                        0.0
                    },
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_markers(slint::ModelRc::new(slint::VecModel::from(marker_items)));
    ui.set_selected_marker_index(-1);
    ui.set_duration_label(display_timecode_at_rate(duration, frame_rate).into());
    update_export_summary(ui, project);
    let labels = (0..6)
        .map(|index| {
            duration
                .checked_mul_i64(index)
                .and_then(|t| t.checked_div_i64(5))
                .map_or_else(
                    |_| "--:--:--:--".to_owned(),
                    |time| display_timecode_at_rate(time, frame_rate),
                )
                .into()
        })
        .collect::<Vec<slint::SharedString>>();
    ui.set_ruler_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
    ui.set_project_summary(
        format!(
            "{} media · {} sequences · {} tracks · {} clips · revision {}",
            summary.media_count,
            summary.sequence_count,
            summary.track_count,
            summary.clip_count,
            summary.revision
        )
        .into(),
    );
}

pub(super) fn load_project_into_ui(
    ui: &PrismStudio,
    project_path: Option<PathBuf>,
) -> Result<Option<Project>, prism_persistence::PersistenceError> {
    let Some(project_path) = project_path else {
        return Ok(None);
    };
    let project = prism_persistence::load(&project_path)?;
    apply_project_summary(ui, &project, &project_path);
    ui.set_export_output(
        project_path
            .with_extension("export.mp4")
            .display()
            .to_string()
            .into(),
    );
    Ok(Some(project))
}

pub(super) fn refresh_shortcuts(ui: &PrismStudio) {
    let query = ui.get_shortcut_query().to_lowercase();
    let premiere = ui.get_keyboard_preset() == "Premiere";
    let items = crate::edit_shortcuts::bindings(premiere)
        .into_iter()
        .filter(|binding| {
            query.is_empty()
                || binding.name.to_lowercase().contains(&query)
                || binding.detail.to_lowercase().contains(&query)
                || binding.key.to_lowercase().contains(&query)
        })
        .map(|binding| {
            let mut modifiers = Vec::new();
            if binding.primary {
                modifiers.push("Ctrl/Cmd");
            }
            if binding.shift {
                modifiers.push("Shift");
            }
            modifiers.push(binding.key);
            ShortcutItem {
                keys: modifiers.join("+").into(),
                name: binding.name.into(),
                detail: binding.detail.into(),
                available: !matches!(binding.target, crate::edit_shortcuts::Target::Missing(_)),
            }
        })
        .collect::<Vec<_>>();
    ui.set_shortcut_items(slint::ModelRc::new(slint::VecModel::from(items)));
}

#[allow(clippy::too_many_lines)]
pub(super) fn bind_editor_callbacks(ui: &PrismStudio, runtime: &Rc<NativeExportRuntime>) {
    let weak_ui = ui.as_weak();
    let weak = ui.as_weak();
    ui.on_shortcuts_changed(move || {
        if let Some(ui) = weak.upgrade() {
            refresh_shortcuts(&ui);
        }
    });
    let weak = ui.as_weak();
    let shortcut_runtime = Rc::clone(runtime);
    ui.on_edit_shortcut(move |key, primary, shift, alt| {
        let Some(ui) = weak.upgrade() else {
            return false;
        };
        let premiere = ui.get_keyboard_preset() == "Premiere";
        let Some(target) =
            crate::edit_shortcuts::resolve(key.as_str(), primary, shift, alt, premiere)
        else {
            return false;
        };
        match target {
            crate::edit_shortcuts::Target::Tool(tool) => {
                ui.set_active_tool(tool.into());
            }
            crate::edit_shortcuts::Target::Command(command) => {
                if let Err(error) = shortcut_runtime.handle(command, &ui) {
                    ui.set_feedback(error.into());
                }
            }
            crate::edit_shortcuts::Target::Missing(detail) => {
                ui.set_feedback(format!("Shortcut reserved · {detail} is not built yet").into());
            }
        }
        true
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_source_seek(move |ratio| {
        if let Some(ui) = weak.upgrade() {
            handler.seek_source(&ui, ratio);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_view_changed(move || {
        if let Some(ui) = weak.upgrade() {
            handler.update_ruler(&ui);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_filter_changed(move || {
        if let Some(ui) = weak.upgrade() {
            handler.filter_cards(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_palette_filter(move || {
        if let Some(ui) = weak.upgrade() {
            filter_launcher(&ui);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_palette_run(move |id| {
        if let Some(ui) = weak.upgrade() {
            let eligible = launcher_actions(&ui)
                .iter()
                .any(|item| item.id == id && item.enabled);
            if !eligible {
                ui.set_feedback("This action needs a project or an unlocked clip selection".into());
                return;
            }
            ui.set_palette_open(false);
            if id.starts_with("timeline.")
                || id.starts_with("source.")
                || id.starts_with("playback.")
            {
                ui.set_active_workspace("EDIT".into());
            }
            if let Err(error) = handler.handle(id.as_str(), &ui) {
                ui.set_feedback(error.into());
            }
        }
    });
    ui.on_media_matches(|name, query| name.to_lowercase().contains(&query.to_lowercase()));
    let command_runtime = Rc::clone(runtime);
    ui.on_command_invoked(move |id| {
        if let Some(ui) = weak_ui.upgrade() {
            match command_runtime.handle(id.as_str(), &ui) {
                Err(error) => ui.set_feedback(error.into()),
                Ok(()) if id.starts_with("export.") => {
                    apply_export_status(&ui, command_runtime.controller.borrow().status().as_ref());
                }
                Ok(()) => {}
            }
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_media_selected(move |index| {
        if let Some(ui) = weak.upgrade() {
            handler.select_media(&ui, index);
            ui.set_inspector_section("Source".into());
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_clip_selected(move |index| {
        if let Some(ui) = weak.upgrade() {
            handler.select_clip(&ui, index);
            ui.set_inspector_section("Clip".into());
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_clip_adjusted(move |index, delta, edge| {
        if let Some(ui) = weak.upgrade()
            && let Err(error) = handler.drag_clip(&ui, index, delta, edge.as_str())
        {
            ui.set_feedback(error.into());
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_seek(move |ratio| {
        if let Some(ui) = weak.upgrade() {
            handler.seek_ratio(&ui, ratio);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_track_action(move |index, action| {
        if let Some(ui) = weak.upgrade()
            && let Err(error) = handler.change_track(&ui, index, action.as_str())
        {
            ui.set_feedback(error.into());
        }
    });
    refresh_shortcuts(ui);
}

/// Opens the main editor window and runs the native event loop.
///
/// # Errors
///
/// Returns a platform error if a window cannot be created or the native event
/// loop cannot run.
pub fn launch(project_path: Option<PathBuf>) -> Result<(), NativeUiError> {
    let ui = PrismStudio::new()?;
    update_media_runtime_status(&ui);
    let runtime_project_path = project_path.clone();
    let loaded_project = load_project_into_ui(&ui, project_path)?;
    let export_runtime = Rc::new(NativeExportRuntime::new(
        loaded_project,
        runtime_project_path,
    ));
    if export_runtime.project.borrow().is_some() {
        export_runtime.reset_view(&ui);
    }
    bind_editor_callbacks(&ui, &export_runtime);
    let poll_ui = ui.as_weak();
    let poll_runtime = Rc::clone(&export_runtime);
    let export_timer = slint::Timer::default();
    export_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            if let Some(ui) = poll_ui.upgrade() {
                poll_runtime.poll(&ui);
            }
        },
    );
    ui.run()?;
    Ok(())
}
