//! Typed, validated project mutations shared by every interface.

use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_project::{
    Clip, ClipTransform, Marker, MediaAsset, Project, ProxySourceFingerprint, Sequence, Track,
    TrackKind, Transition, TransitionAlignment, TransitionKind,
};
use prism_timeline::RationalTime;
use thiserror::Error;

/// Stable command categories used by menus, toolbars, shortcuts, and panels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandCategory {
    /// Project and media management.
    Project,
    /// Timeline editing operations.
    Timeline,
    /// Playback and monitor controls.
    Playback,
    /// Application and workspace controls.
    Application,
}

/// Undo behavior declared by a registered command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UndoStrategy {
    /// The command commits one atomic transaction and can be undone.
    Transaction,
    /// The command changes only ephemeral UI state.
    Ephemeral,
}

/// A command descriptor shared by every Prism UI surface.
#[derive(Clone, Copy, Debug)]
pub struct CommandDescriptor {
    /// Stable machine-readable identifier.
    pub id: &'static str,
    /// User-facing label.
    pub label: &'static str,
    /// Menu/toolbar category.
    pub category: CommandCategory,
    /// Default keyboard shortcut label.
    pub shortcut: Option<&'static str>,
    /// Declared undo behavior.
    pub undo: UndoStrategy,
    /// Context predicate evaluated before execution.
    pub enabled: fn(&Project) -> bool,
}

impl CommandDescriptor {
    /// Returns whether this command is currently enabled for the project.
    #[must_use]
    pub fn is_enabled(self, project: &Project) -> bool {
        (self.enabled)(project)
    }

    /// Executes the supplied typed command through the authoritative engine.
    ///
    /// # Errors
    ///
    /// Returns the same validation or invariant error as direct command
    /// execution. The descriptor itself is intentionally metadata-only so
    /// adapters cannot bypass command validation.
    pub fn execute(
        self,
        project: &mut Project,
        command: Command,
    ) -> Result<Vec<Event>, CommandError> {
        if !(self.enabled)(project) {
            return Err(CommandError::Disabled { command: self.id });
        }
        CommandEngine::execute(project, command)
    }
}

/// Stable registry for commands exposed by the current editing shell.
pub struct CommandRegistry;

