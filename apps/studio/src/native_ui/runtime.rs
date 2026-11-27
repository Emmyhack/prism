use super::*;

pub(super) struct NativeExportRuntime {
    pub(super) project: RefCell<Option<Project>>,
    pub(super) project_path: RefCell<Option<PathBuf>>,
    pub(super) controller: RefCell<ExportController>,
    pub(super) worker: RefCell<Option<ExportWorker>>,
    pub(super) queued_export: RefCell<Option<(Project, ExportPreset)>>,
    pub(super) program_preview: PreviewWorker,
    pub(super) source_preview: PreviewWorker,
    pub(super) playhead: Cell<RationalTime>,
    pub(super) playing: Cell<bool>,
    pub(super) playback_started: Cell<Option<Instant>>,
    pub(super) playback_origin: Cell<RationalTime>,
    pub(super) audio_process: RefCell<Option<Child>>,
    pub(super) audio_decoder: RefCell<Option<Child>>,
    pub(super) audio_end: Cell<RationalTime>,
    pub(super) audio_clock: RefCell<Option<crate::audio_clock::AudioClock>>,
    pub(super) audio_clip: Cell<Option<ClipId>>,
    pub(super) source_position: Cell<RationalTime>,
    pub(super) source_duration: Cell<RationalTime>,
    pub(super) source_in: Cell<RationalTime>,
    pub(super) source_out: Cell<RationalTime>,
    pub(super) undo: RefCell<Vec<UndoRecord>>,
    pub(super) redo: RefCell<Vec<UndoRecord>>,
    pub(super) thumbnail_preview: PreviewWorker,
    pub(super) thumbnail_queue: RefCell<VecDeque<(usize, PathBuf)>>,
    pub(super) thumbnail_current: Cell<Option<usize>>,
    pub(super) thumbnail_total: Cell<usize>,
    pub(super) thumbnail_completed: Cell<usize>,
    pub(super) thumbnail_failed: Cell<usize>,
    pub(super) waveform_worker: RefCell<WaveformWorker>,
    pub(super) waveform_total: Cell<usize>,
    pub(super) waveform_completed: Cell<usize>,
    pub(super) waveform_failed: Cell<usize>,
    pub(super) waveforms: RefCell<BTreeMap<String, Vec<f32>>>,
    pub(super) media_artifacts: RefCell<Option<MediaArtifactWorker>>,
    pub(super) cards: RefCell<Vec<MediaCard>>,
    pub(super) use_proxy: Cell<bool>,
    pub(super) proxy_generation: RefCell<Option<ProxyGeneration>>,
    pub(super) proxy_availability: RefCell<BTreeMap<MediaAssetId, bool>>,
    pub(super) recovery_candidates: RefCell<Vec<prism_persistence::AutosaveCandidate>>,
    pub(super) autosave_scheduler: RefCell<Option<prism_persistence::AutosaveScheduler>>,
    pub(super) pending_autosave: RefCell<Option<Project>>,
    pub(super) autosave_preferences: Cell<AutosavePreferences>,
    pub(super) cache_eviction: RefCell<Option<CacheEvictionWorker>>,
}

