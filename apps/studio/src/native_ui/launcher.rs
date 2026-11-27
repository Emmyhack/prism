use super::*;

#[allow(clippy::too_many_lines)]
pub(super) fn launcher_actions(ui: &PrismStudio) -> Vec<CommandItem> {
    let project = ui.get_has_project();
    let sequence = ui.get_clip_count() > 0;
    let clip = ui.get_selected_clip_index() >= 0 && !ui.get_inspector_locked();
    let entries = [
        (
            "project.new",
            "New project…",
            "Start a local Prism project",
            "",
            true,
        ),
        (
            "project.open",
            "Open project…",
            "Open an existing .prism file",
            "Ctrl/⌘ O",
            true,
        ),
        (
            "project.import_media",
            "Import footage…",
            "Browse files and add media",
            "Ctrl/⌘ I",
            true,
        ),
        (
            "project.save_as",
            "Save project as…",
            "Create a new project file and continue editing the copy",
            "Ctrl/⌘ Shift S",
            project,
        ),
        (
            "project.save",
            "Save project",
            "Save the current project",
            "Ctrl/⌘ S",
            project,
        ),
        (
            "edit.undo",
            "Undo last edit",
            "Restore the preceding saved state",
            "Ctrl/⌘ Z",
            ui.get_can_undo(),
        ),
        (
            "edit.redo",
            "Redo edit",
            "Reapply the last undone transaction",
            "Ctrl/⌘ Shift Z",
            ui.get_can_redo(),
        ),
        (
            "playback.play_pause",
            "Play / pause sequence",
            "Control sequence playback",
            "Space",
            sequence,
        ),
        (
            "timeline.split",
            "Split selected clip",
            "Cut the selected clip at the playhead",
            "Ctrl/⌘ K",
            clip,
        ),
        (
            "timeline.split_all",
            "Add edit to all unlocked tracks",
            "Cut crossing clips together, including muted and hidden tracks",
            "Ctrl/⌘ Shift K",
            sequence,
        ),
        (
            "source.match_program_frame",
            "Match Program frame in Source",
            "Locate the original frame beneath the playhead",
            if ui.get_keyboard_preset() == "Premiere" {
                "F"
            } else {
                ""
            },
            sequence,
        ),
        (
            "timeline.trim_av",
            "Change aligned A/V source range",
            "Apply Source In and Duration to both matching clips",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.move_av",
            "Move aligned audio and video",
            "Move both matching clips to the time entered in aligned A/V controls",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.split_av",
            "Split aligned audio and video",
            "Cut both matching clips at the playhead in one undo step",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.delete_av",
            "Delete aligned audio and video",
            "Remove both matching clips and leave their gaps",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.duplicate",
            "Duplicate clip to track end",
            "Append a copy of the selected source range",
            "",
            clip,
        ),
        (
            "timeline.delete",
            "Delete selected clip",
            "Remove the clip and leave its gap",
            "Delete",
            clip,
        ),
        (
            "timeline.ripple_delete",
            "Ripple delete selected clip",
            "Remove the clip and close time on its track",
            if ui.get_keyboard_preset() == "Premiere" {
                "Shift Delete"
            } else {
                "Backspace"
            },
            clip,
        ),
        (
            "timeline.add_marker",
            "Add timeline marker",
            "Annotate the current playhead position",
            "M",
            sequence,
        ),
        (
            "view.markers",
            "Show markers",
            "Browse, rename, and navigate timeline markers",
            "",
            true,
        ),
        (
            "playback.previous_edit",
            "Go to previous edit",
            "Jump to the preceding enabled track boundary",
            "↑",
            sequence,
        ),
        (
            "playback.next_edit",
            "Go to next edit",
            "Jump to the following enabled track boundary",
            "↓",
            sequence,
        ),
        (
            "playback.start",
            "Go to sequence start",
            "Move the playhead to the beginning",
            "Home",
            sequence,
        ),
        (
            "playback.end",
            "Go to sequence end",
            "Move the playhead to the end",
            "End",
            sequence,
        ),
        (
            "timeline.fit",
            "Fit sequence in timeline",
            "Show the entire sequence",
            if ui.get_keyboard_preset() == "Premiere" {
                "Backslash"
            } else {
                "F"
            },
            project,
        ),
        (
            "timeline.toggle_snap",
            "Toggle timeline snapping",
            "Switch frame-grid snapping on or off",
            "S",
            true,
        ),
        (
            "source.match_frame",
            "Match selected clip in Source",
            "Show its frame at the playhead, or its source In when outside the clip",
            if ui.get_keyboard_preset() == "Premiere" {
                ""
            } else {
                "Shift F"
            },
            ui.get_selected_clip_index() >= 0,
        ),
        (
            "source.clear_marks",
            "Use full source clip",
            "Clear source In/Out marks without changing the timeline",
            "",
            ui.get_source_ready(),
        ),
        (
            "source.insert",
            "Insert marked source at playhead",
            "Use first video/audio tracks and shift unlocked tracks",
            if ui.get_keyboard_preset() == "Premiere" {
                ","
            } else {
                ""
            },
            ui.get_source_ready(),
        ),
        (
            "source.overwrite",
            "Overwrite with marked source at playhead",
            "Replace first video/audio tracks while preserving later timing",
            if ui.get_keyboard_preset() == "Premiere" {
                "."
            } else {
                ""
            },
            ui.get_source_ready(),
        ),
        (
            "source.append",
            "Append marked source range",
            "Add the source In/Out range to the sequence",
            if ui.get_keyboard_preset() == "Premiere" {
                ""
            } else {
                ","
            },
            ui.get_source_ready(),
        ),
        (
            "media.find_timeline",
            "Find source in timeline",
            "Cycle through clips that use the selected source",
            "",
            ui.get_selected_media_uses() > 0,
        ),
        (
            "media.relink_source",
            "Relink selected source…",
            "Repair the location of source media",
            "",
            ui.get_selected_media_index() >= 0,
        ),
        (
            "view.source",
            "Show source controls",
            "Inspect source range, relinking, and proxies",
            "",
            true,
        ),
        (
            "view.properties",
            "Show clip properties",
            "Edit selected clip timing",
            "",
            true,
        ),
        (
            "view.shortcuts",
            "Show keyboard shortcuts",
            "Choose a preset and inspect available or reserved mappings",
            "",
            true,
        ),
        (
            "view.history",
            "Show edit history",
            "Inspect undoable session actions",
            "",
            true,
        ),
        (
            "view.scopes",
            "Show color scopes",
            "Inspect the decoded Program frame histogram, waveform and chroma",
            "",
            true,
        ),
        (
            "view.jobs",
            "Show background jobs",
            "Autosave, thumbnails, proxies, cache, and export",
            "",
            true,
        ),
        (
            "view.export",
            "Open export settings",
            "Choose 1080p or 4K output and a destination",
            "",
            true,
        ),
        (
            "view.save_layout",
            "Save workspace layout",
            "Remember this project's panels, sizes and media sorting",
            "",
            project,
        ),
        (
            "view.reset",
            "Reset workspace layout",
            "Restore Editing, the inspector, and timeline fit",
            "",
            true,
        ),
        (
            "tool.V",
            "Selection / move tool",
            "Select clips, drag to move, or scrub the ruler",
            "V",
            true,
        ),
        (
            "tool.C",
            "Razor tool",
            "Split a clip at the clicked frame",
            "C",
            true,
        ),
        (
            "tool.T",
            "Trim tool",
            "Drag the nearest clip edge",
            if ui.get_keyboard_preset() == "Premiere" {
                ""
            } else {
                "T"
            },
            true,
        ),
        (
            "tool.Y",
            "Slip tool",
            "Change source frames without moving the clip",
            "Y",
            true,
        ),
        (
            "tool.U",
            "Slide tool",
            "Reposition a clip and adjust both neighbors",
            "U",
            true,
        ),
        (
            "tool.R",
            "Rolling edit tool",
            "Move a boundary between adjacent clips",
            if ui.get_keyboard_preset() == "Premiere" {
                "N"
            } else {
                "R"
            },
            true,
        ),
    ];
    entries
        .into_iter()
        .map(|(id, label, detail, shortcut, enabled)| CommandItem {
            id: id.into(),
            label: label.into(),
            detail: if enabled {
                detail.into()
            } else {
                match id {
                    "edit.undo" => "Nothing to undo in this session".into(),
                    "edit.redo" => "Nothing to redo in this session".into(),
                    "project.save" => "Open or create a project first".into(),
                    "source.append" => "Select an available source and mark its range".into(),
                    "playback.play_pause" | "timeline.add_marker" => {
                        "Add clips to a sequence first".into()
                    }
                    _ => "Select an unlocked timeline clip first".into(),
                }
            },
            shortcut: shortcut.into(),
            enabled,
        })
        .collect()
}