impl CommandRegistry {
    /// Returns the initial command set shared by menus, shortcuts, and tools.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub const fn descriptors() -> &'static [CommandDescriptor] {
        &[
            CommandDescriptor {
                id: "project.import_media",
                label: "Import Media",
                category: CommandCategory::Project,
                shortcut: Some("Cmd+I"),
                undo: UndoStrategy::Transaction,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "project.relink_media",
                label: "Relink Media",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Transaction,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "project.set_proxy",
                label: "Set Media Proxy",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Transaction,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "timeline.add_marker",
                label: "Add Marker",
                category: CommandCategory::Timeline,
                shortcut: Some("M"),
                undo: UndoStrategy::Transaction,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "timeline.delete_marker",
                label: "Delete Marker",
                category: CommandCategory::Timeline,
                shortcut: None,
                undo: UndoStrategy::Transaction,
                enabled: has_revision,
            },
            CommandDescriptor {
                id: "timeline.toggle_track_lock",
                label: "Toggle Track Lock",
                category: CommandCategory::Timeline,
                shortcut: None,
                undo: UndoStrategy::Transaction,
                enabled: has_revision,
            },
            CommandDescriptor {
                id: "timeline.toggle_track_visibility",
                label: "Toggle Track Visibility",
                category: CommandCategory::Timeline,
                shortcut: None,
                undo: UndoStrategy::Transaction,
                enabled: has_revision,
            },
            CommandDescriptor {
                id: "timeline.toggle_track_mute",
                label: "Toggle Track Mute",
                category: CommandCategory::Timeline,
                shortcut: None,
                undo: UndoStrategy::Transaction,
                enabled: has_revision,
            },
            CommandDescriptor {
                id: "timeline.razor",
                label: "Razor Tool",
                category: CommandCategory::Timeline,
                shortcut: Some("C"),
                undo: UndoStrategy::Transaction,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "timeline.insert_clip",
                label: "Insert Clip",
                category: CommandCategory::Timeline,
                shortcut: Some(","),
                undo: UndoStrategy::Transaction,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "timeline.ripple_delete",
                label: "Ripple Delete",
                category: CommandCategory::Timeline,
                shortcut: Some("Shift+Delete"),
                undo: UndoStrategy::Transaction,
                enabled: has_revision,
            },
            CommandDescriptor {
                id: "timeline.add_transition",
                label: "Add Transition",
                category: CommandCategory::Timeline,
                shortcut: Some("Cmd+D"),
                undo: UndoStrategy::Transaction,
                enabled: has_revision,
            },
            CommandDescriptor {
                id: "timeline.undo",
                label: "Undo",
                category: CommandCategory::Timeline,
                shortcut: Some("Cmd+Z"),
                undo: UndoStrategy::Transaction,
                enabled: has_revision,
            },
            CommandDescriptor {
                id: "playback.play_pause",
                label: "Play/Pause",
                category: CommandCategory::Playback,
                shortcut: Some("Space"),
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "playback.source_monitor",
                label: "Source Monitor",
                category: CommandCategory::Playback,
                shortcut: Some("Shift+2"),
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "playback.program_monitor",
                label: "Program Monitor",
                category: CommandCategory::Playback,
                shortcut: Some("Shift+4"),
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "playback.step_backward",
                label: "Step Back One Frame",
                category: CommandCategory::Playback,
                shortcut: Some("Left"),
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "playback.step_forward",
                label: "Step Forward One Frame",
                category: CommandCategory::Playback,
                shortcut: Some("Right"),
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "export.queue",
                label: "Queue Export",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "export.cancel",
                label: "Cancel Export",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "export.retry",
                label: "Retry Export",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "export.remove",
                label: "Remove Export Job",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "scripting.review_permissions",
                label: "Review Script Permissions",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "scripting.run",
                label: "Run Script",
                category: CommandCategory::Project,
                shortcut: None,
                undo: UndoStrategy::Transaction,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "application.file",
                label: "File Menu",
                category: CommandCategory::Application,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "application.edit",
                label: "Edit Menu",
                category: CommandCategory::Application,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "application.window",
                label: "Window Menu",
                category: CommandCategory::Application,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "application.help",
                label: "Help Menu",
                category: CommandCategory::Application,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "timeline.clip_menu",
                label: "Clip Menu",
                category: CommandCategory::Timeline,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "timeline.sequence_menu",
                label: "Sequence Menu",
                category: CommandCategory::Timeline,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "timeline.markers_menu",
                label: "Markers Menu",
                category: CommandCategory::Timeline,
                shortcut: Some("M"),
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "graphics.menu",
                label: "Graphics Menu",
                category: CommandCategory::Application,
                shortcut: None,
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
            CommandDescriptor {
                id: "application.save",
                label: "Save Project",
                category: CommandCategory::Application,
                shortcut: Some("Cmd+S"),
                undo: UndoStrategy::Ephemeral,
                enabled: always_enabled,
            },
        ]
    }

    /// Finds a descriptor by its stable identifier.
    #[must_use]
    pub fn find(id: &str) -> Option<CommandDescriptor> {
        Self::descriptors()
            .iter()
            .copied()
            .find(|command| command.id == id)
    }
}

fn always_enabled(_: &Project) -> bool {
    true
}
fn has_revision(project: &Project) -> bool {
    project.revision() > 0
}

