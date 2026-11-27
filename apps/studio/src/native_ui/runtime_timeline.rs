// Runtime timeline and polling methods.
use super::*;

impl NativeExportRuntime {
    pub(super) fn update_companion_selection(ui: &PrismStudio, project: Option<&Project>) {
        ui.set_selected_companion_index(
            project
                .and_then(|project| {
                    aligned_av_companion_index(project, ui.get_selected_clip_index())
                })
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1),
        );
    }

    pub(super) fn update_inspector(&self, ui: &PrismStudio) {
        let project = self.project.borrow();
        Self::update_companion_selection(ui, project.as_ref());
        let pair = project
            .as_ref()
            .ok_or_else(|| "Select a timeline clip".to_owned())
            .and_then(|project| {
                aligned_av_commands(
                    project,
                    ui.get_selected_clip_index(),
                    "timeline.delete_av",
                    self.playhead.get(),
                )
            });
        ui.set_can_edit_av(pair.is_ok());
        ui.set_aligned_av_detail(
            match pair {
                Ok(_) => {
                    "Matching source and timing · both clips will be edited together".to_owned()
                }
                Err(reason) => reason,
            }
            .into(),
        );
        let selected = project.as_ref().and_then(|p| {
            let sequence = p.sequences.values().next()?;
            let (row, track, clip) = sequence
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(row, track)| track.clips.iter().map(move |clip| (row, track, clip)))
                .nth(usize::try_from(ui.get_selected_clip_index()).ok()?)?;
            Some((p, sequence, row, track, clip))
        });
        if let Some((project, sequence, row, track, clip)) = selected {
            let path = project
                .media
                .get(&clip.media_id)
                .map_or("Offline media", |m| m.source_path.as_str());
            ui.set_inspector_name(
                Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path)
                    .into(),
            );
            ui.set_inspector_path(path.into());
            ui.set_inspector_track(
                format!(
                    "{}{}",
                    if track.kind == TrackKind::Audio {
                        "A"
                    } else {
                        "V"
                    },
                    row + 1
                )
                .into(),
            );
            ui.set_inspector_locked(track.locked);
            ui.set_selected_is_audio(track.kind == TrackKind::Audio);
            let waveform = self
                .waveforms
                .borrow()
                .get(path)
                .cloned()
                .unwrap_or_default();
            ui.set_selected_waveform(slint::ModelRc::new(slint::VecModel::from(waveform)));
            ui.set_selected_track_index(i32::try_from(row).unwrap_or(-1));
            ui.set_clip_start(timing_text(clip.timeline_start));
            ui.set_paired_start(timing_text(clip.timeline_start));
            ui.set_paired_source_in(timing_text(clip.source_start));
            ui.set_paired_duration(timing_text(clip.source_duration));
            ui.set_clip_source_in(timing_text(clip.source_start));
            ui.set_clip_duration(timing_text(clip.source_duration));
            let next = track
                .clips
                .iter()
                .position(|item| item.id == clip.id)
                .and_then(|index| track.clips.get(index + 1));
            ui.set_selected_has_next(track.kind == TrackKind::Video && next.is_some());
            ui.set_selected_has_transition(next.is_some_and(|next| {
                sequence.transitions.iter().any(|transition| {
                    transition.outgoing_clip == clip.id && transition.incoming_clip == next.id
                })
            }));
        } else {
            ui.set_inspector_name("".into());
            ui.set_inspector_path("".into());
            ui.set_inspector_locked(false);
            ui.set_selected_is_audio(false);
            ui.set_selected_waveform(slint::ModelRc::new(
                slint::VecModel::from(Vec::<f32>::new()),
            ));
            ui.set_selected_track_index(-1);
            ui.set_selected_has_next(false);
            ui.set_selected_has_transition(false);
        }
    }

    pub(super) fn apply_timing(&self, ui: &PrismStudio) -> Result<(), String> {
        let start = parse_timing(ui.get_clip_start().as_str())?;
        let source_start = parse_timing(ui.get_clip_source_in().as_str())?;
        let duration = parse_timing(ui.get_clip_duration().as_str())?;
        if !duration.is_positive() {
            return Err("Duration must be greater than zero".to_owned());
        }
        let commands = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "No project available".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip) = sequence
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                .nth(
                    usize::try_from(ui.get_selected_clip_index())
                        .map_err(|_| "Select a clip first".to_owned())?,
                )
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let asset = project
                .media
                .get(&clip.media_id)
                .ok_or_else(|| "Media unavailable".to_owned())?;
            let metadata = FfprobeInspector::new(media_tool("ffprobe"))
                .inspect(Path::new(&asset.source_path))
                .map_err(|e| e.to_string())?;
            let available = metadata
                .duration
                .ok_or_else(|| "Source duration unavailable".to_owned())?;
            if source_start
                .checked_add(duration)
                .map_err(|e| e.to_string())?
                > available
            {
                return Err("The requested trim exceeds the source duration".to_owned());
            }
            if aligned_av_companion_index(project, ui.get_selected_clip_index()).is_some() {
                aligned_av_range_commands(
                    project,
                    ui.get_selected_clip_index(),
                    start,
                    source_start,
                    duration,
                    available,
                )?
            } else {
                vec![Command::TrimClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    timeline_start: start,
                    source_start,
                    source_duration: duration,
                }]
            }
        };
        self.commit_commands(ui, commands, "Change clip timing")
    }

    pub(super) fn adjust_clip_frame(
        &self,
        ui: &PrismStudio,
        operation: &str,
    ) -> Result<(), String> {
        let frame = self.frame_duration();
        if matches!(operation, "timeline.move_left" | "timeline.move_right") {
            let current = parse_timing(ui.get_clip_start().as_str())?;
            let timeline_start = if operation == "timeline.move_left" {
                current
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?
            } else {
                current
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?
            };
            if timeline_start.is_negative() {
                return Err("A clip cannot move before sequence start".to_owned());
            }
            let commands = {
                let project = self.project.borrow();
                let project = project
                    .as_ref()
                    .ok_or_else(|| "No project available".to_owned())?;
                move_selected_commands(project, ui.get_selected_clip_index(), timeline_start)?
            };
            return self.commit_commands(ui, commands, "Move clip one frame");
        }

        let original_start = ui.get_clip_start();
        let original_source = ui.get_clip_source_in();
        let original_duration = ui.get_clip_duration();
        let mut start = parse_timing(original_start.as_str())?;
        let mut source = parse_timing(original_source.as_str())?;
        let mut duration = parse_timing(original_duration.as_str())?;
        match operation {
            "timeline.trim_in_left" => {
                start = start
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                if start.is_negative() || source.is_negative() {
                    return Err("No earlier source frame is available".to_owned());
                }
            }
            "timeline.trim_in_right" => {
                start = start
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
            }
            "timeline.trim_out_left" => {
                duration = duration
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
            }
            "timeline.trim_out_right" => {
                duration = duration
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("Unknown clip adjustment".to_owned()),
        }
        if !duration.is_positive() {
            return Err("A clip must remain at least one frame long".to_owned());
        }
        ui.set_clip_start(timing_text(start));
        ui.set_clip_source_in(timing_text(source));
        ui.set_clip_duration(timing_text(duration));
        if let Err(error) = self.apply_timing(ui) {
            ui.set_clip_start(original_start);
            ui.set_clip_source_in(original_source);
            ui.set_clip_duration(original_duration);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn precision_edit_frame(
        &self,
        ui: &PrismStudio,
        operation: &str,
    ) -> Result<(), String> {
        let frame = self.frame_duration();
        let delta = if operation.ends_with("_left") {
            RationalTime::ZERO
                .checked_sub(frame)
                .map_err(|error| error.to_string())?
        } else {
            frame
        };
        let kind = if operation.contains("slip") {
            "slip"
        } else if operation.contains("roll_in") {
            "roll_in"
        } else if operation.contains("roll_out") {
            "roll_out"
        } else {
            "slide"
        };
        self.precision_edit_delta(ui, kind, delta)
    }

    pub(super) fn precision_edit_delta(
        &self,
        ui: &PrismStudio,
        operation: &str,
        delta: RationalTime,
    ) -> Result<(), String> {
        let commands = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let selected = usize::try_from(ui.get_selected_clip_index())
                .map_err(|_| "Select a clip first".to_owned())?;
            let (track, clip_index) = sequence
                .tracks
                .iter()
                .flat_map(|track| (0..track.clips.len()).map(move |index| (track, index)))
                .nth(selected)
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let clip = &track.clips[clip_index];
            match operation {
                "slip" => slip_selected_commands(project, ui.get_selected_clip_index(), delta)?,
                "roll_in" => {
                    let previous = clip_index
                        .checked_sub(1)
                        .and_then(|index| track.clips.get(index))
                        .ok_or_else(|| {
                            "Roll In requires an adjacent clip before this clip".to_owned()
                        })?;
                    vec![Command::RollEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        left_clip_id: previous.id,
                        right_clip_id: clip.id,
                        delta,
                    }]
                }
                "roll_out" => {
                    let next = track.clips.get(clip_index + 1).ok_or_else(|| {
                        "Roll Out requires an adjacent clip after this clip".to_owned()
                    })?;
                    vec![Command::RollEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        left_clip_id: clip.id,
                        right_clip_id: next.id,
                        delta,
                    }]
                }
                "slide" => {
                    let previous = clip_index
                        .checked_sub(1)
                        .and_then(|index| track.clips.get(index))
                        .ok_or_else(|| "Slide requires clips on both sides".to_owned())?;
                    let next = track
                        .clips
                        .get(clip_index + 1)
                        .ok_or_else(|| "Slide requires clips on both sides".to_owned())?;
                    vec![Command::SlideEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        previous_clip_id: previous.id,
                        clip_id: clip.id,
                        next_clip_id: next.id,
                        delta,
                    }]
                }
                _ => return Err("Unknown precision edit".to_owned()),
            }
        };
        let label = if operation == "slip" {
            "Slip clip"
        } else if operation.starts_with("roll") {
            "Roll edit"
        } else {
            "Slide clip"
        };
        self.commit_commands(ui, commands, label)
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn drag_clip(
        &self,
        ui: &PrismStudio,
        index: i32,
        delta_ratio: f32,
        edge: &str,
    ) -> Result<(), String> {
        if !delta_ratio.is_finite() {
            return Err("Invalid timeline drag".to_owned());
        }
        ui.set_selected_clip_index(index);
        self.update_inspector(ui);
        let sequence_duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next()?.duration().ok())
            .ok_or_else(|| "No sequence available".to_owned())?;
        let delta = if ui.get_snap_enabled() {
            let frame = self.frame_duration();
            #[allow(clippy::cast_possible_truncation)]
            let frames = (sequence_duration.as_seconds_f64() * f64::from(delta_ratio)
                / frame.as_seconds_f64())
            .round() as i64;
            frame
                .checked_mul_i64(frames)
                .map_err(|error| error.to_string())?
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let ticks = (sequence_duration.as_seconds_f64() * f64::from(delta_ratio) * 300.0)
                .round() as i64;
            RationalTime::new(ticks, 300).map_err(|error| error.to_string())?
        };
        if delta.is_zero() {
            return Ok(());
        }
        if matches!(edge, "slip" | "slide" | "roll_in" | "roll_out") {
            return self.precision_edit_delta(ui, edge, delta);
        }
        if edge == "move" {
            let current = parse_timing(ui.get_clip_start().as_str())?;
            let target = current
                .checked_add(delta)
                .map_err(|error| error.to_string())?;
            if target.is_negative() {
                return Err("A clip cannot move before sequence start".to_owned());
            }
            let commands = {
                let project = self.project.borrow();
                let project = project
                    .as_ref()
                    .ok_or_else(|| "No project available".to_owned())?;
                let sequence = project
                    .sequences
                    .values()
                    .next()
                    .ok_or_else(|| "No sequence available".to_owned())?;
                let (track, clip) = sequence
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
                    .nth(usize::try_from(index).map_err(|_| "Select a clip first".to_owned())?)
                    .ok_or_else(|| "Selected clip unavailable".to_owned())?;
                let (has_video, has_audio, _) = media_stream_summary(project, clip.media_id);
                if has_video && has_audio {
                    aligned_av_commands(project, index, "timeline.move_av", target)?
                } else {
                    vec![Command::MoveClip {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        clip_id: clip.id,
                        timeline_start: target,
                    }]
                }
            };
            return self.commit_commands(ui, commands, "Drag clip");
        }

        let original_start = ui.get_clip_start();
        let original_source = ui.get_clip_source_in();
        let original_duration = ui.get_clip_duration();
        let mut start = parse_timing(original_start.as_str())?;
        let mut source = parse_timing(original_source.as_str())?;
        let mut duration = parse_timing(original_duration.as_str())?;
        match edge {
            "in" => {
                start = start
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_sub(delta)
                    .map_err(|error| error.to_string())?;
            }
            "out" => {
                duration = duration
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("Unknown timeline drag edge".to_owned()),
        }
        if start.is_negative() || source.is_negative() || !duration.is_positive() {
            return Err("The drag would create an invalid clip range".to_owned());
        }
        ui.set_clip_start(timing_text(start));
        ui.set_clip_source_in(timing_text(source));
        ui.set_clip_duration(timing_text(duration));
        let paired_result = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "No project available".to_owned())?;
            let clip = project
                .sequences
                .values()
                .next()
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .nth(usize::try_from(index).ok()?)
                })
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let (has_video, has_audio, _) = media_stream_summary(project, clip.media_id);
            if has_video && has_audio {
                let asset = project
                    .media
                    .get(&clip.media_id)
                    .ok_or_else(|| "Media unavailable".to_owned())?;
                let available = FfprobeInspector::new(media_tool("ffprobe"))
                    .inspect(Path::new(&asset.source_path))
                    .map_err(|error| error.to_string())?
                    .duration
                    .ok_or_else(|| "Source duration unavailable".to_owned())?;
                Some(aligned_av_range_commands(
                    project, index, start, source, duration, available,
                )?)
            } else {
                None
            }
        };
        let result = if let Some(commands) = paired_result {
            self.commit_commands(ui, commands, "Drag aligned A/V")
        } else {
            self.apply_timing(ui)
        };
        if let Err(error) = result {
            ui.set_clip_start(original_start);
            ui.set_clip_source_in(original_source);
            ui.set_clip_duration(original_duration);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn update_ruler(&self, ui: &PrismStudio) {
        let duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next()?.duration().ok())
            .unwrap_or(RationalTime::ZERO);
        let zoom = f64::from(ui.get_timeline_zoom()).clamp(1.0, 8.0);
        let offset = f64::from(ui.get_timeline_offset()).clamp(0.0, 1.0 - 1.0 / zoom);
        let labels = (0..6)
            .map(|index| {
                self.display_timecode(self.quantize_to_frame(
                    duration.as_seconds_f64() * (offset + f64::from(index) / (5.0 * zoom)),
                ))
                .into()
            })
            .collect::<Vec<slint::SharedString>>();
        ui.set_ruler_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
    }

    pub(super) fn update_source_labels(&self, ui: &PrismStudio) {
        ui.set_source_timecode(self.display_timecode(self.source_position.get()).into());
        ui.set_source_duration_label(self.display_timecode(self.source_duration.get()).into());
        ui.set_source_in_label(self.display_timecode(self.source_in.get()).into());
        ui.set_source_out_label(self.display_timecode(self.source_out.get()).into());
        #[allow(clippy::cast_possible_truncation)]
        let ratio = if self.source_duration.get().is_positive() {
            (self.source_position.get().as_seconds_f64()
                / self.source_duration.get().as_seconds_f64()) as f32
        } else {
            0.0
        };
        ui.set_source_ratio(ratio);
        let duration = self.source_duration.get().as_seconds_f64();
        #[allow(clippy::cast_possible_truncation)]
        let (start, end) = if duration > 0.0 {
            (
                (self.source_in.get().as_seconds_f64() / duration) as f32,
                (self.source_out.get().as_seconds_f64() / duration) as f32,
            )
        } else {
            (0.0, 0.0)
        };
        ui.set_source_in_ratio(start.clamp(0.0, 1.0));
        ui.set_source_out_ratio(end.clamp(0.0, 1.0));
        ui.set_source_range_label(
            self.source_out
                .get()
                .checked_sub(self.source_in.get())
                .map_or_else(|_| "—".into(), display_timecode)
                .into(),
        );
    }

    pub(super) fn seek_source(&self, ui: &PrismStudio, ratio: f32) {
        if !ratio.is_finite() || !self.source_duration.get().is_positive() {
            return;
        }
        let asset = self.project.borrow().as_ref().and_then(|project| {
            let asset = project
                .media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).ok()?)
                .cloned()?;
            let (has_video, _, _) = media_stream_summary(project, asset.id);
            Some((asset, has_video))
        });
        if let Some((asset, has_video)) = asset {
            let time = self.quantize_to_frame(
                self.source_duration.get().as_seconds_f64() * f64::from(ratio.clamp(0.0, 1.0)),
            );
            {
                let last_frame = self
                    .source_duration
                    .get()
                    .checked_sub(self.frame_duration())
                    .unwrap_or(RationalTime::ZERO)
                    .max(RationalTime::ZERO);
                let time = time.min(last_frame);
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
            }
        }
    }

    pub(super) fn edit_source(&self, ui: &PrismStudio, overwrite: bool) -> Result<(), String> {
        if !ui.get_source_ready() {
            return Err("Select an available source and mark its range first".into());
        }
        let duration = self
            .source_out
            .get()
            .checked_sub(self.source_in.get())
            .map_err(|e| e.to_string())?;
        let position = self.playhead.get();
        let commands = {
            let project = self.project.borrow();
            let project = project.as_ref().ok_or("Open a project first")?;
            let asset = project
                .media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).map_err(|e| e.to_string())?)
                .ok_or("Select a source first")?;
            source_edit_commands(
                project,
                asset.id,
                self.source_in.get(),
                duration,
                position,
                overwrite,
            )?
        };
        self.commit_commands(
            ui,
            commands,
            if overwrite {
                "Overwrite source range"
            } else {
                "Insert source range"
            },
        )?;
        ui.set_selected_clip_index(-1);
        self.update_inspector(ui);
        self.seek_time(
            ui,
            position.checked_add(duration).map_err(|e| e.to_string())?,
        );
        Ok(())
    }

    pub(super) fn append_source(&self, ui: &PrismStudio) -> Result<(), String> {
        if !ui.get_source_ready() {
            return Err("Select an available source clip first".to_owned());
        }
        let duration = self
            .source_out
            .get()
            .checked_sub(self.source_in.get())
            .map_err(|e| e.to_string())?;
        if !duration.is_positive() {
            return Err("Mark a positive source range".to_owned());
        }
        let commands = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let media = project
                .media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).map_err(|e| e.to_string())?)
                .ok_or_else(|| "Select source footage".to_owned())?;
            append_source_commands(project, media.id, self.source_in.get(), duration)?
        };
        self.commit_commands(ui, commands, "Append source range")
    }

    #[allow(clippy::collapsible_if, clippy::too_many_lines)]
    pub(super) fn poll(&self, ui: &PrismStudio) {
        let cache_result = self
            .cache_eviction
            .borrow()
            .as_ref()
            .and_then(|worker| worker.try_result().ok());
        if let Some(result) = cache_result {
            self.cache_eviction.borrow_mut().take();
            match result.result {
                Ok(report) => ui.set_feedback(
                    format!(
                        "Cache cleanup removed {} artifacts ({})",
                        report.removed_files,
                        format_bytes(report.removed_bytes)
                    )
                    .into(),
                ),
                Err(error) => ui.set_feedback(format!("Cache cleanup failed: {error}").into()),
            }
            self.update_cache_state(ui);
        }
        loop {
            let result = self
                .autosave_scheduler
                .borrow()
                .as_ref()
                .and_then(prism_persistence::AutosaveScheduler::try_result);
            let Some(result) = result else {
                break;
            };
            match result.result {
                Ok(()) => {
                    ui.set_autosave_status(format!("SAVED R{}", result.revision).into());
                    if let Some(path) = self.project_path.borrow().as_deref() {
                        if let Err(error) = prism_persistence::retain_autosaves(
                            path,
                            self.autosave_preferences.get().retained_revisions,
                        ) {
                            ui.set_feedback(format!("Autosave retention failed: {error}").into());
                        }
                    }
                    self.refresh_recovery(ui);
                }
                Err(error) => {
                    ui.set_autosave_status("FAILED".into());
                    ui.set_feedback(format!("Autosave failed: {error}").into());
                }
            }
        }
        self.retry_pending_autosave(ui);
        let proxy_result = self
            .proxy_generation
            .borrow()
            .as_ref()
            .and_then(|generation| {
                generation.receiver.try_recv().ok().map(|result| {
                    (
                        generation.media_id,
                        generation.source_fingerprint.clone(),
                        result,
                    )
                })
            });
        if let Some((media_id, source_fingerprint, result)) = proxy_result {
            self.proxy_generation.borrow_mut().take();
            ui.set_proxy_generating(false);
            match result {
                Ok(path) => {
                    let command = Command::SetMediaProxy {
                        media_id,
                        proxy_path: Some(path.display().to_string()),
                        source_fingerprint: Some(source_fingerprint),
                    };
                    if let Err(error) = self.commit(ui, command, "Generate media proxy") {
                        ui.set_feedback(error.into());
                    } else {
                        self.use_proxy.set(true);
                        ui.set_proxy_active(true);
                        let index = self
                            .project
                            .borrow()
                            .as_ref()
                            .and_then(|project| project.media.keys().position(|id| *id == media_id))
                            .and_then(|index| i32::try_from(index).ok())
                            .unwrap_or(-1);
                        self.select_media(ui, index);
                    }
                }
                Err(error) => ui.set_feedback(error.into()),
            }
        }
        if self.playing.get() {
            if let Some(started) = self.playback_started.get() {
                let observed = self
                    .audio_clock
                    .borrow()
                    .as_ref()
                    .map(crate::audio_clock::AudioClock::position);
                ui.set_playback_buffering(observed == Some(None));
                let delta = match observed {
                    Some(position) => position,
                    None => RationalTime::new(
                        i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
                        1000,
                    )
                    .ok(),
                };
                if let Some(delta) = delta {
                    if let Ok(target) = self.playback_origin.get().checked_add(delta) {
                        self.seek_time(ui, target);
                        if ui.get_playhead_ratio() >= 1.0 {
                            self.stop_playback(ui);
                        }
                    }
                } else if started.elapsed() > Duration::from_secs(5) {
                    self.stop_playback(ui);
                    ui.set_feedback("Audio playback clock unavailable · playback stopped".into());
                }
                let stalled = self
                    .audio_clock
                    .borrow()
                    .as_ref()
                    .is_some_and(crate::audio_clock::AudioClock::stalled);
                if self.playing.get() && stalled {
                    self.stop_playback(ui);
                    ui.set_feedback("Audio playback clock stalled · playback stopped".into());
                } else if self.playing.get() {
                    self.follow_audio_timeline(ui);
                }
            }
        }
        let mut cache_changed = false;
        loop {
            let cache_result = self
                .media_artifacts
                .borrow()
                .as_ref()
                .and_then(|worker| worker.try_result().ok());
            let Some(cache_result) = cache_result else {
                break;
            };
            cache_changed = true;
            match cache_result.result {
                Ok(_) | Err(CacheError::AlreadyExists(_)) => {}
                Err(error) => {
                    ui.set_feedback(format!("Could not cache media preview: {error}").into());
                }
            }
        }
        if cache_changed {
            self.update_cache_state(ui);
            self.update_media_task_state(ui);
        }
        let mut waveform_changed = false;
        loop {
            match self.waveform_worker.borrow().try_result() {
                Ok(Ok(result)) => {
                    waveform_changed = true;
                    self.waveforms.borrow_mut().insert(
                        result.path.display().to_string(),
                        waveform_bars(&result.points, 96),
                    );
                    self.waveform_completed
                        .set(self.waveform_completed.get().saturating_add(1));
                }
                Ok(Err(error)) => {
                    waveform_changed = true;
                    self.waveform_completed
                        .set(self.waveform_completed.get().saturating_add(1));
                    self.waveform_failed
                        .set(self.waveform_failed.get().saturating_add(1));
                    ui.set_feedback(format!("Could not prepare audio waveform: {error}").into());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if self.waveform_worker.borrow().pending() > 0 {
                        ui.set_feedback("Audio waveform worker stopped unexpectedly".into());
                    }
                    break;
                }
            }
        }
        if waveform_changed {
            self.update_cache_state(ui);
            self.update_media_task_state(ui);
            self.refresh_timeline_waveforms(ui);
            self.update_inspector(ui);
        }
        if let Some(result) = self.thumbnail_preview.poll_decoded() {
            if let Some(index) = self.thumbnail_current.take() {
                if let Some(card) = self.cards.borrow_mut().get_mut(index) {
                    if let Ok((frame, _, source_fingerprint, cache_hit)) = result {
                        if let Some(worker) = self.media_artifacts.borrow().as_ref()
                            && let Some(asset) = self
                                .project
                                .borrow()
                                .as_ref()
                                .and_then(|project| project.media.values().nth(index))
                        {
                            let entry = MediaCatalogEntry {
                                path: PathBuf::from(&asset.source_path),
                                kind: MediaItemKind::Video,
                                metadata: None,
                                fingerprint: source_fingerprint,
                                offline: false,
                                diagnostic: None,
                            };
                            if !cache_hit
                                && let Err(error) = worker.schedule_thumbnail_variant(
                                    &entry,
                                    &frame,
                                    frame.width,
                                    frame.height,
                                    BROWSER_THUMBNAIL_VARIANT,
                                )
                            {
                                ui.set_feedback(
                                    format!("Could not queue thumbnail cache: {error}").into(),
                                );
                            }
                        }
                        let buffer =
                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                &frame.rgba,
                                frame.width,
                                frame.height,
                            );
                        card.frame = slint::Image::from_rgba8(buffer);
                        card.has_frame = true;
                        card.status = "".into();
                    } else {
                        card.status = if card.offline {
                            "Source offline"
                        } else {
                            "Preview unavailable"
                        }
                        .into();
                        self.thumbnail_failed
                            .set(self.thumbnail_failed.get().saturating_add(1));
                    }
                }
                self.thumbnail_completed
                    .set(self.thumbnail_completed.get().saturating_add(1));
                self.filter_cards(ui);
                self.next_thumbnail(ui);
            }
        }
        if let Some(frame) = self.program_preview.poll_decoded() {
            match frame {
                Ok((frame, _, _, _)) => {
                    ui.set_program_luma_scope(slint::ModelRc::new(slint::VecModel::from(
                        luma_scope(&frame.rgba, 32, 65_536),
                    )));
                    ui.set_program_luma_waveform(slint::ModelRc::new(slint::VecModel::from(
                        luma_waveform(&frame.rgba, frame.width, frame.height, 64, 65_536),
                    )));
                    ui.set_program_vectorscope(slint::ModelRc::new(slint::VecModel::from(
                        vectorscope(&frame.rgba, 16, 65_536),
                    )));
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &frame.rgba,
                        frame.width,
                        frame.height,
                    );
                    ui.set_program_frame(slint::Image::from_rgba8(buffer));
                    ui.set_has_program_frame(true);
                    ui.set_preview_status("Frame ready".into());
                }
                Err(error) => {
                    self.stop_playback(ui);
                    ui.set_has_program_frame(false);
                    ui.set_program_luma_scope(slint::ModelRc::new(slint::VecModel::from(
                        Vec::<f32>::new(),
                    )));
                    ui.set_program_luma_waveform(slint::ModelRc::new(slint::VecModel::from(
                        Vec::<LumaScopeColumn>::new(),
                    )));
                    ui.set_program_vectorscope(slint::ModelRc::new(slint::VecModel::from(Vec::<
                        f32,
                    >::new(
                    ))));
                    ui.set_preview_status(
                        "Preview unavailable · check FFmpeg and source media".into(),
                    );
                    ui.set_feedback(error.into());
                }
            }
        }
        if self.playing.get() && self.program_preview.timed_out(Duration::from_secs(5)) {
            self.stop_playback(ui);
            ui.set_preview_status("Video decoder timed out · playback stopped".into());
            ui.set_feedback(
                "Video decoder did not produce a frame within 5 seconds · playback stopped".into(),
            );
        }
        if let Some(frame) = self.source_preview.poll() {
            match frame {
                Ok((frame, duration)) => {
                    let size = frame.size();
                    ui.set_source_preview_size(
                        format!(
                            "{} · {} × {} · {}",
                            if ui.get_proxy_active() {
                                "Proxy preview"
                            } else {
                                "Source preview"
                            },
                            size.width,
                            size.height,
                            self.display_timecode(duration)
                        )
                        .into(),
                    );
                    ui.set_source_frame(frame);
                    ui.set_has_source_frame(true);
                    if self.source_duration.get() != duration {
                        self.source_duration.set(duration);
                        self.source_out.set(duration);
                    }
                    ui.set_source_ready(duration.is_positive());
                    self.update_source_labels(ui);
                }
                Err(error) => {
                    ui.set_has_source_frame(false);
                    ui.set_source_ready(false);
                    ui.set_source_file_status(
                        if Path::new(ui.get_selected_source_path().as_str()).is_file() {
                            "Source found · preview unavailable"
                        } else {
                            "Source offline · choose Relink source"
                        }
                        .into(),
                    );
                    ui.set_source_detail(
                        "Preview unavailable · check FFmpeg and source media".into(),
                    );
                    ui.set_feedback(error.into());
                }
            }
        }
        let event = self
            .worker
            .borrow()
            .as_ref()
            .and_then(|worker| worker.try_next().ok().flatten());
        let Some(event) = event else {
            return;
        };
        let terminal = matches!(
            event,
            ExportWorkerEvent::Completed
                | ExportWorkerEvent::Cancelled
                | ExportWorkerEvent::Failed(_)
        );
        let mut controller = self.controller.borrow_mut();
        let already_cancelled = matches!(
            (controller.state(), &event),
            (
                Some(prism_export::ExportJobState::Cancelled),
                ExportWorkerEvent::Cancelled
            )
        );
        if !already_cancelled {
            let _ = controller.apply_worker_event(event);
        }
        if terminal {
            *self.worker.borrow_mut() = None;
        }
        apply_export_status(ui, controller.status().as_ref());
    }
}
