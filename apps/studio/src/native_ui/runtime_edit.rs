// Runtime editing methods.
use super::*;

impl NativeExportRuntime {
    pub(super) fn commit(
        &self,
        ui: &PrismStudio,
        command: Command,
        label: &str,
    ) -> Result<(), String> {
        self.commit_commands(ui, vec![command], label)
    }

    pub(super) fn selected_media_id(&self, ui: &PrismStudio) -> Result<MediaAssetId, String> {
        self.project
            .borrow()
            .as_ref()
            .and_then(|project| {
                project
                    .media
                    .keys()
                    .nth(usize::try_from(ui.get_selected_media_index()).ok()?)
                    .copied()
            })
            .ok_or_else(|| "Select media first".to_owned())
    }

    pub(super) fn assign_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let source = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| PathBuf::from(&asset.source_path))
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let Some(path) = rfd::FileDialog::new()
            .set_title("Assign Proxy Media")
            .add_filter("Video", &["mp4", "mov", "m4v", "mkv", "webm"])
            .pick_file()
        else {
            return Ok(());
        };
        let index = ui.get_selected_media_index();
        let source_fingerprint = fingerprint(&source).map_err(|error| error.to_string())?;
        self.commit(
            ui,
            Command::SetMediaProxy {
                media_id,
                proxy_path: Some(path.display().to_string()),
                source_fingerprint: Some(ProxySourceFingerprint {
                    sha256: source_fingerprint.sha256,
                    byte_length: source_fingerprint.byte_length,
                }),
            },
            "Assign media proxy",
        )?;
        self.use_proxy.set(true);
        ui.set_proxy_active(true);
        self.select_media(ui, index);
        Ok(())
    }

    pub(super) fn relink_source(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Relink Source Media")
            .add_filter(
                "Video",
                &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpeg", "mpg"],
            )
            .pick_file()
        else {
            return Ok(());
        };
        self.relink_source_to(ui, &path)
    }

    pub(super) fn relink_source_to(&self, ui: &PrismStudio, path: &Path) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let metadata = FfprobeInspector::new(media_tool("ffprobe"))
            .inspect(path)
            .map_err(|error| error.to_string())?;
        if !metadata
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
        {
            return Err("The replacement file has no video stream".to_owned());
        }
        let available = metadata
            .duration
            .filter(|duration| duration.is_positive())
            .ok_or_else(|| "The replacement file has no valid duration".to_owned())?;
        let required = self
            .project
            .borrow()
            .as_ref()
            .into_iter()
            .flat_map(|project| project.sequences.values())
            .flat_map(|sequence| &sequence.tracks)
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.media_id == media_id)
            .filter_map(|clip| clip.source_start.checked_add(clip.source_duration).ok())
            .max()
            .unwrap_or(RationalTime::ZERO);
        if available < required {
            return Err(format!(
                "Replacement is too short: clips require {}, file provides {}",
                timing_text(required),
                timing_text(available)
            ));
        }
        let index = ui.get_selected_media_index();
        self.commit(
            ui,
            Command::RelinkMedia {
                media_id,
                source_path: path.display().to_string(),
            },
            "Relink source media",
        )?;
        self.reset_thumbnails(ui);
        self.select_media(ui, index);
        self.update_inspector(ui);
        Ok(())
    }

    pub(super) fn clear_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let index = ui.get_selected_media_index();
        self.commit(
            ui,
            Command::SetMediaProxy {
                media_id,
                proxy_path: None,
                source_fingerprint: None,
            },
            "Clear media proxy",
        )?;
        self.use_proxy.set(false);
        ui.set_proxy_active(false);
        self.select_media(ui, index);
        Ok(())
    }

    pub(super) fn generate_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let source = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| PathBuf::from(&asset.source_path))
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let default_name = source
            .file_stem()
            .and_then(|name| name.to_str())
            .map_or_else(
                || "Prism.proxy.mp4".to_owned(),
                |name| format!("{name}.proxy.mp4"),
            );
        let Some(output) = rfd::FileDialog::new()
            .set_title("Generate Prism Proxy")
            .set_file_name(default_name)
            .add_filter("MP4 proxy", &["mp4"])
            .save_file()
        else {
            return Ok(());
        };
        self.start_proxy_generation(ui, media_id, source, output)
    }

    pub(super) fn regenerate_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let (source, base) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| {
                let source = PathBuf::from(&asset.source_path);
                let base = asset.proxy_path.as_ref().map_or_else(
                    || {
                        let stem = source
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Prism");
                        source.with_file_name(format!("{stem}.proxy.mp4"))
                    },
                    PathBuf::from,
                );
                (source, base)
            })
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let output = next_proxy_version(&base)?;
        self.start_proxy_generation(ui, media_id, source, output)
    }

    pub(super) fn start_proxy_generation(
        &self,
        ui: &PrismStudio,
        media_id: MediaAssetId,
        source: PathBuf,
        output: PathBuf,
    ) -> Result<(), String> {
        if self.proxy_generation.borrow().is_some() {
            return Err("A proxy is already being generated".to_owned());
        }
        let source_fingerprint = fingerprint(&source).map_err(|error| error.to_string())?;
        let profile = ProxyProfile::new(960, 540, "libx264", Some("aac".to_owned()), "mp4")
            .map_err(|error| error.to_string())?;
        let request = ProxyRequest {
            source,
            output,
            profile,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = FfmpegProxyGenerator::new(media_tool("ffmpeg"))
                .generate(&request, Some(&worker_cancelled))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        *self.proxy_generation.borrow_mut() = Some(ProxyGeneration {
            receiver,
            cancelled,
            media_id,
            source_fingerprint: ProxySourceFingerprint {
                sha256: source_fingerprint.sha256,
                byte_length: source_fingerprint.byte_length,
            },
        });
        ui.set_proxy_generating(true);
        ui.set_feedback("Generating 540p proxy in the background…".into());
        Ok(())
    }

    pub(super) fn commit_commands(
        &self,
        ui: &PrismStudio,
        commands: Vec<Command>,
        label: &str,
    ) -> Result<(), String> {
        let mut updated = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let outcome = CommandEngine::execute_transaction(&mut updated, label, commands)
            .map_err(|e| e.to_string())?;
        let path = self
            .project_path
            .borrow()
            .clone()
            .ok_or_else(|| "Save a project first".to_owned())?;
        // Publish to the UI only after the validated edit has been saved successfully.
        prism_persistence::save_atomic(&updated, &path).map_err(|e| e.to_string())?;
        let audio_changed = self.project.borrow().as_ref().and_then(|project| {
            crate::audio_preview::plan(project, self.playhead.get())
                .ok()
                .flatten()
        }) != crate::audio_preview::plan(&updated, self.playhead.get())
            .ok()
            .flatten();
        if self.playing.get() && audio_changed {
            self.stop_playback(ui);
        }
        apply_project_summary(ui, &updated, &path);
        self.refresh_timeline_waveforms(ui);
        *self.project.borrow_mut() = Some(updated);
        self.filter_cards(ui);
        if let Some(project) = self.project.borrow().as_ref() {
            self.queue_autosave(ui, project);
        }
        let mut undo = self.undo.borrow_mut();
        undo.push(outcome.undo);
        if undo.len() > 50 {
            undo.remove(0);
        }
        drop(undo);
        self.redo.borrow_mut().clear();
        self.update_history(ui);
        self.update_inspector(ui);
        self.update_ruler(ui);
        ui.set_feedback(format!("{label} · saved").into());
        self.seek_time(ui, self.playhead.get());
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn edit_aligned_av(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        let moving = command == "timeline.move_av";
        let trimming = command == "timeline.trim_av";
        let position = if moving {
            parse_timing(ui.get_paired_start().as_str())?
        } else {
            self.playhead.get()
        };
        let (commands, selected_id) = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let commands = if trimming {
                let index = ui.get_selected_clip_index();
                // Check pair eligibility before starting media inspection.
                aligned_av_commands(project, index, "timeline.move_av", RationalTime::ZERO)?;
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
                    .ok_or("Selected clip unavailable")?;
                let asset = project
                    .media
                    .get(&clip.media_id)
                    .ok_or("Media unavailable")?;
                let metadata = FfprobeInspector::new(media_tool("ffprobe"))
                    .inspect(Path::new(&asset.source_path))
                    .map_err(|error| error.to_string())?;
                let available = metadata
                    .duration
                    .ok_or("Original source duration unavailable")?;
                aligned_av_range_commands(
                    project,
                    index,
                    clip.timeline_start,
                    parse_timing(ui.get_paired_source_in().as_str())?,
                    parse_timing(ui.get_paired_duration().as_str())?,
                    available,
                )?
            } else {
                aligned_av_commands(project, ui.get_selected_clip_index(), command, position)?
            };
            let id = project
                .sequences
                .values()
                .next()
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .nth(usize::try_from(ui.get_selected_clip_index()).ok()?)
                })
                .map(|clip| clip.id);
            (commands, id)
        };
        let label = match command {
            "timeline.split_av" => "Split aligned A/V",
            "timeline.move_av" => "Move aligned A/V",
            "timeline.trim_av" => "Change aligned A/V source range",
            "timeline.ripple_delete_av" => "Ripple delete aligned A/V",
            _ => "Delete aligned A/V",
        };
        self.commit_commands(ui, commands, label)?;
        let selection = if moving || trimming {
            self.project
                .borrow()
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .position(|clip| Some(clip.id) == selected_id)
                })
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1)
        } else {
            -1
        };
        ui.set_selected_clip_index(selection);
        self.update_inspector(ui);
        if moving || trimming {
            self.stop_playback(ui);
            self.seek_time(
                ui,
                if moving {
                    position
                } else {
                    self.playhead.get()
                },
            );
        }
        Ok(())
    }

    pub(super) fn edit_clip(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        let index = usize::try_from(ui.get_selected_clip_index())
            .map_err(|_| "Select a clip first".to_owned())?;
        let operation = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip) = sequence
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                .nth(index)
                .ok_or_else(|| "Selected clip is no longer available".to_owned())?;
            match command {
                "timeline.split" => Command::SplitClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    right_clip_id: ClipId::new(),
                    timeline_position: self.playhead.get(),
                },
                "timeline.ripple_delete" => Command::RippleDelete {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                },
                _ => Command::DeleteClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                },
            }
        };
        // End the immutable project borrow before commit replaces the snapshot.
        let label = match command {
            "timeline.split" => "Split clip",
            "timeline.ripple_delete" => "Ripple delete clip",
            _ => "Delete clip",
        };
        self.commit(ui, operation, label)?;
        ui.set_selected_clip_index(-1);
        self.update_inspector(ui);
        Ok(())
    }

    pub(super) fn edit_clip_or_aligned(
        &self,
        command: &str,
        ui: &PrismStudio,
    ) -> Result<(), String> {
        let paired = self.project.borrow().as_ref().is_some_and(|project| {
            aligned_av_companion_index(project, ui.get_selected_clip_index()).is_some()
        });
        if paired {
            let aligned = match command {
                "timeline.split" => "timeline.split_av",
                "timeline.ripple_delete" => "timeline.ripple_delete_av",
                _ => "timeline.delete_av",
            };
            self.edit_aligned_av(aligned, ui)
        } else {
            self.edit_clip(command, ui)
        }
    }

    pub(super) fn add_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let (sequence_id, marker_number) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .map(|sequence| (sequence.id, sequence.markers.len() + 1))
            .ok_or_else(|| "No sequence available".to_owned())?;
        self.commit(
            ui,
            Command::AddMarker {
                sequence_id,
                id: MarkerId::new(),
                time: self.playhead.get(),
                label: format!("Marker {marker_number}"),
            },
            "Add timeline marker",
        )
    }

    pub(super) fn rename_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let label = ui.get_marker_label().trim().to_owned();
        if label.is_empty() {
            return Err("Give the marker a name before saving".to_owned());
        }
        let index = usize::try_from(ui.get_selected_marker_index())
            .map_err(|_| "Select a marker first".to_owned())?;
        let (sequence_id, marker) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next())
            .and_then(|s| s.markers.get(index).map(|m| (s.id, m.clone())))
            .ok_or_else(|| "Marker unavailable".to_owned())?;
        self.commit_commands(
            ui,
            vec![
                Command::DeleteMarker {
                    sequence_id,
                    marker_id: marker.id,
                },
                Command::AddMarker {
                    sequence_id,
                    id: marker.id,
                    time: marker.time,
                    label: label.clone(),
                },
            ],
            "Rename timeline marker",
        )?;
        let new_index = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next())
            .and_then(|s| s.markers.iter().position(|m| m.id == marker.id));
        ui.set_selected_marker_index(new_index.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1));
        ui.set_marker_label(label.into());
        Ok(())
    }

    pub(super) fn delete_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let index = usize::try_from(ui.get_selected_marker_index())
            .map_err(|_| "Select a marker first".to_owned())?;
        let (sequence_id, marker_id) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .and_then(|sequence| {
                sequence
                    .markers
                    .get(index)
                    .map(|marker| (sequence.id, marker.id))
            })
            .ok_or_else(|| "Selected marker is unavailable".to_owned())?;
        self.commit(
            ui,
            Command::DeleteMarker {
                sequence_id,
                marker_id,
            },
            "Delete timeline marker",
        )?;
        ui.set_selected_marker_index(-1);
        Ok(())
    }

    pub(super) fn toggle_dissolve(&self, ui: &PrismStudio) -> Result<(), String> {
        let selected = usize::try_from(ui.get_selected_clip_index())
            .map_err(|_| "Select a video clip first".to_owned())?;
        let operation = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip_index) = sequence
                .tracks
                .iter()
                .flat_map(|track| (0..track.clips.len()).map(move |index| (track, index)))
                .nth(selected)
                .ok_or_else(|| "Selected clip is unavailable".to_owned())?;
            if track.kind != TrackKind::Video {
                return Err("Dissolves require a video track".to_owned());
            }
            let outgoing = &track.clips[clip_index];
            let incoming = track
                .clips
                .get(clip_index + 1)
                .ok_or_else(|| "Select a clip with an adjacent video clip after it".to_owned())?;
            if let Some(transition) = sequence.transitions.iter().find(|transition| {
                transition.outgoing_clip == outgoing.id && transition.incoming_clip == incoming.id
            }) {
                Command::DeleteTransition {
                    sequence_id: sequence.id,
                    transition_id: transition.id,
                }
            } else {
                let duration = self
                    .frame_duration()
                    .checked_mul_i64(15)
                    .map_err(|error| error.to_string())?
                    .min(outgoing.source_duration)
                    .min(incoming.source_duration);
                Command::AddTransition {
                    sequence_id: sequence.id,
                    id: TransitionId::new(),
                    outgoing_clip: outgoing.id,
                    incoming_clip: incoming.id,
                    duration,
                    alignment: prism_project::TransitionAlignment::Centered,
                    kind: TransitionKind::Dissolve,
                }
            }
        };
        let label = if ui.get_selected_has_transition() {
            "Remove dissolve"
        } else {
            "Add dissolve"
        };
        self.commit(ui, operation, label)
    }

    pub(super) fn duplicate_clip(&self, ui: &PrismStudio) -> Result<(), String> {
        let operations = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            duplicate_selected_commands(project, ui.get_selected_clip_index())?
        };
        self.commit_commands(ui, operations, "Duplicate clip to track end")
    }

    pub(super) fn change_track(
        &self,
        ui: &PrismStudio,
        index: i32,
        action: &str,
    ) -> Result<(), String> {
        let (operation, selected_clip_id) = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|p| p.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let track = sequence
                .tracks
                .get(usize::try_from(index).map_err(|e| e.to_string())?)
                .ok_or_else(|| "Track unavailable".to_owned())?;
            let selected_clip_id = sequence
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .nth(usize::try_from(ui.get_selected_clip_index()).unwrap_or(usize::MAX))
                .map(|clip| clip.id);
            let operation = match action {
                "lock" => Command::SetTrackLocked {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    locked: !track.locked,
                },
                "mute" => Command::SetTrackMuted {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    muted: !track.muted,
                },
                "visible" => Command::SetTrackVisible {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    visible: !track.visible,
                },
                "up" | "down" => {
                    let current = usize::try_from(index).map_err(|e| e.to_string())?;
                    let destination_index = if action == "up" {
                        current
                            .checked_sub(1)
                            .ok_or_else(|| "Track is already first".to_owned())?
                    } else {
                        let next = current + 1;
                        if next >= sequence.tracks.len() {
                            return Err("Track is already last".to_owned());
                        }
                        next
                    };
                    Command::ReorderTrack {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        destination_index,
                    }
                }
                _ => return Err("Unknown track action".to_owned()),
            };
            (operation, selected_clip_id)
        };
        self.commit(ui, operation, "Track updated")?;
        if let Some(selected_clip_id) = selected_clip_id {
            let new_index = self
                .project
                .borrow()
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .position(|clip| clip.id == selected_clip_id)
                })
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1);
            ui.set_selected_clip_index(new_index);
            self.update_inspector(ui);
        }
        Ok(())
    }

    pub(super) fn reset_thumbnails(&self, ui: &PrismStudio) {
        self.thumbnail_preview.invalidate();
        self.thumbnail_current.set(None);
        let project = self.project.borrow();
        let media = project
            .as_ref()
            .map(|p| p.media.values().enumerate().collect::<Vec<_>>())
            .unwrap_or_default();
        let video_media = project
            .as_ref()
            .map(|project| {
                project
                    .sequences
                    .values()
                    .flat_map(|sequence| &sequence.tracks)
                    .filter(|track| track.kind == TrackKind::Video)
                    .flat_map(|track| &track.clips)
                    .map(|clip| clip.media_id)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        *self.cards.borrow_mut() = media
            .iter()
            .map(|(index, asset)| {
                let metadata = std::fs::metadata(&asset.source_path)
                    .ok()
                    .filter(std::fs::Metadata::is_file);
                MediaCard {
                    uses: 0,
                    offline: metadata.is_none(),
                    file_size: metadata
                        .as_ref()
                        .map_or_else(
                            || "Unavailable".to_owned(),
                            |metadata| format_bytes(metadata.len()),
                        )
                        .into(),
                    index: i32::try_from(*index).unwrap_or(i32::MAX),
                    name: Path::new(&asset.source_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&asset.source_path)
                        .into(),
                    frame: slint::Image::default(),
                    has_frame: false,
                    status: if metadata.is_none() {
                        "Source offline"
                    } else if video_media.contains(&asset.id) {
                        "Preparing preview…"
                    } else {
                        "Audio source · waveform in timeline"
                    }
                    .into(),
                }
            })
            .collect();
        *self.thumbnail_queue.borrow_mut() = media
            .iter()
            .filter(|(_, asset)| video_media.contains(&asset.id))
            .map(|(i, a)| (*i, PathBuf::from(&a.source_path)))
            .collect();
        self.thumbnail_total
            .set(self.thumbnail_queue.borrow().len());
        self.thumbnail_completed.set(0);
        self.thumbnail_failed.set(0);
        self.filter_cards(ui);
        self.next_thumbnail(ui);
    }

    pub(super) fn filter_cards(&self, ui: &PrismStudio) {
        let query = ui.get_search().to_lowercase();
        let usage = self
            .project
            .borrow()
            .as_ref()
            .map(|project| {
                let counts = media_usage(project);
                project
                    .media
                    .keys()
                    .enumerate()
                    .map(|(index, id)| {
                        (
                            i32::try_from(index).unwrap_or(i32::MAX),
                            i32::try_from(*counts.get(id).unwrap_or(&0)).unwrap_or(i32::MAX),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let paths = self
            .project
            .borrow()
            .as_ref()
            .map(|project| {
                project
                    .media
                    .values()
                    .enumerate()
                    .map(|(index, asset)| {
                        (
                            i32::try_from(index).unwrap_or(i32::MAX),
                            asset.source_path.clone(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        ui.set_selected_media_uses(*usage.get(&ui.get_selected_media_index()).unwrap_or(&0));
        let mut cards = self
            .cards
            .borrow()
            .iter()
            .filter(|item| item.name.to_lowercase().contains(&query))
            .cloned()
            .map(|mut item| {
                item.uses = *usage.get(&item.index).unwrap_or(&0);
                if let Some(path) = paths.get(&item.index) {
                    item.offline = !Path::new(path).is_file();
                }
                item
            })
            .filter(|item| match ui.get_media_filter() {
                1 => item.uses > 0,
                2 => item.uses == 0,
                3 => item.offline,
                _ => true,
            })
            .collect::<Vec<_>>();
        if ui.get_media_sort() != 0 {
            cards.sort_by_cached_key(|item| (item.name.to_lowercase(), item.index));
            if ui.get_media_sort() == 2 {
                cards.reverse();
            }
        }
        ui.set_media_cards(slint::ModelRc::new(slint::VecModel::from(cards)));
    }

    pub(super) fn update_media_task_state(&self, ui: &PrismStudio) {
        let total = self.thumbnail_total.get();
        let completed = self.thumbnail_completed.get();
        let failed = self.thumbnail_failed.get();
        let pending_decode = total.saturating_sub(completed);
        let pending_writes = self
            .media_artifacts
            .borrow()
            .as_ref()
            .map_or(0, MediaArtifactWorker::pending_writes);
        let pending_waveforms = self.waveform_worker.borrow().pending();
        let pending = pending_decode
            .saturating_add(pending_writes)
            .saturating_add(pending_waveforms);
        ui.set_media_task_pending(i32::try_from(pending).unwrap_or(i32::MAX));
        ui.set_media_task_status(
            if total == 0 {
                "IDLE"
            } else if pending_decode > 0 {
                "DECODING"
            } else if pending_writes > 0 {
                "CACHING"
            } else if pending_waveforms > 0 {
                "WAVEFORMS"
            } else if failed > 0 || self.waveform_failed.get() > 0 {
                "COMPLETE WITH ERRORS"
            } else {
                "COMPLETE"
            }
            .into(),
        );
        ui.set_media_task_detail(
            format!(
                "{completed}/{total} previews · {}/{} waveforms{}{}{}",
                self.waveform_completed.get(),
                self.waveform_total.get(),
                if failed > 0 {
                    format!(" · {failed} unavailable")
                } else {
                    String::new()
                },
                if pending_writes > 0 {
                    format!(" · {pending_writes} cache writes")
                } else {
                    String::new()
                },
                if self.waveform_failed.get() > 0 {
                    format!(" · {} waveform errors", self.waveform_failed.get())
                } else {
                    String::new()
                }
            )
            .into(),
        );
    }

    pub(super) fn next_thumbnail(&self, ui: &PrismStudio) {
        if let Some((index, path)) = self.thumbnail_queue.borrow_mut().pop_front() {
            self.thumbnail_current.set(Some(index));
            self.thumbnail_preview
                .request_thumbnail(path, self.cache_root());
        } else {
            self.thumbnail_current.set(None);
        }
        self.update_media_task_state(ui);
    }

    pub(super) fn reset_waveforms(&self, ui: &PrismStudio) {
        self.waveform_worker
            .replace(WaveformWorker::spawn(media_tool("ffmpeg"), 64));
        self.waveform_total.set(0);
        self.waveform_completed.set(0);
        self.waveform_failed.set(0);
        self.waveforms.borrow_mut().clear();
        let Some(cache_root) = self.cache_root() else {
            self.update_media_task_state(ui);
            return;
        };
        let requests = self
            .project
            .borrow()
            .as_ref()
            .map(|project| {
                let mut audio_media = BTreeMap::<MediaAssetId, RationalTime>::new();
                for clip in project
                    .sequences
                    .values()
                    .flat_map(|sequence| &sequence.tracks)
                    .filter(|track| track.kind == TrackKind::Audio)
                    .flat_map(|track| &track.clips)
                {
                    let Ok(source_end) = clip.source_start.checked_add(clip.source_duration) else {
                        continue;
                    };
                    audio_media
                        .entry(clip.media_id)
                        .and_modify(|current| *current = (*current).max(source_end))
                        .or_insert(source_end);
                }
                audio_media
                    .into_iter()
                    .filter_map(|(id, source_end)| {
                        let asset = project.media.get(&id)?;
                        let samples = source_end
                            .to_ticks(48_000, prism_timeline::RoundingMode::Ceil)
                            .ok()
                            .and_then(|samples| usize::try_from(samples).ok())?
                            .max(1);
                        Some(WaveformRequest {
                            path: PathBuf::from(&asset.source_path),
                            cache_root: cache_root.clone(),
                            sample_rate: 48_000,
                            max_samples: samples,
                            samples_per_point: 1_024,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.waveform_total.set(requests.len());
        for request in requests {
            if let Err(error) = self.waveform_worker.borrow().try_submit(request) {
                self.waveform_failed
                    .set(self.waveform_failed.get().saturating_add(1));
                ui.set_feedback(format!("Could not queue audio waveform: {error}").into());
            }
        }
        self.update_media_task_state(ui);
    }

    pub(super) fn refresh_timeline_waveforms(&self, ui: &PrismStudio) {
        let clips = ui.get_timeline_clips();
        let waveforms = self.waveforms.borrow();
        for index in 0..clips.row_count() {
            let Some(mut clip) = clips.row_data(index) else {
                continue;
            };
            if !clip.audio {
                continue;
            }
            let peaks = waveforms
                .get(clip.source_path.as_str())
                .map(|bars| {
                    waveform_range(bars, clip.waveform_start_ratio, clip.waveform_width_ratio)
                })
                .unwrap_or_default();
            clip.waveform = slint::ModelRc::new(slint::VecModel::from(peaks));
            clips.set_row_data(index, clip);
        }
    }

    pub(super) fn update_history(&self, ui: &PrismStudio) {
        let undo = self.undo.borrow();
        let redo = self.redo.borrow();
        ui.set_can_undo(!undo.is_empty());
        ui.set_can_redo(!redo.is_empty());
        let items = undo
            .iter()
            .map(|record| HistoryItem {
                label: record.label().into(),
                applied: true,
            })
            .chain(redo.iter().rev().map(|record| HistoryItem {
                label: record.label().into(),
                applied: false,
            }))
            .collect::<Vec<_>>();
        ui.set_history_items(slint::ModelRc::new(slint::VecModel::from(items)));
    }

    pub(super) fn apply_history(&self, ui: &PrismStudio, redo_action: bool) -> Result<(), String> {
        let mut updated = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let path = self
            .project_path
            .borrow()
            .clone()
            .ok_or_else(|| "Project destination unavailable".to_owned())?;
        let (from, to) = if redo_action {
            (&self.redo, &self.undo)
        } else {
            (&self.undo, &self.redo)
        };
        let record = from
            .borrow_mut()
            .pop()
            .ok_or_else(|| "No edit available".to_owned())?;
        let label = record.label().to_owned();
        let inverse = record.apply(&mut updated).map_err(|e| e.to_string())?;
        if let Err(error) = prism_persistence::save_atomic(&updated, &path) {
            // Restore the history entry if disk persistence failed.
            let restored = inverse.apply(&mut updated).map_err(|e| e.to_string())?;
            from.borrow_mut().push(restored);
            return Err(error.to_string());
        }
        to.borrow_mut().push(inverse);
        let audio_changed = self.project.borrow().as_ref().and_then(|project| {
            crate::audio_preview::plan(project, self.playhead.get())
                .ok()
                .flatten()
        }) != crate::audio_preview::plan(&updated, self.playhead.get())
            .ok()
            .flatten();
        if self.playing.get() && audio_changed {
            self.stop_playback(ui);
        }
        apply_project_summary(ui, &updated, &path);
        self.refresh_timeline_waveforms(ui);
        *self.project.borrow_mut() = Some(updated);
        self.filter_cards(ui);
        if let Some(project) = self.project.borrow().as_ref() {
            self.queue_autosave(ui, project);
        }
        ui.set_selected_clip_index(-1);
        self.update_history(ui);
        self.update_inspector(ui);
        self.update_ruler(ui);
        self.seek_time(ui, self.playhead.get());
        if label == "Relink source media" {
            let media_index = ui.get_selected_media_index();
            self.reset_thumbnails(ui);
            self.select_media(ui, media_index);
            self.update_inspector(ui);
        }
        ui.set_feedback(
            format!(
                "{} {label} · saved",
                if redo_action { "Redid" } else { "Undid" }
            )
            .into(),
        );
        Ok(())
    }
}