/// Mutation requests accepted by the authoritative command engine.
#[derive(Debug)]
pub enum Command {
    /// Registers a nondestructive reference to source media.
    ImportMedia {
        /// Stable asset ID.
        id: MediaAssetId,
        /// Source path selected by the user.
        source_path: String,
    },
    /// Updates the authoritative source path for an existing media asset.
    RelinkMedia {
        /// Stable asset ID.
        media_id: MediaAssetId,
        /// New source path selected by the user.
        source_path: String,
    },
    /// Sets or clears a relinked proxy path for an imported media asset.
    SetMediaProxy {
        /// Asset to update.
        media_id: MediaAssetId,
        /// New proxy path, or `None` to clear the relink.
        proxy_path: Option<String>,
        /// Source identity captured when the proxy was assigned.
        source_fingerprint: Option<ProxySourceFingerprint>,
    },
    /// Adds an empty sequence.
    CreateSequence {
        /// Stable ID selected before execution for deterministic replay.
        id: SequenceId,
        /// User-visible sequence name.
        name: String,
    },
    /// Changes a sequence's exact frames-per-second setting.
    SetSequenceFrameRate {
        /// Sequence to update.
        sequence_id: SequenceId,
        /// Exact frames per second.
        frame_rate: RationalTime,
    },
    /// Adds a named exact-time marker to a sequence.
    AddMarker {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Stable marker identifier.
        id: MarkerId,
        /// Exact sequence position.
        time: RationalTime,
        /// User-visible marker label.
        label: String,
    },
    /// Removes a marker from a sequence.
    DeleteMarker {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Marker to remove.
        marker_id: MarkerId,
    },
    /// Adds a transition between adjacent clips.
    AddTransition {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Stable transition identifier.
        id: TransitionId,
        /// Outgoing clip.
        outgoing_clip: ClipId,
        /// Incoming clip.
        incoming_clip: ClipId,
        /// Exact overlap duration.
        duration: RationalTime,
        /// Placement of the overlap relative to the edit boundary.
        alignment: TransitionAlignment,
        /// Transition operation.
        kind: TransitionKind,
    },
    /// Removes a sequence transition.
    DeleteTransition {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Transition to remove.
        transition_id: TransitionId,
    },
    /// Adds a track to an existing sequence.
    CreateTrack {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Stable track ID.
        id: TrackId,
        /// Accepted media kind.
        kind: TrackKind,
    },
    /// Changes whether a track accepts editing mutations.
    SetTrackLocked {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Track to lock or unlock.
        track_id: TrackId,
        /// New lock state.
        locked: bool,
    },
    /// Changes whether a track contributes video layers.
    SetTrackVisible {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Track to update.
        track_id: TrackId,
        /// New visibility state.
        visible: bool,
    },
    /// Changes whether a track contributes audio.
    SetTrackMuted {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Track to update.
        track_id: TrackId,
        /// New mute state.
        muted: bool,
    },
    /// Moves a track to another position in its sequence's ordered track list.
    ReorderTrack {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Track to move.
        track_id: TrackId,
        /// Zero-based destination index.
        destination_index: usize,
    },
    /// Places a source range on a track at normal speed.
    InsertClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Destination track.
        track_id: TrackId,
        /// Stable clip ID.
        id: ClipId,
        /// Referenced media asset.
        media_id: MediaAssetId,
        /// Start in sequence time.
        timeline_start: RationalTime,
        /// Start in source time.
        source_start: RationalTime,
        /// Positive source duration.
        source_duration: RationalTime,
    },
    /// Duplicates a clip's source range at a new timeline position.
    DuplicateClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Existing clip to duplicate.
        clip_id: ClipId,
        /// Stable ID assigned to the duplicate.
        new_clip_id: ClipId,
        /// New sequence position for the duplicate.
        timeline_start: RationalTime,
    },
    /// Moves a clip on its current track.
    MoveClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to move.
        clip_id: ClipId,
        /// New start in sequence time.
        timeline_start: RationalTime,
    },
    /// Changes the source range while preserving a clip's timeline placement.
    SlipClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to slip.
        clip_id: ClipId,
        /// Exact source-time delta; positive advances the source window.
        source_delta: RationalTime,
    },
    /// Moves the boundary between two adjacent clips while preserving their
    /// combined timeline span.
    RollEdit {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip immediately before the edit point.
        left_clip_id: ClipId,
        /// Clip immediately after the edit point.
        right_clip_id: ClipId,
        /// Exact boundary delta; positive extends the left clip.
        delta: RationalTime,
    },
    /// Slides a clip while preserving its duration and adjusting both neighbors.
    SlideEdit {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip immediately before the selected clip.
        previous_clip_id: ClipId,
        /// Clip being moved.
        clip_id: ClipId,
        /// Clip immediately after the selected clip.
        next_clip_id: ClipId,
        /// Exact timeline delta; positive moves the selected clip later.
        delta: RationalTime,
    },
    /// Replaces a clip's sequence placement and source range.
    TrimClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to trim.
        clip_id: ClipId,
        /// New start in sequence time.
        timeline_start: RationalTime,
        /// New start in source time.
        source_start: RationalTime,
        /// New positive source duration.
        source_duration: RationalTime,
    },
    /// Replaces a clip's nondestructive visual transform.
    SetClipTransform {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to update.
        clip_id: ClipId,
        /// Complete validated transform value.
        transform: ClipTransform,
    },
    /// Splits one clip into adjacent left and right clips.
    SplitClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Existing clip retained for the left side.
        clip_id: ClipId,
        /// Stable ID assigned to the new right side.
        right_clip_id: ClipId,
        /// Exact split position in sequence time.
        timeline_position: RationalTime,
    },
    /// Removes a clip from a track.
    DeleteClip {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to remove.
        clip_id: ClipId,
    },
    /// Removes a clip and closes the resulting gap on its track.
    RippleDelete {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Owning track.
        track_id: TrackId,
        /// Clip to remove.
        clip_id: ClipId,
    },
}

