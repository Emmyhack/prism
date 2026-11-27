use super::{
    BROWSER_THUMBNAIL_VARIANT, Command, MediaArtifactKind, MediaArtifactWorker, MediaCatalogEntry,
    MediaItemKind, PROXY_AVAILABLE, PreviewWorker, ProxyGeneration, ProxySourceFingerprint,
    TrackId, TrackKind, fingerprint, insert_video_into_project, next_proxy_version,
};
use prism_cache::CacheLayout;
use prism_project::Project;
use prism_timeline::RationalTime;

#[test]
#[allow(clippy::float_cmp, clippy::too_many_lines)]
fn native_workspace_renders_and_edits_are_persisted() -> Result<(), Box<dyn std::error::Error>> {
    use super::{
        NativeExportRuntime, PrismStudio, apply_project_summary, bind_editor_callbacks,
        display_timecode,
    };
    use slint::platform::{
        Platform, WindowAdapter,
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    };
    use slint::{ComponentHandle, Model};
    use std::rc::Rc;

    struct TestPlatform(Rc<MinimalSoftwareWindow>);
    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }
    let window = MinimalSoftwareWindow::new(RepaintBufferType::default());
    slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
    let ui = PrismStudio::new()?;
    ui.on_media_matches(|name, query| name.to_lowercase().contains(&query.to_lowercase()));
    window.set_size(slint::PhysicalSize::new(1440, 900));
    ui.show()?;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/media/av-ntsc-48k.mp4");
    let mut project = Project::new("A different perspective");
    insert_video_into_project(&mut project, &fixture, RationalTime::new(2, 1)?)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "prism-ui-test-{}-{nonce}.prism",
        std::process::id()
    ));
    prism_persistence::save_atomic(&project, &path)?;
    let autosave_path = prism_persistence::save_autosave(&project, &path)?;
    let autosave_revision = project.revision();
    apply_project_summary(&ui, &project, &path);
    let runtime = Rc::new(NativeExportRuntime::new(Some(project), Some(path.clone())));
    bind_editor_callbacks(&ui, &runtime);
    assert!(ui.invoke_edit_shortcut("n".into(), false, false, false));
    assert_eq!(ui.get_active_tool(), "R");
    ui.set_active_tool("V".into());
    assert!(ui.invoke_edit_shortcut("r".into(), false, false, false));
    assert_eq!(ui.get_active_tool(), "V");
    assert!(ui.get_feedback().contains("not built"));
    ui.set_keyboard_preset("Prism".into());
    assert!(ui.invoke_edit_shortcut("r".into(), false, false, false));
    assert_eq!(ui.get_active_tool(), "R");
    ui.set_keyboard_preset("Premiere".into());
    ui.set_active_tool("V".into());
    assert!(ui.invoke_edit_shortcut("=".into(), false, false, false));
    assert_eq!(ui.get_timeline_zoom(), 2.0);
    assert!(ui.invoke_edit_shortcut("\\".into(), false, false, false));
    assert_eq!(ui.get_timeline_zoom(), 1.0);

    runtime.reset_view(&ui);
    assert_eq!(ui.get_media_task_status(), "DECODING");
    assert_eq!(ui.get_media_task_pending(), 1);
    assert_eq!(ui.get_recovery_count(), 1);
    assert_eq!(ui.get_recovery_path(), autosave_path.display().to_string());
    assert_eq!(ui.get_recovery_revision(), autosave_revision.to_string());
    // Exercise actual keyboard dispatch: open, type, navigate, run, restore focus.
    {
        use slint::platform::{Key, WindowEvent};
        let press = |text: slint::SharedString| {
            window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            window.dispatch_event(WindowEvent::KeyReleased { text });
        };
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
        press("p".into());
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert!(ui.get_palette_open());
        for character in ["m", "a", "r", "k", "e", "r"] {
            press(character.into());
        }
        assert_eq!(ui.get_palette_query(), "marker");
        press(Key::DownArrow.into());
        assert_eq!(ui.get_palette_index(), 1);
        press(Key::Tab.into());
        press(Key::Return.into());
        assert!(!ui.get_palette_open());
        assert_eq!(ui.get_browser_tab(), "Markers");
        press("s".into());
        assert!(!ui.get_snap_enabled());
        press("s".into());
    }
    assert!(ui.get_snap_enabled());
    runtime.handle("timeline.toggle_snap", &ui)?;
    assert!(!ui.get_snap_enabled());
    runtime.handle("timeline.toggle_snap", &ui)?;
    runtime.add_marker(&ui)?;
    let marker_revision = runtime
        .project
        .borrow()
        .as_ref()
        .map(Project::revision)
        .ok_or("missing project")?;
    let autosave_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while ui.get_autosave_status() != format!("SAVED R{marker_revision}")
        && std::time::Instant::now() < autosave_deadline
    {
        runtime.poll(&ui);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        ui.get_autosave_status(),
        format!("SAVED R{marker_revision}")
    );
    assert_eq!(
        runtime
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .map(|sequence| sequence.markers.len()),
        Some(1)
    );
    assert_eq!(ui.get_timeline_markers().row_count(), 1);
    ui.set_selected_marker_index(0);
    runtime.delete_marker(&ui)?;
    assert_eq!(ui.get_timeline_markers().row_count(), 0);
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_markers().row_count(), 1);
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_markers().row_count(), 0);
    assert_eq!(ui.get_tracks().row_count(), 1);
    assert_eq!(ui.get_timeline_clips().row_count(), 1);
    assert_eq!(ui.get_duration_label(), "00:00:02:00");
    assert!(ui.invoke_media_matches("Interview.MP4".into(), "interview".into()));
    assert!(!ui.invoke_media_matches("Interview.MP4".into(), "music".into()));

    ui.set_selected_clip_index(0);
    runtime.seek_time(&ui, RationalTime::new(1, 1)?);
    ui.invoke_track_action(0, "lock".into());
    assert!(
        ui.get_tracks()
            .row_data(0)
            .is_some_and(|track| track.locked)
    );
    assert!(runtime.edit_clip("timeline.split", &ui).is_err());
    assert_eq!(
        prism_persistence::load(&path)?
            .sequences
            .values()
            .next()
            .map(|s| s.tracks[0].clips.len()),
        Some(1)
    );
    ui.invoke_track_action(0, "lock".into());
    ui.invoke_command_invoked("timeline.split".into());
    assert_eq!(ui.get_timeline_clips().row_count(), 2);
    let saved = prism_persistence::load(&path)?;
    let sequence = saved.sequences.values().next().ok_or("missing sequence")?;
    assert_eq!(
        sequence.tracks[0].clips[1].source_start,
        RationalTime::new(1, 1)?
    );
    assert_eq!(
        sequence.tracks[0].clips[1].timeline_start,
        RationalTime::new(1, 1)?
    );
    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    assert!(ui.get_selected_has_next());
    runtime.toggle_dissolve(&ui)?;
    assert_eq!(ui.get_timeline_transitions().row_count(), 1);
    assert!(ui.get_selected_has_transition());
    assert!(ui.get_can_export());
    assert!(ui.get_export_preflight().contains("Ready"));
    runtime.toggle_dissolve(&ui)?;
    assert_eq!(ui.get_timeline_transitions().row_count(), 0);
    assert!(ui.get_can_export());
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_transitions().row_count(), 1);
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_transitions().row_count(), 0);

    if std::env::var_os("PRISM_TEST_FFMPEG").is_some() {
        runtime.seek_time(&ui, RationalTime::new(1, 2)?);
        ui.set_selected_media("av-ntsc-48k.mp4".into());
        runtime.select_media(&ui, 0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while (!ui.get_has_program_frame()
            || !ui.get_has_source_frame()
            || !runtime
                .cards
                .borrow()
                .first()
                .is_some_and(|card| card.has_frame))
            && std::time::Instant::now() < deadline
        {
            runtime.poll(&ui);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            ui.get_has_program_frame(),
            "Program preview did not decode: {}",
            ui.get_feedback()
        );
        assert_eq!(ui.get_program_luma_scope().row_count(), 32);
        assert_eq!(ui.get_program_luma_waveform().row_count(), 64);
        assert_eq!(ui.get_program_vectorscope().row_count(), 256);
        assert!(
            ui.get_has_source_frame(),
            "Source preview did not decode: {}",
            ui.get_feedback()
        );
    }
    let media_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while ui.get_media_task_pending() > 0 && std::time::Instant::now() < media_deadline {
        runtime.poll(&ui);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(ui.get_media_task_pending(), 0);
    assert!(matches!(
        ui.get_media_task_status().as_str(),
        "COMPLETE" | "COMPLETE WITH ERRORS"
    ));

    assert_eq!(ui.get_media_cards().row_count(), 1);
    ui.set_search("no match".into());
    ui.invoke_filter_changed();
    assert_eq!(ui.get_media_cards().row_count(), 0);
    ui.set_search("AV-NTSC".into());
    ui.invoke_filter_changed();
    assert_eq!(
        ui.get_media_cards().row_data(0).map(|card| card.index),
        Some(0)
    );
    ui.set_search("".into());
    ui.invoke_filter_changed();
    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    assert!(!ui.get_inspector_name().is_empty());
    assert!(ui.get_can_undo());
    assert_eq!(ui.get_history_items().row_count(), 5);

    // Render all four workspaces and the minimum supported desktop size.
    // Optional PPM artifacts support visual review without a live desktop session.
    let snapshot_directory = std::env::var_os("PRISM_UI_SNAPSHOTS");
    for (width, height) in [(1440, 900), (1100, 720)] {
        window.set_size(slint::PhysicalSize::new(width, height));
        for workspace in [
            "HOME",
            "IMPORT",
            "EDIT",
            "EXPORT",
            "HISTORY",
            "JOBS",
            "ASSEMBLY",
            "COLLAPSED",
            "ZOOM",
            "SOURCE",
            "PRECISION",
            "PAIRED",
            "SCOPES",
            "WAVEFORM",
            "VECTOR",
            "MARKERS",
            "COMMANDS",
            "COMMANDS_EMPTY",
            "SHORTCUTS",
            "EXPORT4K",
        ] {
            ui.set_palette_open(matches!(workspace, "COMMANDS" | "COMMANDS_EMPTY"));
            ui.set_palette_query(
                if workspace == "COMMANDS_EMPTY" {
                    "no such action 123"
                } else {
                    "marker"
                }
                .into(),
            );
            ui.invoke_palette_filter();
            ui.set_palette_index(0);
            ui.set_inspector_section(
                if workspace == "SOURCE" {
                    "Source"
                } else {
                    "Clip"
                }
                .into(),
            );
            ui.set_precision_open(workspace == "PRECISION");
            ui.set_paired_tools_open(workspace == "PAIRED");
            ui.set_program_scope_mode(match workspace {
                "WAVEFORM" => 1,
                "VECTOR" => 2,
                _ => 0,
            });
            ui.set_browser_tab(
                if workspace == "MARKERS" {
                    "Markers"
                } else {
                    "Media"
                }
                .into(),
            );
            ui.set_export_preset_index(i32::from(workspace == "EXPORT4K"));
            ui.set_layout_preset(
                if workspace == "ASSEMBLY" {
                    "Assembly"
                } else {
                    "Editing"
                }
                .into(),
            );
            ui.set_inspector_open(workspace != "COLLAPSED");
            ui.set_inspector_tab(
                match workspace {
                    "SHORTCUTS" => "Shortcuts",
                    "HISTORY" => "History",
                    "JOBS" => "Jobs",
                    "SCOPES" | "WAVEFORM" | "VECTOR" => "Scopes",
                    _ => "Properties",
                }
                .into(),
            );
            ui.set_timeline_zoom(if workspace == "ZOOM" { 2.0 } else { 1.0 });
            ui.set_timeline_offset(if workspace == "ZOOM" { 0.25 } else { 0.0 });
            runtime.update_ruler(&ui);
            ui.set_active_workspace(
                match workspace {
                    "HOME" | "IMPORT" | "EXPORT" => workspace,
                    "EXPORT4K" => "EXPORT",
                    _ => "EDIT",
                }
                .into(),
            );
            let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
            window.request_redraw();
            assert!(window.draw_if_needed(|renderer| {
                renderer.render(buffer.make_mut_slice(), width as usize);
            }));
            assert!(buffer.as_bytes().iter().any(|byte| *byte > 80));
            if let Some(directory) = &snapshot_directory {
                use std::io::Write;
                std::fs::create_dir_all(directory)?;
                let mut file = std::fs::File::create(
                    std::path::Path::new(directory).join(format!("{workspace}-{width}.ppm")),
                )?;
                write!(file, "P6\n{width} {height}\n255\n")?;
                file.write_all(buffer.as_bytes())?;
            }
        }
    }
    // Launcher filtering, context eligibility, and execution use the real callbacks.
    ui.set_palette_query("split selected".into());
    ui.invoke_palette_filter();
    assert_eq!(ui.get_command_items().row_count(), 1);
    ui.set_inspector_locked(true);
    ui.invoke_palette_filter();
    assert!(
        !ui.get_command_items()
            .row_data(0)
            .is_some_and(|item| item.enabled)
    );
    let revision = runtime.project.borrow().as_ref().map(Project::revision);
    ui.invoke_palette_run("timeline.split".into());
    assert_eq!(
        runtime.project.borrow().as_ref().map(Project::revision),
        revision
    );
    ui.set_inspector_locked(false);
    ui.set_palette_query("trim tool".into());
    ui.invoke_palette_filter();
    assert_eq!(ui.get_command_items().row_count(), 1);
    ui.set_palette_open(true);
    ui.invoke_palette_run("tool.T".into());
    assert_eq!(ui.get_active_tool(), "T");
    assert!(!ui.get_palette_open());
    ui.invoke_palette_run("tool.V".into());
    ui.invoke_palette_run("view.source".into());
    assert_eq!(ui.get_inspector_section(), "Source");
    assert_eq!(ui.get_inspector_tab(), "Properties");
    ui.invoke_palette_run("view.markers".into());
    assert_eq!(ui.get_browser_tab(), "Markers");
    ui.set_palette_query("no matching action 987".into());
    ui.invoke_palette_filter();
    assert_eq!(ui.get_command_items().row_count(), 0);

    // Rename is one undoable transaction that preserves marker identity/time.
    let initial_markers = ui.get_timeline_markers().row_count();
    runtime.add_marker(&ui)?;
    let marker_index = ui.get_timeline_markers().row_count() - 1;
    let marker_before = runtime
        .project
        .borrow()
        .as_ref()
        .and_then(|p| p.sequences.values().next())
        .and_then(|s| s.markers.get(marker_index))
        .cloned()
        .ok_or("marker missing")?;
    ui.set_selected_marker_index(i32::try_from(marker_index)?);
    ui.set_marker_label("Opening beat".into());
    ui.invoke_command_invoked("timeline.rename_marker".into());
    let saved_markers = prism_persistence::load(&path)?;
    let renamed = saved_markers
        .sequences
        .values()
        .next()
        .and_then(|s| s.markers.iter().find(|m| m.id == marker_before.id))
        .ok_or("renamed marker missing")?;
    assert_eq!(renamed.label, "Opening beat");
    assert_eq!(renamed.time, marker_before.time);
    ui.set_marker_label("   ".into());
    assert!(runtime.rename_marker(&ui).is_err());
    runtime.apply_history(&ui, false)?;
    assert_eq!(
        ui.get_timeline_markers()
            .row_data(marker_index)
            .map(|m| m.label.to_string()),
        Some(marker_before.label)
    );
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_markers().row_count(), initial_markers);

    ui.set_selected_clip_index(1);
    ui.invoke_command_invoked("timeline.delete".into());
    assert_eq!(ui.get_timeline_clips().row_count(), 1);
    runtime.seek_ratio(&ui, 1.0);
    assert!(!ui.get_has_program_frame()); // Half-open clip interval at sequence end.
    assert_eq!(ui.get_timecode(), "00:00:01:00");
    assert_eq!(display_timecode(RationalTime::new(3661, 1)?), "01:01:01:00");
    ui.invoke_command_invoked("edit.undo".into());
    assert_eq!(ui.get_timeline_clips().row_count(), 2);
    ui.invoke_command_invoked("edit.redo".into());
    assert_eq!(ui.get_timeline_clips().row_count(), 1);
    assert!(ui.get_can_undo());
    let undo_count = runtime.undo.borrow().len();
    *runtime.project_path.borrow_mut() = Some(path.join("cannot-save-here.prism"));
    assert!(runtime.apply_history(&ui, false).is_err());
    assert_eq!(runtime.undo.borrow().len(), undo_count);
    assert_eq!(ui.get_timeline_clips().row_count(), 1);
    *runtime.project_path.borrow_mut() = Some(path.clone());

    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    ui.set_clip_start("0".into());
    ui.set_clip_source_in("0".into());
    ui.set_clip_duration("0.5".into());
    runtime.apply_timing(&ui)?;
    assert_eq!(ui.get_duration_label(), "00:00:00:15");
    assert!(!ui.get_can_redo());
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_duration_label(), "00:00:01:00");

    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    runtime.adjust_clip_frame(&ui, "timeline.move_right")?;
    assert_eq!(ui.get_clip_start(), "1/30");
    runtime.apply_history(&ui, false)?;
    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    runtime.adjust_clip_frame(&ui, "timeline.trim_out_left")?;
    assert_eq!(ui.get_clip_duration(), "29/30");
    runtime.apply_history(&ui, false)?;
    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    runtime.drag_clip(&ui, 0, 1.0 / 30.0, "slip")?;
    assert_eq!(ui.get_clip_source_in(), "1/30");
    runtime.apply_history(&ui, false)?;
    assert!(
        runtime
            .precision_edit_frame(&ui, "timeline.roll_out_right")
            .is_err()
    );

    runtime.drag_clip(&ui, 0, 1.0 / 30.0, "move")?;
    assert_eq!(ui.get_clip_start(), "1/30");
    runtime.apply_history(&ui, false)?;
    runtime.drag_clip(&ui, 0, -1.0 / 30.0, "out")?;
    assert_eq!(ui.get_clip_duration(), "29/30");
    runtime.apply_history(&ui, false)?;

    runtime.seek_time(&ui, RationalTime::ZERO);
    runtime.handle("playback.play_pause", &ui)?;
    assert!(ui.get_playing());
    let decoding_generation = runtime.program_preview.generation.get();
    ui.set_has_program_frame(true);
    runtime.seek_time(&ui, RationalTime::new(1, 20)?);
    assert!(ui.get_has_program_frame());
    assert_eq!(
        runtime.program_preview.generation.get(),
        decoding_generation
    );
    let playback_started = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(100))
        .ok_or("playback start instant underflow")?;
    let master = crate::audio_clock::AudioClock::default();
    *runtime.audio_clock.borrow_mut() = Some(master.clone());
    runtime.playback_started.set(Some(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(2))
            .ok_or("playback start instant underflow")?,
    ));
    let held = runtime.playhead.get();
    runtime.poll(&ui);
    assert_eq!(runtime.playhead.get(), held);
    assert!(ui.get_playback_buffering());
    master.observe(b"0.12 M-A: 0 aq=4KB");
    runtime.poll(&ui);
    assert_eq!(
        runtime.playhead.get(),
        runtime
            .playback_origin
            .get()
            .checked_add(RationalTime::new(120, 1000)?)?
    );
    assert!(!ui.get_playback_buffering());
    runtime.audio_clock.borrow_mut().take();
    runtime.playback_started.set(Some(playback_started));
    runtime.poll(&ui);
    assert!(runtime.playhead.get() > RationalTime::ZERO);
    // A real child stands in for ffplay, so Pause must terminate and reap it.
    #[cfg(unix)]
    let audio_pid = {
        let child = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
        let pid = child.id();
        *runtime.audio_process.borrow_mut() = Some(child);
        pid
    };
    #[cfg(unix)]
    let decoder_pid = {
        let child = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
        let pid = child.id();
        *runtime.audio_decoder.borrow_mut() = Some(child);
        pid
    };
    let paused_position = runtime.playhead.get();
    runtime.handle("playback.play_pause", &ui)?;
    assert!(!ui.get_playing());
    assert!(!runtime.playing.get());
    assert!(runtime.playback_started.get().is_none());
    assert!(runtime.audio_process.borrow().is_none());
    assert!(runtime.audio_decoder.borrow().is_none());
    #[cfg(unix)]
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &decoder_pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()?
            .success()
    );
    assert_eq!(runtime.playhead.get(), paused_position);
    #[cfg(unix)]
    assert!(
        !std::process::Command::new("/bin/kill")
            .args(["-0", &audio_pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()?
            .success(),
        "Pause left the audio process running"
    );

    *runtime.audio_clock.borrow_mut() = Some(crate::audio_clock::AudioClock::default());
    runtime.playing.set(true);
    ui.set_playing(true);
    runtime.playback_started.set(Some(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(6))
            .ok_or("playback start instant underflow")?,
    ));
    runtime.poll(&ui);
    assert!(!runtime.playing.get());
    assert!(!ui.get_playback_buffering());
    assert!(runtime.audio_clock.borrow().is_none());
    runtime.playing.set(true);
    ui.set_playing(true);
    runtime
        .playback_started
        .set(Some(std::time::Instant::now()));
    runtime
        .program_preview
        .generation
        .set(runtime.program_preview.generation.get().wrapping_add(1));
    runtime.program_preview.pending.borrow_mut().take();
    runtime.program_preview.busy.set(true);
    runtime.program_preview.requested_at.set(Some(
        std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(6))
            .ok_or("preview request instant underflow")?,
    ));
    #[cfg(unix)]
    let watchdog_pids = {
        let player = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
        let decoder = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
        let pids = [player.id(), decoder.id()];
        *runtime.audio_process.borrow_mut() = Some(player);
        *runtime.audio_decoder.borrow_mut() = Some(decoder);
        pids
    };
    runtime.poll(&ui);
    #[cfg(unix)]
    for pid in watchdog_pids {
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()?
                .success()
        );
    }
    assert!(ui.get_preview_status().contains("timed out"));
    assert!(!runtime.playing.get());
    assert!(!runtime.program_preview.busy.get());
    assert!(runtime.program_preview.requested_at.get().is_none());
    assert!(runtime.audio_process.borrow().is_none());
    assert!(runtime.audio_decoder.borrow().is_none());
    let sequence_position = runtime.playhead.get();
    runtime.source_duration.set(RationalTime::new(1, 1)?);
    runtime.source_in.set(RationalTime::new(1, 4)?);
    runtime.source_out.set(RationalTime::new(1, 2)?);
    ui.set_source_ready(true);
    ui.set_selected_media_index(0);
    runtime.seek_source(&ui, 0.4);
    assert_eq!(runtime.playhead.get(), sequence_position);
    runtime.append_source(&ui)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 2);
    ui.set_selected_clip_index(0);
    runtime.edit_clip("timeline.ripple_delete", &ui)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 1);
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 2);
    ui.set_selected_clip_index(0);
    runtime.drag_clip(&ui, 0, 1.0 / 30.0, "roll_out")?;
    runtime.apply_history(&ui, false)?;
    runtime.append_source(&ui)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 3);
    ui.set_selected_clip_index(1);
    runtime.drag_clip(&ui, 1, 1.0 / 30.0, "slide")?;
    runtime.apply_history(&ui, false)?;
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 2);
    let saved = prism_persistence::load(&path)?;
    let sequence = saved.sequences.values().next().ok_or("missing sequence")?;
    assert_eq!(
        sequence.tracks[0].clips[1].source_start,
        RationalTime::new(1, 4)?
    );
    assert_eq!(
        sequence.tracks[0].clips[1].source_duration,
        RationalTime::new(1, 4)?
    );
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 1);

    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    runtime.duplicate_clip(&ui)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 2);
    let duplicated = prism_persistence::load(&path)?;
    let duplicated_track = &duplicated
        .sequences
        .values()
        .next()
        .ok_or("missing sequence")?
        .tracks[0];
    assert_eq!(
        duplicated_track.clips[1].source_start,
        duplicated_track.clips[0].source_start
    );
    assert_eq!(
        duplicated_track.clips[1].source_duration,
        duplicated_track.clips[0].source_duration
    );
    assert_eq!(
        duplicated_track.clips[1].timeline_start,
        duplicated_track.clips[0]
            .timeline_start
            .checked_add(duplicated_track.clips[0].source_duration)?
    );
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_timeline_clips().row_count(), 1);

    let replacement = std::env::temp_dir().join(format!(
        "prism-relink-test-{}-{nonce}.mp4",
        std::process::id()
    ));
    std::fs::copy(&fixture, &replacement)?;
    ui.set_selected_media_index(0);
    runtime.relink_source_to(&ui, &replacement)?;
    assert_eq!(ui.get_source_detail(), replacement.display().to_string());
    assert_eq!(
        prism_persistence::load(&path)?
            .media
            .values()
            .next()
            .map(|asset| asset.source_path.as_str()),
        replacement.to_str()
    );
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_source_detail(), fixture.display().to_string());

    let media_id = runtime
        .project
        .borrow()
        .as_ref()
        .and_then(|project| project.media.keys().next().copied())
        .ok_or("missing media")?;
    let proxy_path =
        std::env::temp_dir().join(format!("prism-ui-proxy-test-{}.mp4", std::process::id()));
    std::fs::write(&proxy_path, b"proxy")?;
    let (proxy_sender, proxy_receiver) = std::sync::mpsc::sync_channel(1);
    proxy_sender.send(Ok(proxy_path.clone()))?;
    let recorded_source = fingerprint(&fixture)?;
    *runtime.proxy_generation.borrow_mut() = Some(ProxyGeneration {
        receiver: proxy_receiver,
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        media_id,
        source_fingerprint: ProxySourceFingerprint {
            sha256: recorded_source.sha256,
            byte_length: recorded_source.byte_length,
        },
    });
    ui.set_proxy_generating(true);
    runtime.poll(&ui);
    assert_eq!(ui.get_selected_proxy(), proxy_path.display().to_string());
    assert_eq!(ui.get_proxy_status(), PROXY_AVAILABLE);
    assert!(ui.get_proxy_active());
    assert!(!ui.get_proxy_generating());
    runtime.clear_proxy(&ui)?;
    assert_eq!(ui.get_selected_proxy(), "");
    assert!(!ui.get_proxy_active());
    let sequence_id = runtime
        .project
        .borrow()
        .as_ref()
        .and_then(|project| project.sequences.values().next())
        .map(|sequence| sequence.id)
        .ok_or("missing sequence")?;
    let audio_track_id = TrackId::new();
    runtime.commit(
        &ui,
        Command::CreateTrack {
            sequence_id,
            id: audio_track_id,
            kind: TrackKind::Audio,
        },
        "Create test audio track",
    )?;
    ui.set_selected_clip_index(0);
    runtime.update_inspector(&ui);
    runtime.change_track(&ui, 0, "down")?;
    assert_eq!(ui.get_selected_track_index(), 1);
    assert_eq!(
        prism_persistence::load(&path)?
            .sequences
            .values()
            .next()
            .and_then(|sequence| sequence.tracks.first())
            .map(|track| track.id),
        Some(audio_track_id)
    );
    runtime.apply_history(&ui, false)?;
    runtime.apply_history(&ui, false)?;
    assert_eq!(ui.get_tracks().row_count(), 1);
    let cache_root = path.with_extension("prism-cache");
    let cache_layout = CacheLayout::new(&cache_root);
    let cache_started = std::time::Instant::now();
    while runtime
        .media_artifacts
        .borrow()
        .as_ref()
        .is_some_and(|worker| worker.pending_writes() > 0)
        && cache_started.elapsed() < std::time::Duration::from_secs(2)
    {
        runtime.poll(&ui);
        std::thread::yield_now();
    }
    runtime.poll(&ui);
    assert!(cache_layout.inventory()?.artifact_count >= 1);
    runtime.update_cache_state(&ui);
    assert_eq!(ui.get_cache_status(), "READY");
    assert!(!ui.get_cache_detail().starts_with("0 artifacts"));
    runtime.clean_cache(&ui)?;
    assert!(ui.get_cache_cleaning());
    let cleanup_started = std::time::Instant::now();
    while ui.get_cache_cleaning() && cleanup_started.elapsed() < std::time::Duration::from_secs(2) {
        runtime.poll(&ui);
        std::thread::yield_now();
    }
    assert!(!ui.get_cache_cleaning());
    assert_eq!(ui.get_cache_detail(), "0 artifacts · 0 B");
    runtime.update_autosave_preferences(&ui, true)?;
    assert!(!ui.get_autosave_enabled());
    assert_eq!(ui.get_autosave_status(), "DISABLED");
    runtime.update_autosave_preferences(&ui, false)?;
    assert_eq!(ui.get_autosave_retention(), 10);
    runtime.update_autosave_preferences(&ui, true)?;
    assert!(ui.get_autosave_enabled());
    let mut av = Project::new("Export checks");
    super::insert_media_into_project(&mut av, &fixture, RationalTime::new(1, 1)?, true)?;
    super::update_export_summary(&ui, &av);
    assert!(ui.get_can_export());
    assert!(ui.get_export_audio_summary().contains("AAC"));
    let mut layered = av.clone();
    let sequence = layered.sequences.values_mut().next().unwrap();
    let mut extra = sequence
        .tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .unwrap()
        .clone();
    extra.id = TrackId::new();
    for clip in &mut extra.clips {
        clip.id = prism_core::ClipId::new();
    }
    sequence.tracks.push(extra);
    super::update_export_summary(&ui, &layered);
    assert!(ui.get_can_export());
    assert!(ui.get_export_video_summary().contains("2 active tracks"));
    {
        let sequence = av
            .sequences
            .values_mut()
            .next()
            .ok_or("sequence unavailable")?;
        let audio = sequence
            .tracks
            .iter_mut()
            .find(|t| t.kind == TrackKind::Audio)
            .ok_or("audio unavailable")?;
        audio.clips[0].source_duration = RationalTime::new(1, 2)?;
    }
    super::update_export_summary(&ui, &av);
    assert!(ui.get_can_export());
    assert!(ui.get_export_audio_summary().contains("1 active tracks"));
    {
        let sequence = av
            .sequences
            .values_mut()
            .next()
            .ok_or("sequence unavailable")?;
        for track in &mut sequence.tracks {
            if track.kind == TrackKind::Audio {
                track.muted = true;
            }
        }
    }
    super::update_export_summary(&ui, &av);
    assert!(ui.get_can_export());
    assert!(ui.get_export_audio_summary().contains("video-only"));
    {
        let sequence = av
            .sequences
            .values_mut()
            .next()
            .ok_or("sequence unavailable")?;
        for track in &mut sequence.tracks {
            if track.kind == TrackKind::Video {
                track.visible = false;
            }
        }
    }
    super::update_export_summary(&ui, &av);
    assert!(!ui.get_can_export());
    let match_revision = runtime.project.borrow().as_ref().map(Project::revision);
    ui.set_selected_clip_index(0);
    let expected_match = runtime
        .project
        .borrow()
        .as_ref()
        .and_then(|project| super::matched_source(project, 0, runtime.playhead.get()))
        .ok_or("Missing match")?;
    runtime.handle("source.match_frame", &ui)?;
    assert_eq!(runtime.source_position.get(), expected_match.1);
    assert_eq!(
        ui.get_selected_media_index(),
        i32::try_from(expected_match.0)?
    );
    assert_eq!(ui.get_inspector_section(), "Source");
    assert_eq!(
        runtime.project.borrow().as_ref().map(Project::revision),
        match_revision
    );
    ui.set_selected_clip_index(-1);
    assert!(runtime.handle("source.match_frame", &ui).is_err());
    // Navigation reveals off-screen playheads, and Fit restores the complete view.
    ui.set_timeline_zoom(8.0);
    ui.set_timeline_offset(0.0);
    runtime.handle("playback.end", &ui)?;
    assert_eq!(ui.get_playhead_ratio(), 1.0);
    assert_eq!(ui.get_timeline_offset(), 0.875);
    runtime.handle("playback.start", &ui)?;
    assert_eq!(ui.get_playhead_ratio(), 0.0);
    assert_eq!(ui.get_timeline_offset(), 0.0);
    runtime.handle("timeline.fit", &ui)?;
    assert_eq!(ui.get_timeline_zoom(), 1.0);
    assert_eq!(ui.get_timeline_offset(), 0.0);
    // Source range controls describe exact duration and never edit the sequence.
    let before_range = runtime.project.borrow().as_ref().map(Project::revision);
    runtime.source_duration.set(RationalTime::new(4, 1)?);
    runtime.source_in.set(RationalTime::new(1, 1)?);
    runtime.source_out.set(RationalTime::new(3, 1)?);
    runtime.update_source_labels(&ui);
    assert_eq!(ui.get_source_in_ratio(), 0.25);
    assert_eq!(ui.get_source_out_ratio(), 0.75);
    assert_eq!(ui.get_source_range_label(), "00:00:02:00");
    ui.set_source_ready(true);
    runtime.handle("source.clear_marks", &ui)?;
    assert_eq!(runtime.source_in.get(), RationalTime::ZERO);
    assert_eq!(runtime.source_out.get(), RationalTime::new(4, 1)?);
    assert_eq!(ui.get_source_range_label(), "00:00:04:00");
    assert_eq!(ui.get_source_in_ratio(), 0.0);
    assert_eq!(ui.get_source_out_ratio(), 1.0);
    assert_eq!(
        runtime.project.borrow().as_ref().map(Project::revision),
        before_range
    );
    ui.set_source_ready(false);
    assert!(runtime.handle("source.clear_marks", &ui).is_err());
    runtime.source_duration.set(RationalTime::ZERO);
    runtime.source_in.set(RationalTime::ZERO);
    runtime.source_out.set(RationalTime::ZERO);
    runtime.update_source_labels(&ui);
    assert_eq!(ui.get_source_out_ratio(), 0.0);
    // Media lookup follows source IDs and does not create an editing transaction.
    let lookup_revision = runtime.project.borrow().as_ref().map(Project::revision);
    ui.set_selected_media_index(0);
    ui.set_selected_clip_index(-1);
    runtime.find_media_in_timeline(&ui)?;
    let first_found = ui.get_selected_clip_index();
    assert!(first_found >= 0);
    assert_eq!(ui.get_inspector_section(), "Clip");
    runtime.find_media_in_timeline(&ui)?;
    assert!(ui.get_selected_clip_index() >= 0);
    assert_eq!(
        runtime.project.borrow().as_ref().map(Project::revision),
        lookup_revision
    );
    ui.set_search("".into());
    ui.set_media_filter(1);
    runtime.filter_cards(&ui);
    assert!(ui.get_media_cards().row_count() > 0);
    assert!(ui.get_media_cards().iter().all(|card| card.uses > 0));
    ui.set_media_filter(2);
    runtime.filter_cards(&ui);
    assert!(ui.get_media_cards().iter().all(|card| card.uses == 0));
    ui.set_media_filter(3);
    runtime.filter_cards(&ui);
    assert!(ui.get_media_cards().iter().all(|card| card.offline));
    ui.set_media_filter(0);
    runtime.filter_cards(&ui);
    // View persistence must not mutate editing history or source identities.
    let edit_revision = runtime.project.borrow().as_ref().map(Project::revision);
    ui.set_layout_preset("Assembly".into());
    ui.set_inspector_section("Source".into());
    ui.set_media_sort(2);
    runtime.handle("view.scopes", &ui)?;
    ui.set_media_filter(2);
    ui.set_program_scope_mode(2);
    ui.set_show_monitor_scope(true);
    runtime.save_view(&ui)?;
    runtime.handle("view.reset", &ui)?;
    assert_eq!(ui.get_layout_preset(), "Editing");
    runtime.restore_view(&ui);
    assert_eq!(ui.get_layout_preset(), "Assembly");
    assert_eq!(ui.get_inspector_section(), "Source");
    assert_eq!(ui.get_media_sort(), 2);
    assert_eq!(ui.get_inspector_tab(), "Scopes");
    assert_eq!(ui.get_program_scope_mode(), 2);
    assert!(ui.get_show_monitor_scope());
    assert_eq!(ui.get_media_filter(), 2);
    ui.set_media_filter(0);
    assert_eq!(
        runtime.project.borrow().as_ref().map(Project::revision),
        edit_revision
    );
    let saved_cards = runtime.cards.borrow().clone();
    let mut sorted_cards = Vec::new();
    for (name, index) in [("Beta", 0), ("Alpha", 8), ("Gamma", 3)] {
        let mut card = saved_cards[0].clone();
        card.name = name.into();
        card.index = index;
        sorted_cards.push(card);
    }
    *runtime.cards.borrow_mut() = sorted_cards;
    ui.set_search("".into());
    ui.set_media_sort(1);
    runtime.filter_cards(&ui);
    assert_eq!(
        ui.get_media_cards().row_data(0).map(|card| card.index),
        Some(8)
    );
    ui.set_media_sort(2);
    runtime.filter_cards(&ui);
    assert_eq!(
        ui.get_media_cards().row_data(0).map(|card| card.index),
        Some(3)
    );
    ui.set_search("bEtA".into());
    runtime.filter_cards(&ui);
    assert_eq!(ui.get_media_cards().row_count(), 1);
    assert_eq!(
        ui.get_media_cards().row_data(0).map(|card| card.index),
        Some(0)
    );
    *runtime.cards.borrow_mut() = saved_cards;
    std::fs::write(path.with_extension("workspace.json"), br#"{"version":999}"#)?;
    runtime.restore_view(&ui);
    assert_eq!(ui.get_layout_preset(), "Editing");
    assert!(ui.get_feedback().contains("Could not restore"));
    std::fs::remove_file(path.with_extension("workspace.json"))?;
    // Save As protects the original and directs subsequent edits to the copy.
    let original_revision = prism_persistence::load(&path)?.revision();
    let copy_path = path.with_extension("copy.prism");
    let undo_count = runtime.undo.borrow().len();
    runtime.save_as_to(&ui, &copy_path)?;
    assert_eq!(runtime.project_path.borrow().as_ref(), Some(&copy_path));
    assert_eq!(runtime.undo.borrow().len(), undo_count);
    assert!(runtime.save_as_to(&ui, &path).is_err());
    assert_eq!(runtime.project_path.borrow().as_ref(), Some(&copy_path));
    runtime.handle("timeline.add_marker", &ui)?;
    assert!(prism_persistence::load(&copy_path)?.revision() > original_revision);
    assert_eq!(
        prism_persistence::load(&path)?.revision(),
        original_revision
    );
    runtime.autosave_scheduler.borrow_mut().take();
    for candidate in prism_persistence::list_autosaves(&copy_path)? {
        std::fs::remove_file(candidate.path)?;
    }
    std::fs::remove_file(&copy_path)?;
    let copy_preferences = copy_path.with_extension("autosave.json");
    if copy_preferences.exists() {
        std::fs::remove_file(copy_preferences)?;
    }
    let copy_cache = copy_path.with_extension("prism-cache");
    if copy_cache.exists() {
        std::fs::remove_dir_all(copy_cache)?;
    }
    runtime.autosave_scheduler.borrow_mut().take();
    for candidate in prism_persistence::list_autosaves(&path)? {
        std::fs::remove_file(candidate.path)?;
    }
    std::fs::remove_file(proxy_path)?;
    std::fs::remove_file(replacement)?;
    std::fs::remove_file(&path)?;
    std::fs::remove_file(path.with_extension("autosave.json"))?;
    std::fs::remove_dir_all(cache_root)?;
    ui.hide()?;
    Ok(())
}

