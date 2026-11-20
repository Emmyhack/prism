//! Native editing presets. One table drives dispatch and the visible shortcut panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Command(&'static str),
    Tool(&'static str),
    Missing(&'static str),
}
#[derive(Clone, Copy)]
pub(crate) struct Binding {
    pub key: &'static str,
    pub primary: bool,
    pub shift: bool,
    pub name: &'static str,
    pub detail: &'static str,
    pub target: Target,
}
fn binding(
    key: &'static str,
    primary: bool,
    shift: bool,
    name: &'static str,
    target: Target,
    detail: &'static str,
) -> Binding {
    Binding {
        key,
        primary,
        shift,
        name,
        detail,
        target,
    }
}
#[allow(clippy::too_many_lines)]
pub(crate) fn bindings(premiere: bool) -> Vec<Binding> {
    use Target::{Command as C, Missing as M, Tool as T};
    let mut rows = vec![
        binding(
            "v",
            false,
            false,
            "Selection tool",
            T("V"),
            "Select and move timeline clips",
        ),
        binding(
            "c",
            false,
            false,
            "Razor tool",
            T("C"),
            "Split at the clicked frame",
        ),
        binding(
            "y",
            false,
            false,
            "Slip tool",
            T("Y"),
            "Change source timing beneath the edit",
        ),
        binding(
            "u",
            false,
            false,
            "Slide tool",
            T("U"),
            "Move a clip between its neighbors",
        ),
        binding(
            "s",
            false,
            false,
            "Snapping",
            C("timeline.toggle_snap"),
            "Toggle timeline snapping",
        ),
        binding(
            "m",
            false,
            false,
            "Add marker",
            C("timeline.add_marker"),
            "Add a marker at the playhead",
        ),
        binding(
            "Space",
            false,
            false,
            "Play / Stop",
            C("playback.play_pause"),
            "Toggle timeline playback",
        ),
        binding(
            "Left",
            false,
            false,
            "Previous frame",
            C("playback.step_backward"),
            "Step the playhead backward",
        ),
        binding(
            "Right",
            false,
            false,
            "Next frame",
            C("playback.step_forward"),
            "Step the playhead forward",
        ),
        binding(
            "Up",
            false,
            false,
            "Previous edit",
            C("playback.previous_edit"),
            "Jump to the preceding enabled boundary",
        ),
        binding(
            "Down",
            false,
            false,
            "Next edit",
            C("playback.next_edit"),
            "Jump to the following enabled boundary",
        ),
        binding(
            "Home",
            false,
            false,
            "Sequence start",
            C("playback.start"),
            "Go to the beginning",
        ),
        binding(
            "End",
            false,
            false,
            "Sequence end",
            C("playback.end"),
            "Go to the end",
        ),
        binding(
            "k",
            true,
            false,
            "Add edit",
            C("timeline.split"),
            "Current selected clip / uniquely aligned A/V pair",
        ),
        binding(
            "k",
            true,
            true,
            "Add edit to all unlocked tracks",
            C("timeline.split_all"),
            "One atomic cut across crossing clips",
        ),
        binding(
            "z",
            true,
            false,
            "Undo",
            C("edit.undo"),
            "Undo one saved edit",
        ),
        binding(
            "z",
            true,
            true,
            "Redo",
            C("edit.redo"),
            "Redo one saved edit",
        ),
        binding(
            "s",
            true,
            false,
            "Save",
            C("project.save"),
            "Save the active project",
        ),
        binding(
            "s",
            true,
            true,
            "Save As",
            C("project.save_as"),
            "Continue editing a separate copy",
        ),
        binding(
            "o",
            true,
            false,
            "Open project",
            C("project.open"),
            "Open a Prism project",
        ),
        binding(
            "i",
            true,
            false,
            "Import media",
            C("project.import_media"),
            "Choose source files",
        ),
    ];
    if premiere {
        rows.extend([
            binding(
                "n",
                false,
                false,
                "Rolling edit tool",
                T("R"),
                "Adjust the shared edit boundary",
            ),
            binding(
                "f",
                false,
                false,
                "Match frame",
                C("source.match_program_frame"),
                "Open the visible Program source at the playhead",
            ),
            binding(
                "i",
                false,
                false,
                "Mark source In",
                C("source.mark_in"),
                "Source monitor range; sequence marks remain pending",
            ),
            binding(
                "o",
                false,
                false,
                "Mark source Out",
                C("source.mark_out"),
                "Source monitor range; sequence marks remain pending",
            ),
            binding(
                "k",
                false,
                false,
                "Stop",
                C("playback.stop"),
                "Stop transport at the current position",
            ),
            binding(
                "Delete",
                false,
                false,
                "Clear selected clip",
                C("timeline.delete"),
                "Leave the gap",
            ),
            binding(
                "Backspace",
                false,
                false,
                "Clear selected clip",
                C("timeline.delete"),
                "Leave the gap",
            ),
            binding(
                "Delete",
                false,
                true,
                "Ripple delete",
                C("timeline.ripple_delete"),
                "Close time on the edited track or aligned pair",
            ),
            binding(
                "Backspace",
                false,
                true,
                "Ripple delete (alias)",
                C("timeline.ripple_delete"),
                "Laptop keyboard alias",
            ),
            binding(
                "=",
                false,
                false,
                "Zoom in",
                C("view.zoom_in"),
                "Increase timeline magnification",
            ),
            binding(
                "-",
                false,
                false,
                "Zoom out",
                C("view.zoom_out"),
                "Decrease timeline magnification",
            ),
            binding(
                "\\",
                false,
                false,
                "Fit sequence",
                C("timeline.fit"),
                "Show the complete timeline",
            ),
            binding(
                "r",
                false,
                false,
                "Rate Stretch",
                M("Rate Stretch is not implemented yet"),
                "Reserved; never activates Rolling Edit",
            ),
            binding(
                "t",
                false,
                false,
                "Type tool",
                M("Text/title authoring is not implemented yet"),
                "Reserved; never activates Prism Trim",
            ),
            binding(
                "b",
                false,
                false,
                "Ripple Edit tool",
                M("The Ripple Edit pointer tool is not implemented yet"),
                "Ripple delete is available separately",
            ),
            binding(
                "a",
                false,
                false,
                "Track Select",
                M("Track Select is not implemented yet"),
                "Multi-clip targeting remains pending",
            ),
            binding(
                "h",
                false,
                false,
                "Hand tool",
                M("The Hand tool is not implemented yet"),
                "Use the timeline navigation bar",
            ),
            binding(
                "p",
                false,
                false,
                "Pen tool",
                M("The Pen tool and editable keyframes are not implemented yet"),
                "Reserved for the actual tool",
            ),
            binding(
                "j",
                false,
                false,
                "Reverse shuttle",
                M("Reverse/speed shuttle playback is not implemented yet"),
                "Reserved for J/K/L transport",
            ),
            binding(
                "l",
                false,
                false,
                "Forward shuttle",
                M("Variable-speed shuttle playback is not implemented yet; use Space"),
                "Reserved for J/K/L transport",
            ),
            binding(
                ",",
                false,
                false,
                "Insert edit",
                C("source.insert"),
                "Insert marked source on first video/audio tracks; shift unlocked tracks",
            ),
            binding(
                ".",
                false,
                false,
                "Overwrite edit",
                C("source.overwrite"),
                "Replace at playhead on first video/audio tracks; later clips stay put",
            ),
            binding(
                "c",
                true,
                false,
                "Copy clips",
                M("Timeline clipboard copy/paste is not implemented yet"),
                "Text-field clipboard is unaffected",
            ),
            binding(
                "v",
                true,
                false,
                "Paste clips",
                M("Timeline clipboard copy/paste is not implemented yet"),
                "Text-field clipboard is unaffected",
            ),
            binding(
                "x",
                true,
                false,
                "Cut clips",
                M("Timeline clipboard cut/paste is not implemented yet"),
                "Text-field clipboard is unaffected",
            ),
        ]);
    } else {
        rows.extend([
            binding(
                "r",
                false,
                false,
                "Rolling edit tool",
                T("R"),
                "Legacy Prism mapping",
            ),
            binding(
                "t",
                false,
                false,
                "Trim tool",
                T("T"),
                "Legacy Prism mapping",
            ),
            binding(
                "f",
                false,
                false,
                "Fit sequence",
                C("timeline.fit"),
                "Legacy Prism mapping",
            ),
            binding(
                "f",
                false,
                true,
                "Match selected clip",
                C("source.match_frame"),
                "Legacy Prism mapping",
            ),
            binding(
                ",",
                false,
                false,
                "Append source",
                C("source.append"),
                "Append is not an insert edit",
            ),
            binding(
                "Delete",
                false,
                false,
                "Delete clip",
                C("timeline.delete"),
                "Leave the gap",
            ),
            binding(
                "Backspace",
                false,
                false,
                "Ripple delete",
                C("timeline.ripple_delete"),
                "Legacy Prism mapping",
            ),
        ]);
    }
    rows
}
#[allow(clippy::fn_params_excessive_bools)]
pub(crate) fn resolve(
    key: &str,
    primary: bool,
    shift: bool,
    alt: bool,
    premiere: bool,
) -> Option<Target> {
    if alt {
        return None;
    }
    bindings(premiere)
        .into_iter()
        .find(|row| {
            row.key.eq_ignore_ascii_case(key) && row.primary == primary && row.shift == shift
        })
        .map(|row| row.target)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn familiar_keys_never_invoke_a_different_editing_operation() {
        assert_eq!(
            resolve("n", false, false, false, true),
            Some(Target::Tool("R"))
        );
        assert!(matches!(
            resolve("r", false, false, false, true),
            Some(Target::Missing(_))
        ));
        assert!(matches!(
            resolve("t", false, false, false, true),
            Some(Target::Missing(_))
        ));
        assert!(matches!(
            resolve(",", false, false, false, true),
            Some(Target::Command("source.insert"))
        ));
        assert_eq!(
            resolve("F", false, false, false, true),
            Some(Target::Command("source.match_program_frame"))
        );
        assert_eq!(
            resolve("Delete", false, true, false, true),
            Some(Target::Command("timeline.ripple_delete"))
        );
        assert_eq!(
            resolve("Backspace", false, false, false, true),
            Some(Target::Command("timeline.delete"))
        );
        assert_eq!(
            resolve("k", true, true, false, true),
            Some(Target::Command("timeline.split_all"))
        );
        assert_eq!(
            resolve("r", false, false, false, false),
            Some(Target::Tool("R"))
        );
        assert_eq!(
            resolve(",", false, false, false, false),
            Some(Target::Command("source.append"))
        );
        assert!(resolve("n", false, false, true, true).is_none());
    }
}