/// Event emitted only after a successful state transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    /// Media was registered.
    MediaImported {
        /// Imported asset.
        id: MediaAssetId,
    },
    /// A media asset's source path changed through an explicit relink.
    MediaRelinked {
        /// Updated asset identifier.
        id: MediaAssetId,
    },
    /// A media asset's proxy path changed.
    MediaProxyUpdated {
        /// Updated asset identifier.
        id: MediaAssetId,
    },
    /// A sequence was created.
    SequenceCreated {
        /// Created sequence.
        id: SequenceId,
    },
    /// A sequence frame-rate setting changed.
    SequenceFrameRateChanged {
        /// Updated sequence.
        sequence_id: SequenceId,
        /// New exact frames per second.
        frame_rate: RationalTime,
    },
    /// A marker was added.
    MarkerAdded {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Added marker identifier.
        id: MarkerId,
    },
    /// A marker was removed.
    MarkerDeleted {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Removed marker identifier.
        id: MarkerId,
    },
    /// A transition was added.
    TransitionAdded {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Added transition identifier.
        id: TransitionId,
    },
    /// A transition was removed.
    TransitionDeleted {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Removed transition identifier.
        id: TransitionId,
    },
    /// A track was created.
    TrackCreated {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Created track.
        id: TrackId,
    },
    /// A track lock state changed.
    TrackLockChanged {
        /// Track whose lock state changed.
        track_id: TrackId,
        /// New lock state.
        locked: bool,
    },
    /// A track visibility state changed.
    TrackVisibilityChanged {
        /// Track whose visibility changed.
        track_id: TrackId,
        /// New visibility state.
        visible: bool,
    },
    /// A track mute state changed.
    TrackMuteChanged {
        /// Track whose mute state changed.
        track_id: TrackId,
        /// New mute state.
        muted: bool,
    },
    /// A track changed position within its sequence.
    TrackReordered {
        /// Owning sequence.
        sequence_id: SequenceId,
        /// Track that moved.
        track_id: TrackId,
        /// New zero-based index.
        destination_index: usize,
    },
    /// A clip was inserted.
    ClipInserted {
        /// Owning track.
        track_id: TrackId,
        /// Inserted clip.
        id: ClipId,
    },
    /// A clip was duplicated.
    ClipDuplicated {
        /// Original clip identifier.
        source_id: ClipId,
        /// New duplicate identifier.
        id: ClipId,
    },
    /// A clip was moved.
    ClipMoved {
        /// Moved clip.
        id: ClipId,
        /// New start time.
        timeline_start: RationalTime,
    },
    /// A clip range was trimmed.
    ClipTrimmed {
        /// Trimmed clip.
        id: ClipId,
    },
    /// A clip visual transform changed.
    ClipTransformChanged {
        /// Updated clip.
        id: ClipId,
    },
    /// One clip was divided into two adjacent clips.
    ClipSplit {
        /// Existing left clip.
        left_id: ClipId,
        /// New right clip.
        right_id: ClipId,
    },
    /// A clip was removed.
    ClipDeleted {
        /// Removed clip.
        id: ClipId,
    },
    /// A clip was removed and later clips shifted left by its duration.
    ClipRippleDeleted {
        /// Removed clip.
        id: ClipId,
        /// Number of clips shifted.
        shifted_count: usize,
    },
    /// A clip's source window was slipped without changing its timeline span.
    ClipSlipped {
        /// Slipped clip identifier.
        id: ClipId,
    },
    /// An adjacent clip boundary was rolled.
    ClipsRolled {
        /// Clip before the edit point.
        left_clip_id: ClipId,
        /// Clip after the edit point.
        right_clip_id: ClipId,
    },
    /// A clip and its two adjacent neighbors were slid.
    ClipsSlid {
        /// Selected clip identifier.
        id: ClipId,
    },
}