#[test]
fn proxy_versions_never_overwrite_existing_media() -> Result<(), Box<dyn std::error::Error>> {
    let directory =
        std::env::temp_dir().join(format!("prism-proxy-version-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let base = directory.join("interview.proxy.mp4");
    let version_two = directory.join("interview.proxy.v2.mp4");
    std::fs::write(&base, b"original proxy")?;
    std::fs::write(&version_two, b"second proxy")?;
    assert_eq!(
        next_proxy_version(&base)?,
        directory.join("interview.proxy.v3.mp4")
    );
    assert_eq!(
        next_proxy_version(&version_two)?,
        directory.join("interview.proxy.v3.mp4")
    );
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn inspector_timing_parser_is_exact_and_rejects_invalid_values()
-> Result<(), Box<dyn std::error::Error>> {
    use super::parse_timing;
    assert_eq!(parse_timing("1.25")?, RationalTime::new(5, 4)?);
    assert_eq!(
        parse_timing(" 1001/30000 ")?,
        RationalTime::new(1001, 30000)?
    );
    assert_eq!(parse_timing(".5")?, RationalTime::new(1, 2)?);
    for input in [
        "-1",
        "-0.5",
        "NaN",
        "inf",
        "1/0",
        "0.1234567",
        "1..2",
        "9223372036854775807.9",
        "",
    ] {
        assert!(parse_timing(input).is_err(), "accepted {input}");
    }
    Ok(())
}
#[test]
fn export_preset_selection_uses_the_typed_encoder_settings()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(super::export_preset(0)?.settings().0, 1920);
    assert_eq!(super::export_preset(1)?.settings().0, 3840);
    assert_eq!(super::export_preset(1)?.settings().3, 35_000);
    assert_eq!(super::export_preset(2)?.settings().1, 1920);
    assert_eq!(super::export_preset(3)?.settings().0, 1080);
    assert!(super::export_preset(-1).is_err());
    assert!(super::export_preset(4).is_err());
    Ok(())
}

#[test]
fn waveform_bars_preserve_bucket_peaks_and_bound_ui_nodes() {
    let points = (0..250)
        .map(|index| prism_audio::WaveformPoint {
            min: if index == 120 { -1.0 } else { -0.25 },
            max: 0.5,
            rms: 0.2,
        })
        .collect::<Vec<_>>();
    let bars = super::waveform_bars(&points, 96);
    assert!(bars.len() <= 96);
    assert!(bars.iter().any(|peak| (*peak - 1.0).abs() < f32::EPSILON));
    assert!(bars.iter().all(|peak| (0.0..=1.0).contains(peak)));
}

#[test]
fn waveform_range_projects_only_the_trimmed_source_interval() {
    let bars = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
    assert_eq!(super::waveform_range(&bars, 0.25, 0.5), bars[2..6]);
    assert_eq!(super::waveform_range(&bars, 0.875, 0.5), bars[7..8]);
    assert!(super::waveform_range(&bars, 0.0, 0.0).is_empty());
}

#[test]
#[allow(clippy::float_cmp)]
fn luma_scope_is_bounded_normalized_and_tracks_brightness() {
    let mut rgba = Vec::new();
    for value in [0_u8, 0, 128, 255] {
        rgba.extend([value, value, value, 255]);
    }
    let scope = super::luma_scope(&rgba, 4, 4);
    assert_eq!(scope.len(), 4);
    assert_eq!(scope[0], 1.0);
    assert_eq!(scope[2], 0.5);
    assert_eq!(scope[3], 0.5);
    assert!(scope.iter().all(|value| (0.0..=1.0).contains(value)));
    assert!(super::luma_scope(&rgba, 0, 4).is_empty());
}

#[test]
fn luma_waveform_preserves_horizontal_brightness_ranges() {
    let rgba = [
        0, 0, 0, 255, 255, 255, 255, 255, 128, 128, 128, 255, 64, 64, 64, 255,
    ];
    let waveform = super::luma_waveform(&rgba, 2, 2, 2, 4);
    assert_eq!(waveform.len(), 2);
    assert!(waveform[0].low.abs() < f32::EPSILON);
    assert!((waveform[0].high - 128.0 / 255.0).abs() < 0.001);
    assert!((waveform[1].low - 64.0 / 255.0).abs() < 0.001);
    assert!((waveform[1].high - 1.0).abs() < f32::EPSILON);
    assert!(super::luma_waveform(&rgba, 0, 2, 2, 4).is_empty());
}

#[test]
fn vectorscope_places_neutral_and_saturated_samples_in_chroma_space() {
    let rgba = [128, 128, 128, 255, 255, 0, 0, 255];
    let scope = super::vectorscope(&rgba, 4, 8);
    assert_eq!(scope.len(), 16);
    assert!(
        (scope[10] - 1.0).abs() < f32::EPSILON,
        "neutral gray belongs at scope center"
    );
    assert!(
        (scope[1] - 1.0).abs() < f32::EPSILON,
        "red belongs above and left of center"
    );
    assert_eq!(scope.iter().filter(|density| **density > 0.0).count(), 2);
    assert!(super::vectorscope(&rgba, 0, 8).is_empty());
    assert!(super::vectorscope(&rgba, 4, 0).is_empty());
}

#[test]
fn thumbnail_preview_reuses_cache_before_decoding_source() -> Result<(), Box<dyn std::error::Error>>
{
    let root =
        std::env::temp_dir().join(format!("prism-studio-thumbnail-hit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let source = root.join("not-a-video.mov");
    std::fs::write(&source, b"fingerprinted but intentionally not a video")?;
    let cache_root = root.join("cache");
    let fingerprint = fingerprint(&source)?;
    let entry = MediaCatalogEntry {
        path: source.clone(),
        kind: MediaItemKind::Video,
        metadata: None,
        fingerprint: Some(fingerprint),
        offline: false,
        diagnostic: None,
    };
    let artifacts = MediaArtifactWorker::spawn(&cache_root, 1);
    let frame = prism_media::DecodedVideoFrame {
        width: 1,
        height: 1,
        rgba: vec![12, 34, 56, 255],
    };
    artifacts.schedule_thumbnail_variant(&entry, &frame, 1, 1, BROWSER_THUMBNAIL_VARIANT)?;
    artifacts
        .result_timeout(std::time::Duration::from_secs(1))?
        .result?;

    let preview = PreviewWorker::with_thumbnails(true);
    preview.request_thumbnail(source.clone(), Some(cache_root.clone()));
    let started = std::time::Instant::now();
    let result = loop {
        if let Some(result) = preview.poll_decoded() {
            break result;
        }
        if started.elapsed() >= std::time::Duration::from_secs(1) {
            return Err("cached thumbnail timed out".into());
        }
        std::thread::yield_now();
    }?;
    assert_eq!(result.0, frame);
    assert!(result.3);

    let key = MediaArtifactWorker::thumbnail_variant_key(&entry, BROWSER_THUMBNAIL_VARIANT);
    let layout = CacheLayout::new(&cache_root);
    let artifact_path = layout
        .lookup(key, MediaArtifactKind::Thumbnail.extension())?
        .ok_or("cached thumbnail missing")?;
    std::fs::write(artifact_path, b"corrupt thumbnail")?;
    preview.request_thumbnail(source, Some(cache_root));
    let started = std::time::Instant::now();
    let corrupt_result = loop {
        if let Some(result) = preview.poll_decoded() {
            break result;
        }
        if started.elapsed() >= std::time::Duration::from_secs(1) {
            return Err("corrupt thumbnail repair timed out".into());
        }
        std::thread::yield_now();
    };
    assert!(corrupt_result.is_err());
    assert_eq!(
        layout.lookup(key, MediaArtifactKind::Thumbnail.extension())?,
        None
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}