impl Drop for NativeExportRuntime {
    fn drop(&mut self) {
        self.waveform_worker.borrow().cancel();
        if let Some(worker) = self.worker.get_mut().as_ref() {
            worker.cancel();
        }
        if let Some(generation) = self.proxy_generation.get_mut().as_ref() {
            generation.cancelled.store(true, Ordering::Release);
        }
        if let Some(mut child) = self.audio_process.get_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.audio_decoder.get_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl NativeExportRuntime {
    pub(super) fn new(project: Option<Project>, project_path: Option<PathBuf>) -> Self {
        Self {
            project: RefCell::new(project),
            project_path: RefCell::new(project_path),
            controller: RefCell::new(ExportController::new()),
            worker: RefCell::new(None),
            queued_export: RefCell::new(None),
            program_preview: PreviewWorker::new(),
            source_preview: PreviewWorker::new(),
            playhead: Cell::new(RationalTime::ZERO),
            playing: Cell::new(false),
            playback_started: Cell::new(None),
            playback_origin: Cell::new(RationalTime::ZERO),
            audio_process: RefCell::new(None),
            audio_decoder: RefCell::new(None),
            audio_end: Cell::new(RationalTime::ZERO),
            audio_clock: RefCell::new(None),
            audio_clip: Cell::new(None),
            source_position: Cell::new(RationalTime::ZERO),
            source_duration: Cell::new(RationalTime::ZERO),
            source_in: Cell::new(RationalTime::ZERO),
            source_out: Cell::new(RationalTime::ZERO),
            undo: RefCell::new(Vec::new()),
            redo: RefCell::new(Vec::new()),
            thumbnail_preview: PreviewWorker::with_thumbnails(true),
            thumbnail_queue: RefCell::new(VecDeque::new()),
            thumbnail_current: Cell::new(None),
            thumbnail_total: Cell::new(0),
            thumbnail_completed: Cell::new(0),
            thumbnail_failed: Cell::new(0),
            waveform_worker: RefCell::new(WaveformWorker::spawn(media_tool("ffmpeg"), 64)),
            waveform_total: Cell::new(0),
            waveform_completed: Cell::new(0),
            waveform_failed: Cell::new(0),
            waveforms: RefCell::new(BTreeMap::new()),
            media_artifacts: RefCell::new(None),
            cards: RefCell::new(Vec::new()),
            use_proxy: Cell::new(false),
            proxy_generation: RefCell::new(None),
            proxy_availability: RefCell::new(BTreeMap::new()),
            recovery_candidates: RefCell::new(Vec::new()),
            autosave_scheduler: RefCell::new(None),
            pending_autosave: RefCell::new(None),
            autosave_preferences: Cell::new(AutosavePreferences::default()),
            cache_eviction: RefCell::new(None),
        }
    }

    pub(super) fn frame_duration(&self) -> RationalTime {
        self.project
            .borrow()
            .as_ref()
            .and_then(|project| {
                let sequence_id = *project.sequences.keys().next()?;
                project.sequence_frame_duration(sequence_id)
            })
            .unwrap_or_else(|| RationalTime::new(1, 30).unwrap_or(RationalTime::from_integer(1)))
    }

    pub(super) fn frame_rate(&self) -> RationalTime {
        self.project
            .borrow()
            .as_ref()
            .and_then(|project| {
                let sequence_id = *project.sequences.keys().next()?;
                project.sequence_frame_rate(sequence_id)
            })
            .unwrap_or(RationalTime::from_integer(30))
    }

    pub(super) fn quantize_to_frame(&self, seconds: f64) -> RationalTime {
        let frame = self.frame_duration();
        #[allow(clippy::cast_possible_truncation)]
        let count = (seconds.max(0.0) / frame.as_seconds_f64()).floor() as i64;
        frame.checked_mul_i64(count).unwrap_or(RationalTime::ZERO)
    }

    pub(super) fn display_timecode(&self, time: RationalTime) -> String {
        display_timecode_at_rate(time, self.frame_rate())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn handle(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        match command {
            "view.markers" => {
                ui.set_active_workspace("EDIT".into());
                ui.set_browser_tab("Markers".into());
                Ok(())
            }
            "view.properties" | "view.source" | "view.history" | "view.jobs" | "view.scopes"
            | "view.shortcuts" => {
                ui.set_active_workspace("EDIT".into());
                ui.set_inspector_open(true);
                ui.set_inspector_tab(
                    match command {
                        "view.history" => "History",
                        "view.jobs" => "Jobs",
                        "view.scopes" => "Scopes",
                        "view.shortcuts" => "Shortcuts",
                        _ => "Properties",
                    }
                    .into(),
                );
                if matches!(command, "view.properties" | "view.source") {
                    ui.set_inspector_section(
                        if command == "view.source" {
                            "Source"
                        } else {
                            "Clip"
                        }
                        .into(),
                    );
                }
                Ok(())
            }
            "view.export" => {
                ui.set_active_workspace("EXPORT".into());
                Ok(())
            }
            "view.save_layout" => self.save_view(ui),
            "view.reset" => {
                apply_view(ui, &ViewPreferences::default());
                ui.set_workspace_status("Default view · not saved".into());
                self.filter_cards(ui);
                self.update_ruler(ui);
                Ok(())
            }
            "tool.V" | "tool.C" | "tool.T" | "tool.Y" | "tool.U" | "tool.R" => {
                ui.set_active_workspace("EDIT".into());
                ui.set_active_tool(command.trim_start_matches("tool.").into());
                Ok(())
            }
            "timeline.rename_marker" => self.rename_marker(ui),
            "project.new" => self.new_project(ui),
            "media.find_timeline" => self.find_media_in_timeline(ui),
            "project.save" => self.save(ui),
            "project.save_as" => self.save_as(ui),
            "edit.undo" | "edit.redo" => self.apply_history(ui, command == "edit.redo"),
            "timeline.apply_timing" => self.apply_timing(ui),
            "timeline.move_left"
            | "timeline.move_right"
            | "timeline.trim_in_left"
            | "timeline.trim_in_right"
            | "timeline.trim_out_left"
            | "timeline.trim_out_right" => self.adjust_clip_frame(ui, command),
            "timeline.slip_left"
            | "timeline.slip_right"
            | "timeline.roll_in_left"
            | "timeline.roll_in_right"
            | "timeline.roll_out_left"
            | "timeline.roll_out_right"
            | "timeline.slide_left"
            | "timeline.slide_right" => self.precision_edit_frame(ui, command),
            "source.append" => self.append_source(ui),
            "source.insert" | "source.overwrite" => {
                self.edit_source(ui, command == "source.overwrite")
            }
            "media.assign_proxy" => self.assign_proxy(ui),
            "media.relink_source" => self.relink_source(ui),
            "media.clear_proxy" => self.clear_proxy(ui),
            "media.generate_proxy" => self.generate_proxy(ui),
            "media.regenerate_proxy" => self.regenerate_proxy(ui),
            "media.cancel_proxy" => {
                if let Some(generation) = self.proxy_generation.borrow().as_ref() {
                    generation.cancelled.store(true, Ordering::Release);
                    ui.set_feedback("Cancelling proxy generation…".into());
                }
                Ok(())
            }
            "cache.clean" => self.clean_cache(ui),
            "autosave.toggle" => self.update_autosave_preferences(ui, true),
            "autosave.retention" => self.update_autosave_preferences(ui, false),
            "media.toggle_proxy" => {
                self.use_proxy.set(!self.use_proxy.get());
                ui.set_proxy_active(self.use_proxy.get());
                self.select_media(ui, ui.get_selected_media_index());
                self.seek_time(ui, self.playhead.get());
                Ok(())
            }
            "source.match_program_frame" => {
                let index = self
                    .project
                    .borrow()
                    .as_ref()
                    .and_then(|project| program_clip_index(project, self.playhead.get()))
                    .ok_or("No visible video clip beneath the playhead")?;
                ui.set_selected_clip_index(index);
                self.update_inspector(ui);
                self.match_frame(ui)
            }
            "source.match_frame" => self.match_frame(ui),
            "source.clear_marks" => {
                if !ui.get_source_ready() {
                    return Err("Select an available source first".to_owned());
                }
                self.source_in.set(RationalTime::ZERO);
                self.source_out.set(self.source_duration.get());
                self.update_source_labels(ui);
                ui.set_feedback("Source range reset to the full clip".into());
                Ok(())
            }
            "source.mark_in" | "source.mark_out" => {
                if !ui.get_source_ready() {
                    return Err("Select an available source first".to_owned());
                }
                let position = self.source_position.get();
                if command == "source.mark_in" {
                    if position >= self.source_out.get() {
                        return Err("In point must be before Out".to_owned());
                    }
                    self.source_in.set(position);
                } else {
                    let end = position
                        .checked_add(self.frame_duration())
                        .map_err(|e| e.to_string())?
                        .min(self.source_duration.get());
                    if end <= self.source_in.get() {
                        return Err("Out point must be after In".to_owned());
                    }
                    self.source_out.set(end);
                }
                self.update_source_labels(ui);
                Ok(())
            }
            "timeline.split_all" => {
                let commands = split_all_commands(
                    self.project
                        .borrow()
                        .as_ref()
                        .ok_or("Open a project first")?,
                    self.playhead.get(),
                )?;
                self.commit_commands(ui, commands, "Add edit to all unlocked tracks")?;
                ui.set_selected_clip_index(-1);
                self.update_inspector(ui);
                Ok(())
            }
            "timeline.split_av"
            | "timeline.delete_av"
            | "timeline.ripple_delete_av"
            | "timeline.move_av"
            | "timeline.trim_av" => self.edit_aligned_av(command, ui),
            "timeline.split" | "timeline.delete" | "timeline.ripple_delete" => {
                self.edit_clip_or_aligned(command, ui)
            }
            "timeline.add_marker" => self.add_marker(ui),
            "timeline.delete_marker" => self.delete_marker(ui),
            "timeline.toggle_dissolve" => self.toggle_dissolve(ui),
            "timeline.duplicate" => self.duplicate_clip(ui),
            "timeline.track_up" => self.change_track(ui, ui.get_selected_track_index(), "up"),
            "timeline.track_down" => self.change_track(ui, ui.get_selected_track_index(), "down"),
            "timeline.toggle_snap" => {
                ui.set_snap_enabled(!ui.get_snap_enabled());
                ui.set_feedback(
                    if ui.get_snap_enabled() {
                        "Timeline snapping enabled"
                    } else {
                        "Timeline snapping disabled"
                    }
                    .into(),
                );
                Ok(())
            }
            "playback.stop" => {
                let retain_picture = ui.get_has_program_frame();
                self.stop_playback(ui);
                self.seek_time(ui, self.playhead.get());
                ui.set_has_program_frame(retain_picture);
                Ok(())
            }
            "view.zoom_in" | "view.zoom_out" => {
                let zoom = if command == "view.zoom_in" {
                    ui.get_timeline_zoom() * 2.0
                } else {
                    ui.get_timeline_zoom() / 2.0
                }
                .clamp(1.0, 8.0);
                ui.set_timeline_zoom(zoom);
                ui.set_timeline_offset(ui.get_timeline_offset().min(1.0 - 1.0 / zoom));
                self.update_ruler(ui);
                Ok(())
            }
            "playback.play_pause" => {
                let playing = !self.playing.get();
                self.playing.set(playing);
                ui.set_playing(playing);
                if playing {
                    self.playback_origin.set(self.playhead.get());
                    self.playback_started.set(Some(Instant::now()));
                    if let Err(error) = self.start_audio_playback() {
                        self.stop_playback(ui);
                        ui.set_feedback(error.into());
                    }
                    ui.set_playback_buffering(
                        self.playing.get() && self.audio_clock.borrow().is_some(),
                    );
                } else {
                    let retain_picture = ui.get_has_program_frame();
                    self.stop_playback(ui);
                    self.seek_time(ui, self.playhead.get());
                    ui.set_has_program_frame(retain_picture);
                }
                Ok(())
            }
            "timeline.fit" => {
                ui.set_timeline_zoom(1.0);
                ui.set_timeline_offset(0.0);
                self.update_ruler(ui);
                Ok(())
            }
            "playback.start" | "playback.end" | "playback.previous_edit" | "playback.next_edit" => {
                let target = self
                    .project
                    .borrow()
                    .as_ref()
                    .and_then(|project| navigation_target(project, self.playhead.get(), command))
                    .ok_or_else(|| "Open a sequence to navigate its edits".to_owned())?;
                self.stop_playback(ui);
                self.seek_time(ui, target);
                Ok(())
            }
            "playback.step_backward" | "playback.step_forward" => {
                self.stop_playback(ui);
                let frame = self.frame_duration();
                let delta = if command == "playback.step_forward" {
                    frame
                } else {
                    RationalTime::ZERO
                        .checked_sub(frame)
                        .map_err(|error| error.to_string())?
                };
                let time = self
                    .playhead
                    .get()
                    .checked_add(delta)
                    .map_err(|e| e.to_string())?;
                self.seek_time(ui, time.max(RationalTime::ZERO));
                Ok(())
            }
            "export.destination" => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export video")
                    .set_file_name("Prism export.mp4")
                    .add_filter("MP4 video", &["mp4"])
                    .save_file()
                {
                    ui.set_export_output(path.display().to_string().into());
                }
                Ok(())
            }
            "project.open" => self.open_project(ui),
            "recovery.recover_latest" => self.recover_latest(ui),
            "project.import_media" => self.import_media(ui),
            "export.queue" => self.queue(ui),
            "export.cancel" => {
                if let Some(worker) = self.worker.borrow().as_ref() {
                    worker.cancel();
                }
                self.controller
                    .borrow_mut()
                    .cancel()
                    .map_err(|error| error.to_string())
            }
            "export.retry" => self.retry(),
            "export.remove" => self
                .controller
                .borrow_mut()
                .remove()
                .map_err(|error| error.to_string()),
            _ => Err("This command is not available".to_owned()),
        }
    }

    pub(super) fn new_project(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Create Prism Project")
            .set_file_name("Untitled.prism")
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled");
        let project = Project::new(name);
        prism_persistence::save_atomic(&project, &path).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &path);
        ui.set_export_output(
            path.with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(path);
        self.reset_view(ui);
        Ok(())
    }

    pub(super) fn open_project(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open Prism Project")
            .add_filter("Prism Project", &["prism", "json"])
            .pick_file()
        else {
            return Ok(());
        };
        let project = prism_persistence::load(&path).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &path);
        ui.set_export_output(
            path.with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(path);
        self.reset_view(ui);
        Ok(())
    }

    pub(super) fn queue(&self, ui: &PrismStudio) -> Result<(), String> {
        media_runtime_status()?;
        let project = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "open a project before exporting".to_owned())?;
        if self.worker.borrow().is_some() {
            return Err("An export is already running".to_owned());
        }
        if ui.get_export_output().trim().is_empty() {
            return Err("Choose an output destination first".to_owned());
        }
        let output = PathBuf::from(ui.get_export_output().as_str());
        if output.exists() {
            return Err(
                "Choose a new output filename; Prism does not replace existing media".to_owned(),
            );
        }
        let preset = export_preset(ui.get_export_preset_index())?;
        update_export_summary(ui, &project);
        if !ui.get_can_export() {
            return Err(ui.get_export_preflight().to_string());
        }
        *self.queued_export.borrow_mut() = Some((project.clone(), preset));
        let mut controller = self.controller.borrow_mut();
        controller.queue(output.clone());
        launch_default_export(
            &mut controller,
            &mut self.worker.borrow_mut(),
            &project,
            output,
            preset,
        )
    }

    pub(super) fn retry(&self) -> Result<(), String> {
        if self.worker.borrow().is_some() {
            return Err("Wait for the current export worker to finish".to_owned());
        }
        let (project, preset) = self
            .queued_export
            .borrow()
            .clone()
            .ok_or_else(|| "Export job is unavailable".to_owned())?;
        let mut controller = self.controller.borrow_mut();
        controller.retry().map_err(|error| error.to_string())?;
        let output = controller
            .status()
            .map(|status| PathBuf::from(status.output))
            .ok_or_else(|| "export job is unavailable".to_owned())?;
        launch_default_export(
            &mut controller,
            &mut self.worker.borrow_mut(),
            &project,
            output,
            preset,
        )
    }

    pub(super) fn import_media(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("Import Media")
            .add_filter(
                "Video and audio",
                &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpeg", "mpg"],
            )
            .add_filter(
                "Audio",
                &["wav", "aif", "aiff", "mp3", "m4a", "aac", "flac", "ogg"],
            )
            .pick_files()
        else {
            return Ok(());
        };
        let mut project = self.project.borrow().clone().unwrap_or_else(|| {
            let name = paths
                .first()
                .and_then(|path| path.file_stem())
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled");
            Project::new(name)
        });
        let mut import_history = Vec::new();
        for path in paths {
            import_history.push(import_media_into_project(&mut project, &path)?);
        }
        let project_path = if let Some(path) = self.project_path.borrow().clone() {
            path
        } else {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Save Prism Project")
                .set_file_name(format!("{}.prism", project.name))
                .add_filter("Prism Project", &["prism"])
                .save_file()
            else {
                return Ok(());
            };
            path
        };
        prism_persistence::save_atomic(&project, &project_path)
            .map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &project_path);
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(project_path);
        self.reset_view(ui);
        self.undo.borrow_mut().extend(import_history);
        self.update_history(ui);
        Ok(())
    }