/// Structured command failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CommandError {
    /// The command is unavailable in the current project context.
    #[error("command is disabled in the current context: {command}")]
    Disabled {
        /// Stable command identifier.
        command: &'static str,
    },
    /// User-provided text failed validation.
    #[error("{field} must not be empty")]
    EmptyField {
        /// Invalid field name.
        field: &'static str,
    },
    /// A proxy mutation supplied an invalid or inconsistent source fingerprint.
    #[error("proxy source fingerprint must be a non-empty lowercase SHA-256 identity")]
    InvalidProxyFingerprint,
    /// A transaction must contain at least one mutation.
    #[error("transaction must contain at least one command")]
    EmptyTransaction,
    /// Caller based its transaction on stale project state.
    #[error("revision conflict: expected {expected}, actual {actual}")]
    RevisionConflict {
        /// Revision observed by the caller.
        expected: u64,
        /// Current project revision.
        actual: u64,
    },
    /// An ID was not found in current project state.
    #[error("sequence not found")]
    SequenceNotFound,
    /// A track ID was not found.
    #[error("track not found")]
    TrackNotFound,
    /// A marker ID was not found.
    #[error("marker not found")]
    MarkerNotFound,
    /// A transition ID was not found.
    #[error("transition not found")]
    TransitionNotFound,
    /// A track destination index was outside the sequence's current range.
    #[error("track destination index is out of range")]
    InvalidTrackIndex,
    /// A clip ID was not found.
    #[error("clip not found")]
    ClipNotFound,
    /// A media asset ID was not found.
    #[error("media asset not found")]
    MediaNotFound,
    /// A locked track rejected a mutation.
    #[error("track is locked")]
    TrackLocked,
    /// A time or duration was outside its valid range.
    #[error("{field} must be {constraint}")]
    InvalidTime {
        /// Invalid field name.
        field: &'static str,
        /// Required range description.
        constraint: &'static str,
    },
    /// A clip visual transform contained non-finite or out-of-range values.
    #[error("clip transform contains invalid values")]
    InvalidClipTransform,
    /// Two clips would occupy intersecting ranges on one track.
    #[error("clip overlaps another clip on the track")]
    ClipOverlap,
    /// Transition clips are not adjacent or the duration exceeds a clip.
    #[error("transition clips must be adjacent and contain the transition duration")]
    InvalidTransition,
    /// A stable ID already exists.
    #[error("object identifier already exists")]
    DuplicateId,
    /// The same source path is already referenced by the project.
    #[error("media source path is already imported")]
    DuplicateMediaPath,
    /// A project invariant prevented the commit.
    #[error("project could not commit: {0}")]
    Commit(String),
    /// Staged state failed authoritative domain validation.
    #[error("project invariant failed: {0}")]
    Invariant(String),
}

/// Result of an atomic command batch.
#[derive(Debug)]
pub struct TransactionOutcome {
    /// Events published in command order after commit.
    pub events: Vec<Event>,
    /// Snapshot-based undo record for this initial architecture.
    pub undo: UndoRecord,
}

/// Reversible project-state record.
#[derive(Debug)]
pub struct UndoRecord {
    label: String,
    previous: Project,
    expected_revision: u64,
}