pub(super) fn filter_launcher(ui: &PrismStudio) {
    let terms = ui
        .get_palette_query()
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let items = launcher_actions(ui)
        .into_iter()
        .filter(|item| {
            let searchable = format!("{} {}", item.label, item.detail).to_lowercase();
            terms.iter().all(|term| searchable.contains(term))
        })
        .collect::<Vec<_>>();
    ui.set_command_items(slint::ModelRc::new(slint::VecModel::from(items)));
}

pub(super) fn launch_default_export(
    controller: &mut ExportController,
    worker: &mut Option<ExportWorker>,
    project: &Project,
    output: PathBuf,
    preset: ExportPreset,
) -> Result<(), String> {
    controller.start().map_err(|error| error.to_string())?;
    let target = project
        .sequences
        .iter()
        .find_map(|(sequence_id, sequence)| {
            sequence
                .tracks
                .iter()
                .find(|track| {
                    track.kind == TrackKind::Video && track.visible && !track.clips.is_empty()
                })
                .map(|_| *sequence_id)
        });
    let Some(sequence_id) = target else {
        controller
            .fail("project has no visible non-empty video track")
            .map_err(|error| error.to_string())?;
        return Ok(());
    };
    let (width, height, frame_rate, video_bitrate_kbps) = preset.settings();
    *worker = Some(ExportWorker::spawn_sequence(
        media_tool("ffmpeg"),
        media_tool("ffprobe"),
        project.clone(),
        sequence_id,
        SequenceMp4ExportRequest {
            width,
            height,
            frame_rate,
            video_bitrate_kbps,
            audio_bitrate_kbps: preset.audio_bitrate_kbps(),
            output,
        },
    ));
    Ok(())
}