    pub(super) fn save(&self, ui: &PrismStudio) -> Result<(), String> {
        let project = self.project.borrow();
        let project = project
            .as_ref()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let path = self.project_path.borrow();
        let path = path
            .as_ref()
            .ok_or_else(|| "Project has no save destination".to_owned())?;
        prism_persistence::save_atomic(project, path).map_err(|e| e.to_string())?;
        ui.set_feedback("Project saved".into());
        Ok(())
    }

    pub(super) fn save_as(&self, ui: &PrismStudio) -> Result<(), String> {
        let default_name = self
            .project_path
            .borrow()
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|name| name.to_str())
            .map_or_else(
                || "Prism copy.prism".to_owned(),
                |name| format!("{name} copy.prism"),
            );
        let Some(destination) = rfd::FileDialog::new()
            .set_title("Save Prism Project As")
            .set_file_name(default_name)
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        self.save_as_to(ui, &destination)
    }

    pub(super) fn save_as_to(&self, ui: &PrismStudio, destination: &Path) -> Result<(), String> {
        let project = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        // Existing projects must never be replaced by Save As.
        prism_persistence::save_backup(&project, destination).map_err(|error| error.to_string())?;
        let view = capture_view(ui);
        let undo = self.undo.borrow_mut().drain(..).collect::<Vec<_>>();
        let redo = self.redo.borrow_mut().drain(..).collect::<Vec<_>>();
        *self.project_path.borrow_mut() = Some(destination.to_path_buf());
        apply_project_summary(ui, &project, destination);
        ui.set_export_output(
            destination
                .with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        self.reset_view(ui);
        *self.undo.borrow_mut() = undo;
        *self.redo.borrow_mut() = redo;
        self.update_history(ui);
        apply_view(ui, &view);
        self.filter_cards(ui);
        self.update_ruler(ui);
        ui.set_workspace_status("Current view · not saved for this copy".into());
        ui.set_feedback("Saved a new project copy · further edits use this copy".into());
        Ok(())
    }

    pub(super) fn refresh_recovery(&self, ui: &PrismStudio) {
        let candidates = self
            .project_path
            .borrow()
            .as_deref()
            .map(prism_persistence::list_autosaves)
            .transpose();
        match candidates {
            Ok(Some(candidates)) => {
                ui.set_recovery_count(i32::try_from(candidates.len()).unwrap_or(i32::MAX));
                if let Some(candidate) = candidates.first() {
                    ui.set_recovery_revision(candidate.revision.to_string().into());
                    ui.set_recovery_path(candidate.path.display().to_string().into());
                } else {
                    ui.set_recovery_revision("".into());
                    ui.set_recovery_path("".into());
                }
                *self.recovery_candidates.borrow_mut() = candidates;
            }
            Ok(None) => {
                self.recovery_candidates.borrow_mut().clear();
                ui.set_recovery_count(0);
                ui.set_recovery_revision("".into());
                ui.set_recovery_path("".into());
            }
            Err(error) => {
                self.recovery_candidates.borrow_mut().clear();
                ui.set_recovery_count(0);
                ui.set_recovery_revision("".into());
                ui.set_recovery_path("".into());
                ui.set_feedback(format!("Could not inspect recovery files: {error}").into());
            }
        }
    }

    pub(super) fn reset_autosave_scheduler(&self, ui: &PrismStudio) {
        self.autosave_scheduler.borrow_mut().take();
        self.pending_autosave.borrow_mut().take();
        let Some(path) = self.project_path.borrow().clone() else {
            self.autosave_preferences
                .set(AutosavePreferences::default());
            ui.set_autosave_enabled(false);
            ui.set_autosave_retention(5);
            ui.set_autosave_status("UNAVAILABLE".into());
            return;
        };
        let store = AutosavePreferencesStore::new(path.with_extension("autosave.json"));
        let preferences = match store.load() {
            Ok(preferences) => preferences,
            Err(error) => {
                ui.set_feedback(format!("Could not load autosave preferences: {error}").into());
                AutosavePreferences::default()
            }
        };
        self.autosave_preferences.set(preferences);
        ui.set_autosave_enabled(preferences.enabled);
        ui.set_autosave_retention(
            i32::try_from(preferences.retained_revisions).unwrap_or(i32::MAX),
        );
        if !preferences.enabled {
            ui.set_autosave_status("DISABLED".into());
            return;
        }
        match prism_persistence::AutosaveScheduler::spawn(path, 2) {
            Ok(scheduler) => {
                *self.autosave_scheduler.borrow_mut() = Some(scheduler);
                ui.set_autosave_status("READY".into());
            }
            Err(error) => {
                ui.set_autosave_status("FAILED".into());
                ui.set_feedback(format!("Could not start autosave: {error}").into());
            }
        }
    }

    pub(super) fn update_autosave_preferences(
        &self,
        ui: &PrismStudio,
        toggle_enabled: bool,
    ) -> Result<(), String> {
        let project_path =
            self.project_path.borrow().clone().ok_or_else(|| {
                "Save the project before changing autosave preferences".to_owned()
            })?;
        let mut preferences = self.autosave_preferences.get();
        if toggle_enabled {
            preferences.enabled = !preferences.enabled;
        } else {
            preferences.retained_revisions = match preferences.retained_revisions {
                1 => 5,
                5 => 10,
                10 => 20,
                _ => 1,
            };
        }
        AutosavePreferencesStore::new(project_path.with_extension("autosave.json"))
            .save(preferences)
            .map_err(|error| error.to_string())?;
        self.autosave_preferences.set(preferences);
        self.reset_autosave_scheduler(ui);
        ui.set_feedback(
            format!(
                "Autosave {} · retaining {} revisions",
                if preferences.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                preferences.retained_revisions
            )
            .into(),
        );
        Ok(())
    }

    pub(super) fn queue_autosave(&self, ui: &PrismStudio, project: &Project) {
        if !self.autosave_preferences.get().enabled {
            ui.set_autosave_status("DISABLED".into());
            return;
        }
        if self.pending_autosave.borrow().is_some() {
            *self.pending_autosave.borrow_mut() = Some(project.clone());
            ui.set_autosave_status("BUSY".into());
            return;
        }
        let scheduler_ref = self.autosave_scheduler.borrow();
        let Some(scheduler) = scheduler_ref.as_ref() else {
            ui.set_autosave_status("UNAVAILABLE".into());
            return;
        };
        match scheduler.try_submit(project) {
            prism_persistence::AutosaveSubmit::Queued => {
                ui.set_autosave_status("QUEUED".into());
            }
            prism_persistence::AutosaveSubmit::QueueFull => {
                *self.pending_autosave.borrow_mut() = Some(project.clone());
                ui.set_autosave_status("BUSY".into());
            }
            prism_persistence::AutosaveSubmit::WorkerStopped => {
                ui.set_autosave_status("FAILED".into());
            }
        }
    }

    pub(super) fn retry_pending_autosave(&self, ui: &PrismStudio) {
        let Some(project) = self.pending_autosave.borrow().clone() else {
            return;
        };
        let scheduler_ref = self.autosave_scheduler.borrow();
        let Some(scheduler) = scheduler_ref.as_ref() else {
            return;
        };
        match scheduler.try_submit(&project) {
            prism_persistence::AutosaveSubmit::Queued => {
                self.pending_autosave.borrow_mut().take();
                ui.set_autosave_status("QUEUED".into());
            }
            prism_persistence::AutosaveSubmit::QueueFull => {
                ui.set_autosave_status("BUSY".into());
            }
            prism_persistence::AutosaveSubmit::WorkerStopped => {
                ui.set_autosave_status("FAILED".into());
            }
        }
    }

    pub(super) fn recover_latest(&self, ui: &PrismStudio) -> Result<(), String> {
        let source = self
            .recovery_candidates
            .borrow()
            .first()
            .map(|candidate| candidate.path.clone())
            .ok_or_else(|| "No recovery candidate is available".to_owned())?;
        let default_name = self
            .project_path
            .borrow()
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|name| name.to_str())
            .map_or_else(
                || "Recovered.prism".to_owned(),
                |name| format!("{name}.recovered.prism"),
            );
        let Some(destination) = rfd::FileDialog::new()
            .set_title("Recover Prism Autosave As")
            .set_file_name(default_name)
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        let revision = prism_persistence::recover_autosave(&source, &destination)
            .map_err(|error| error.to_string())?;
        let project = prism_persistence::load(&destination).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &destination);
        ui.set_export_output(
            destination
                .with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(destination);
        self.reset_view(ui);
        ui.set_feedback(
            format!("Recovered autosave revision {revision} into a new project").into(),
        );
        Ok(())
    }

    pub(super) fn reset_view(&self, ui: &PrismStudio) {
        self.stop_playback(ui);
        if let Some(generation) = self.proxy_generation.borrow().as_ref() {
            generation.cancelled.store(true, Ordering::Release);
        }
        self.proxy_generation.borrow_mut().take();
        self.proxy_availability.borrow_mut().clear();
        ui.set_proxy_generating(false);
        self.undo.borrow_mut().clear();
        self.redo.borrow_mut().clear();
        self.update_history(ui);
        ui.set_timeline_zoom(1.0);
        ui.set_timeline_offset(0.0);
        ui.set_snap_enabled(true);
        self.refresh_recovery(ui);
        self.reset_autosave_scheduler(ui);
        self.cache_eviction.borrow_mut().take();
        self.media_artifacts.borrow_mut().take();
        if let Some(root) = self.cache_root() {
            *self.media_artifacts.borrow_mut() = Some(MediaArtifactWorker::spawn(root, 8));
        }
        self.update_cache_state(ui);
        self.source_duration.set(RationalTime::ZERO);
        self.source_position.set(RationalTime::ZERO);
        self.source_in.set(RationalTime::ZERO);
        self.source_out.set(RationalTime::ZERO);
        ui.set_source_ready(false);
        self.update_source_labels(ui);
        self.reset_thumbnails(ui);
        self.reset_waveforms(ui);
        ui.set_inspector_name("".into());
        ui.set_inspector_path("".into());
        ui.set_selected_proxy("".into());
        ui.set_proxy_status(PROXY_MISSING.into());
        self.use_proxy.set(false);
        ui.set_proxy_active(false);
        ui.set_inspector_locked(false);
        ui.set_selected_clip_index(-1);
        ui.set_selected_media_index(-1);
        ui.set_selected_media("".into());
        ui.set_selected_source_path("".into());
        ui.set_source_preview_size("".into());
        ui.set_source_file_status("Select source footage to view its details".into());
        ui.set_has_source_frame(false);
        ui.set_source_detail("Select footage in the media browser".into());
        self.source_preview.invalidate();
        ui.set_active_workspace("EDIT".into());
        self.restore_view(ui);
        self.seek_time(ui, RationalTime::ZERO);
    }

    pub(super) fn save_view(&self, ui: &PrismStudio) -> Result<(), String> {
        let path = self
            .project_path
            .borrow()
            .as_ref()
            .map(|path| path.with_extension("workspace.json"))
            .ok_or_else(|| "Save a project before saving its workspace".to_owned())?;
        capture_view(ui)
            .save(&path)
            .map_err(|error| error.to_string())?;
        ui.set_workspace_status("Saved for this project".into());
        ui.set_feedback(
            "Workspace saved · this project will reopen with these panels and preferences".into(),
        );
        Ok(())
    }

    pub(super) fn restore_view(&self, ui: &PrismStudio) {
        let path = self
            .project_path
            .borrow()
            .as_ref()
            .map(|path| path.with_extension("workspace.json"));
        let saved = path
            .as_ref()
            .map_or(Ok(None), |path| ViewPreferences::load(path));
        let view = match saved {
            Ok(Some(view)) => {
                ui.set_workspace_status("Restored saved view".into());
                view
            }
            Ok(None) => {
                ui.set_workspace_status("Default view".into());
                ViewPreferences::default()
            }
            Err(error) => {
                ui.set_workspace_status("Default view".into());
                ui.set_feedback(format!("Could not restore the saved workspace: {error}").into());
                ViewPreferences::default()
            }
        };
        apply_view(ui, &view);
        self.filter_cards(ui);
        self.update_ruler(ui);
    }

    pub(super) fn cache_root(&self) -> Option<PathBuf> {
        self.project_path
            .borrow()
            .as_ref()
            .map(|path| path.with_extension("prism-cache"))
    }

    pub(super) fn update_cache_state(&self, ui: &PrismStudio) {
        let pending = self
            .cache_eviction
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.pending_evictions() > 0);
        ui.set_cache_cleaning(pending);
        let Some(root) = self.cache_root() else {
            ui.set_cache_status("UNAVAILABLE".into());
            ui.set_cache_detail("Save the project to enable its derived cache".into());
            return;
        };
        match CacheLayout::new(root).inventory() {
            Ok(inventory) => {
                ui.set_cache_status(if pending { "CLEANING" } else { "READY" }.into());
                ui.set_cache_detail(
                    format!(
                        "{} artifacts · {}",
                        inventory.artifact_count,
                        format_bytes(inventory.total_bytes)
                    )
                    .into(),
                );
            }
            Err(error) => {
                ui.set_cache_status("FAILED".into());
                ui.set_cache_detail(error.to_string().into());
            }
        }
    }

    pub(super) fn clean_cache(&self, ui: &PrismStudio) -> Result<(), String> {
        if self
            .cache_eviction
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.pending_evictions() > 0)
        {
            return Err("Cache cleanup is already running".to_owned());
        }
        let root = self
            .cache_root()
            .ok_or_else(|| "Save the project before cleaning its cache".to_owned())?;
        let worker = CacheEvictionWorker::spawn(CacheLayout::new(root), 1);
        worker
            .try_submit(CacheEvictionPolicy {
                max_bytes: Some(0),
                max_age: None,
            })
            .map_err(|error| error.to_string())?;
        *self.cache_eviction.borrow_mut() = Some(worker);
        ui.set_cache_cleaning(true);
        ui.set_cache_status("CLEANING".into());
        ui.set_feedback("Cleaning project derived cache in the background…".into());
        Ok(())
    }

    pub(super) fn find_media_in_timeline(&self, ui: &PrismStudio) -> Result<(), String> {
        let target = {
            let project = self.project.borrow();
            let project = project.as_ref().ok_or("Open a project first")?;
            let media_id = project
                .media
                .keys()
                .nth(
                    usize::try_from(ui.get_selected_media_index())
                        .map_err(|_| "Select source footage first")?,
                )
                .ok_or("Selected media is no longer available")?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or("No sequence available")?;
            let uses = sequence
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .enumerate()
                .filter(|(_, clip)| clip.media_id == *media_id)
                .map(|(index, clip)| (index, clip.timeline_start))
                .collect::<Vec<_>>();
            uses.iter()
                .find(|(index, _)| {
                    i32::try_from(*index).is_ok_and(|index| index > ui.get_selected_clip_index())
                })
                .or_else(|| uses.first())
                .copied()
                .ok_or("This source is not used in the active sequence")?
        };
        self.stop_playback(ui);
        self.select_clip(
            ui,
            i32::try_from(target.0).map_err(|error| error.to_string())?,
        );
        self.seek_time(ui, target.1);
        ui.set_active_workspace("EDIT".into());
        ui.set_inspector_open(true);
        ui.set_inspector_tab("Properties".into());
        ui.set_inspector_section("Clip".into());
        ui.set_feedback("Located source usage · choose Find again for the next occurrence".into());
        Ok(())
    }

    pub(super) fn select_media(&self, ui: &PrismStudio, index: i32) {
        let project = self.project.borrow();
        let Some(project) = project.as_ref() else {
            return;
        };
        let Ok(media_index) = usize::try_from(index) else {
            return;
        };
        let Some(asset) = project.media.values().nth(media_index) else {
            return;
        };
        self.proxy_availability.borrow_mut().remove(&asset.id);
        ui.set_selected_media_index(index);
        ui.set_selected_media_uses(
            i32::try_from(*media_usage(project).get(&asset.id).unwrap_or(&0)).unwrap_or(i32::MAX),
        );
        ui.set_selected_media(
            Path::new(&asset.source_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&asset.source_path)
                .into(),
        );
        self.source_position.set(RationalTime::ZERO);
        self.source_duration.set(RationalTime::ZERO);
        self.source_in.set(RationalTime::ZERO);
        self.source_out.set(RationalTime::ZERO);
        ui.set_source_ready(false);
        ui.set_source_preview_size("".into());
        ui.set_source_file_status(
            match std::fs::metadata(&asset.source_path) {
                Ok(metadata) if metadata.is_file() => {
                    format!("Source online · {}", format_bytes(metadata.len()))
                }
                _ => "Source offline · locate the original file".to_owned(),
            }
            .into(),
        );
        self.update_source_labels(ui);
        ui.set_has_source_frame(false);
        ui.set_selected_source_path(asset.source_path.clone().into());
        ui.set_source_detail(asset.source_path.clone().into());
        ui.set_selected_proxy(asset.proxy_path.clone().unwrap_or_default().into());
        let status = proxy_status(
            Path::new(&asset.source_path),
            asset.proxy_path.as_deref().map(Path::new),
            asset.proxy_source_fingerprint.as_ref(),
        );
        self.proxy_availability
            .borrow_mut()
            .insert(asset.id, status == PROXY_AVAILABLE);
        ui.set_proxy_status(status.into());
        if status != PROXY_AVAILABLE {
            self.use_proxy.set(false);
            ui.set_proxy_active(false);
        }
        let (has_video, has_audio, source_duration) = media_stream_summary(project, asset.id);
        if !has_video && has_audio {
            self.source_preview.invalidate();
            let duration = source_duration.unwrap_or(RationalTime::ZERO);
            self.source_duration.set(duration);
            self.source_out.set(duration);
            let available = Path::new(&asset.source_path).is_file() && duration.is_positive();
            ui.set_source_ready(available);
            ui.set_source_preview_size(
                format!("Audio source · {}", self.display_timecode(duration)).into(),
            );
            ui.set_source_detail("Audio source · waveform available in timeline".into());
            self.update_source_labels(ui);
            return;
        }
        let preview_path = if self.use_proxy.get() && status == PROXY_AVAILABLE {
            asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
        } else {
            &asset.source_path
        };
        self.source_preview
            .request(PathBuf::from(preview_path), RationalTime::ZERO);
    }

    pub(super) fn proxy_is_available(&self, asset: &MediaAsset) -> bool {
        if let Some(available) = self.proxy_availability.borrow().get(&asset.id) {
            return *available;
        }
        let available = proxy_status(
            Path::new(&asset.source_path),
            asset.proxy_path.as_deref().map(Path::new),
            asset.proxy_source_fingerprint.as_ref(),
        ) == PROXY_AVAILABLE;
        self.proxy_availability
            .borrow_mut()
            .insert(asset.id, available);
        available
    }

    pub(super) fn match_frame(&self, ui: &PrismStudio) -> Result<(), String> {
        let (media_index, time) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| {
                matched_source(project, ui.get_selected_clip_index(), self.playhead.get())
            })
            .ok_or_else(|| "Select a timeline clip to match its source frame".to_owned())?;
        let index = i32::try_from(media_index).map_err(|error| error.to_string())?;
        self.stop_playback(ui);
        self.select_media(ui, index);
        let source = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| {
                let asset = project.media.values().nth(media_index).cloned()?;
                let (has_video, _, _) = media_stream_summary(project, asset.id);
                Some((asset, has_video))
            })
            .ok_or_else(|| "Selected source is no longer available".to_owned())?;
        let (asset, has_video) = source;
        self.source_position.set(time);
        self.update_source_labels(ui);
        if has_video {
            let path = if self.use_proxy.get() && self.proxy_is_available(&asset) {
                asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
            } else {
                &asset.source_path
            };
            self.source_preview.request(PathBuf::from(path), time);
        }
        ui.set_inspector_open(true);
        ui.set_inspector_tab("Properties".into());
        ui.set_inspector_section("Source".into());
        ui.set_feedback(
            format!(
                "Matched {} at {}",
                if has_video {
                    "source frame"
                } else {
                    "audio source"
                },
                self.display_timecode(time)
            )
            .into(),
        );
        Ok(())
    }

    pub(super) fn select_clip(&self, ui: &PrismStudio, index: i32) {
        ui.set_selected_clip_index(index);
        self.update_inspector(ui);
        let selected = self.project.borrow().as_ref().and_then(|p| {
            let sequence = p.sequences.values().next()?;
            let clip = sequence
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .nth(usize::try_from(index).ok()?)?;
            let media_index = p.media.keys().position(|id| *id == clip.media_id)?;
            Some((clip.timeline_start, media_index))
        });
        if let Some((time, media_index)) = selected {
            if let Ok(index) = i32::try_from(media_index) {
                ui.set_selected_media_index(index);
                if let Some(name) = slint::Model::row_data(&ui.get_media_items(), media_index) {
                    ui.set_selected_media(name);
                }
                self.select_media(ui, index);
            }
            let _ = time;
        }
    }

    pub(super) fn seek_ratio(&self, ui: &PrismStudio, ratio: f32) {
        self.stop_playback(ui);
        if !ratio.is_finite() {
            return;
        }
        let duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next()?.duration().ok());
        if let Some(duration) = duration {
            // Pointer coordinates are display values; quantize once at the UI boundary.
            let time = self
                .quantize_to_frame(duration.as_seconds_f64() * f64::from(ratio.clamp(0.0, 1.0)));
            self.seek_time(ui, time);
        }
    }

    pub(super) fn seek_time(&self, ui: &PrismStudio, time: RationalTime) {
        let project = self.project.borrow();
        let sequence = project.as_ref().and_then(|p| p.sequences.values().next());
        let duration = sequence
            .and_then(|s| s.duration().ok())
            .unwrap_or(RationalTime::ZERO);
        let time = time.min(duration).max(RationalTime::ZERO);
        self.playhead.set(time);
        ui.set_timecode(self.display_timecode(time).into());
        #[allow(clippy::cast_possible_truncation)]
        let ratio = if duration.is_positive() {
            (time.as_seconds_f64() / duration.as_seconds_f64()) as f32
        } else {
            0.0
        };
        ui.set_playhead_ratio(ratio);
        let span = 1.0 / ui.get_timeline_zoom().clamp(1.0, 8.0);
        let offset = ui.get_timeline_offset();
        if ratio < offset || ratio > offset + span {
            ui.set_timeline_offset((ratio - span * 0.15).clamp(0.0, 1.0 - span));
            self.update_ruler(ui);
        }
        let target = sequence.and_then(|s| {
            s.tracks
                .iter()
                .filter(|t| t.visible && t.kind == TrackKind::Video)
                .flat_map(|t| &t.clips)
                .find_map(|clip| {
                    let end = clip.timeline_start.checked_add(clip.source_duration).ok()?;
                    if time < clip.timeline_start || time >= end {
                        return None;
                    }
                    let asset = project.as_ref()?.media.get(&clip.media_id)?;
                    let source_time = time
                        .checked_sub(clip.timeline_start)
                        .ok()?
                        .checked_add(clip.source_start)
                        .ok()?;
                    let proxy_is_available = self.proxy_is_available(asset);
                    let path = if self.use_proxy.get() && proxy_is_available {
                        asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
                    } else {
                        &asset.source_path
                    };
                    Some((PathBuf::from(path), source_time))
                })
        });
        if let Some((path, time)) = target {
            if self.playing.get() {
                // Keep the last completed frame visible while the next one decodes.
                self.program_preview
                    .request_playback(path, time, self.frame_rate());
            } else {
                ui.set_has_program_frame(false);
                self.program_preview.request(path, time);
            }
            ui.set_preview_status("Decoding frame…".into());
        } else {
            ui.set_has_program_frame(false);
            ui.set_program_luma_scope(slint::ModelRc::new(
                slint::VecModel::from(Vec::<f32>::new()),
            ));
            ui.set_program_luma_waveform(slint::ModelRc::new(slint::VecModel::from(Vec::<
                LumaScopeColumn,
            >::new(
            ))));
            ui.set_program_vectorscope(slint::ModelRc::new(slint::VecModel::from(
                Vec::<f32>::new(),
            )));
            self.program_preview.invalidate();
            ui.set_preview_status(
                if duration.is_positive() {
                    "Gap / no visible video at playhead"
                } else {
                    "Import footage to create a sequence"
                }
                .into(),
            );
        }
    }

    pub(super) fn stop_audio_preview(&self) {
        self.audio_clip.set(None);
        if let Some(mut child) = self.audio_process.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.audio_decoder.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.audio_end.set(RationalTime::ZERO);
        self.audio_clock.borrow_mut().take();
    }

    pub(super) fn stop_playback(&self, ui: &PrismStudio) {
        self.program_preview.invalidate();
        self.playing.set(false);
        self.playback_started.set(None);
        self.stop_audio_preview();
        ui.set_playing(false);
        ui.set_playback_buffering(false);
    }

    pub(super) fn start_audio_playback(&self) -> Result<(), String> {
        self.stop_audio_preview();
        let plan = self
            .project
            .borrow()
            .as_ref()
            .map(|project| crate::audio_preview::plan(project, self.playhead.get()))
            .transpose()?
            .flatten();
        let Some(plan) = plan else {
            return Ok(());
        };
        let end = self
            .playhead
            .get()
            .checked_add(plan.duration)
            .map_err(|error| error.to_string())?;
        let (decoder, player, clock) =
            crate::audio_preview::start(&plan, &media_tool("ffmpeg"), &media_tool("ffplay"))?;
        self.audio_end.set(end);
        *self.audio_clock.borrow_mut() = Some(clock);
        *self.audio_decoder.borrow_mut() = Some(decoder);
        *self.audio_process.borrow_mut() = Some(player);
        Ok(())
    }

    pub(super) fn follow_audio_timeline(&self, ui: &PrismStudio) {
        // The decoder/player pair stays alive across cuts and silence gaps.
        self.audio_clip.set(
            self.project
                .borrow()
                .as_ref()
                .and_then(|project| audio_preview_segment(project, self.playhead.get()))
                .map(|segment| segment.clip_id),
        );
        let exited = self
            .audio_process
            .borrow_mut()
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        let remaining = self
            .audio_end
            .get()
            .checked_sub(self.playhead.get())
            .unwrap_or(RationalTime::ZERO);
        if let Some(status) = exited {
            let completed = status.success() && remaining.as_seconds_f64() <= 0.25;
            let end = self.audio_end.get();
            self.stop_playback(ui);
            if completed {
                self.seek_time(ui, end);
            } else {
                ui.set_feedback(
                    "Audio preview stopped early · check source audio and playback device".into(),
                );
            }
        }
    }
}