impl UndoRecord {
    /// User-visible transaction label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Restores the state before this transaction and returns a redo record.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::RevisionConflict`] if later edits made this
    /// record stale.
    pub fn apply(self, project: &mut Project) -> Result<Self, CommandError> {
        if project.revision() != self.expected_revision {
            return Err(CommandError::RevisionConflict {
                expected: self.expected_revision,
                actual: project.revision(),
            });
        }
        let current = std::mem::replace(project, self.previous);
        Ok(Self {
            label: self.label,
            previous: current,
            expected_revision: project.revision(),
        })
    }
}

/// Executes commands atomically against project state.
#[derive(Debug, Default)]
pub struct CommandEngine;

impl CommandEngine {
    /// Validates and applies one command.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] when validation fails or the project cannot
    /// commit. Rejected validation does not mutate state or emit events.
    pub fn execute(project: &mut Project, command: Command) -> Result<Vec<Event>, CommandError> {
        Ok(Self::execute_transaction(project, "Edit", vec![command])?.events)
    }

    /// Validates and commits a batch as one revision.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if any command fails. The original project is
    /// unchanged and no events are returned when a batch fails.
    pub fn execute_transaction(
        project: &mut Project,
        label: impl Into<String>,
        commands: Vec<Command>,
    ) -> Result<TransactionOutcome, CommandError> {
        let revision = project.revision();
        Self::execute_transaction_at_revision(project, revision, label, commands)
    }

    /// Commits a batch only if the project still has the expected revision.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError::RevisionConflict`] for stale callers, or another
    /// [`CommandError`] when validation fails. No failure mutates the project.
    pub fn execute_transaction_at_revision(
        project: &mut Project,
        expected_revision: u64,
        label: impl Into<String>,
        commands: Vec<Command>,
    ) -> Result<TransactionOutcome, CommandError> {
        if project.revision() != expected_revision {
            return Err(CommandError::RevisionConflict {
                expected: expected_revision,
                actual: project.revision(),
            });
        }
        let label = label.into();
        if label.trim().is_empty() {
            return Err(CommandError::EmptyField { field: "label" });
        }
        if commands.is_empty() {
            return Err(CommandError::EmptyTransaction);
        }
        let mut staged = project.clone();
        staged
            .commit_revision()
            .map_err(|error| CommandError::Commit(error.to_string()))?;
        let mut events = Vec::with_capacity(commands.len());
        for command in commands {
            events.push(Self::apply(&mut staged, command)?);
        }
        staged
            .validate()
            .map_err(|error| CommandError::Invariant(error.to_string()))?;
        let previous = std::mem::replace(project, staged);
        let expected_revision = project.revision();
        Ok(TransactionOutcome {
            events,
            undo: UndoRecord {
                label,
                previous,
                expected_revision,
            },
        })
    }

    /// Validates a batch and returns its planned events without committing.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if any command would fail. The supplied project
    /// is never mutated.
    pub fn validate_transaction(
        project: &Project,
        commands: Vec<Command>,
    ) -> Result<Vec<Event>, CommandError> {
        if commands.is_empty() {
            return Err(CommandError::EmptyTransaction);
        }
        let mut staged = project.clone();
        let events = commands
            .into_iter()
            .map(|command| Self::apply(&mut staged, command))
            .collect::<Result<Vec<_>, _>>()?;
        staged
            .validate()
            .map_err(|error| CommandError::Invariant(error.to_string()))?;
        Ok(events)
    }

    #[allow(clippy::too_many_lines)]
    fn apply(project: &mut Project, command: Command) -> Result<Event, CommandError> {
        match command {
            Command::ImportMedia { id, source_path } => import_media(project, id, source_path),
            Command::RelinkMedia {
                media_id,
                source_path,
            } => relink_media(project, media_id, source_path),
            Command::SetMediaProxy {
                media_id,
                proxy_path,
                source_fingerprint,
            } => set_media_proxy(project, media_id, proxy_path, source_fingerprint),
            Command::CreateSequence { id, name } => create_sequence(project, id, name),
            Command::SetSequenceFrameRate {
                sequence_id,
                frame_rate,
            } => set_sequence_frame_rate(project, sequence_id, frame_rate),
            Command::AddMarker {
                sequence_id,
                id,
                time,
                label,
            } => add_marker(project, sequence_id, id, time, label),
            Command::DeleteMarker {
                sequence_id,
                marker_id,
            } => delete_marker(project, sequence_id, marker_id),
            Command::AddTransition {
                sequence_id,
                id,
                outgoing_clip,
                incoming_clip,
                duration,
                alignment,
                kind,
            } => add_transition(
                project,
                sequence_id,
                Transition {
                    id,
                    outgoing_clip,
                    incoming_clip,
                    duration,
                    alignment,
                    kind,
                },
            ),
            Command::DeleteTransition {
                sequence_id,
                transition_id,
            } => delete_transition(project, sequence_id, transition_id),
            Command::CreateTrack {
                sequence_id,
                id,
                kind,
            } => create_track(project, sequence_id, id, kind),
            Command::SetTrackLocked {
                sequence_id,
                track_id,
                locked,
            } => set_track_locked(project, sequence_id, track_id, locked),
            Command::SetTrackVisible {
                sequence_id,
                track_id,
                visible,
            } => set_track_visible(project, sequence_id, track_id, visible),
            Command::SetTrackMuted {
                sequence_id,
                track_id,
                muted,
            } => set_track_muted(project, sequence_id, track_id, muted),
            Command::ReorderTrack {
                sequence_id,
                track_id,
                destination_index,
            } => reorder_track(project, sequence_id, track_id, destination_index),
            Command::InsertClip {
                sequence_id,
                track_id,
                id,
                media_id,
                timeline_start,
                source_start,
                source_duration,
            } => insert_clip(
                project,
                ClipLocation {
                    sequence_id,
                    track_id,
                    id,
                    media_id,
                    timeline_start,
                    source_start,
                    source_duration,
                },
            ),
            Command::DuplicateClip {
                sequence_id,
                track_id,
                clip_id,
                new_clip_id,
                timeline_start,
            } => duplicate_clip(
                project,
                sequence_id,
                track_id,
                clip_id,
                new_clip_id,
                timeline_start,
            ),
            Command::MoveClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
            } => move_clip(project, sequence_id, track_id, clip_id, timeline_start),
            Command::SlipClip {
                sequence_id,
                track_id,
                clip_id,
                source_delta,
            } => slip_clip(project, sequence_id, track_id, clip_id, source_delta),
            Command::RollEdit {
                sequence_id,
                track_id,
                left_clip_id,
                right_clip_id,
                delta,
            } => roll_edit(
                project,
                sequence_id,
                track_id,
                left_clip_id,
                right_clip_id,
                delta,
            ),
            Command::SlideEdit {
                sequence_id,
                track_id,
                previous_clip_id,
                clip_id,
                next_clip_id,
                delta,
            } => slide_edit(
                project,
                sequence_id,
                track_id,
                previous_clip_id,
                clip_id,
                next_clip_id,
                delta,
            ),
            Command::TrimClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
                source_start,
                source_duration,
            } => trim_clip(
                project,
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
                source_start,
                source_duration,
            ),
            Command::SetClipTransform {
                sequence_id,
                track_id,
                clip_id,
                transform,
            } => set_clip_transform(project, sequence_id, track_id, clip_id, transform),
            Command::SplitClip {
                sequence_id,
                track_id,
                clip_id,
                right_clip_id,
                timeline_position,
            } => split_clip(
                project,
                sequence_id,
                track_id,
                clip_id,
                right_clip_id,
                timeline_position,
            ),
            Command::DeleteClip {
                sequence_id,
                track_id,
                clip_id,
            } => delete_clip(project, sequence_id, track_id, clip_id),
            Command::RippleDelete {
                sequence_id,
                track_id,
                clip_id,
            } => ripple_delete(project, sequence_id, track_id, clip_id),
        }
    }
}

mod operations;

use operations::{
    ClipLocation, add_marker, add_transition, create_sequence, create_track, delete_clip,
    delete_marker, delete_transition, duplicate_clip, import_media, insert_clip, move_clip,
    relink_media, reorder_track, ripple_delete, roll_edit, set_clip_transform, set_media_proxy,
    set_sequence_frame_rate, set_track_locked, set_track_muted, set_track_visible, slide_edit,
    slip_clip, split_clip, trim_clip,
};

#[cfg(test)]
mod tests;
