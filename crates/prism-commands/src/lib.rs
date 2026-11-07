//! Typed, validated project mutations shared by every interface.

use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_project::{
    Clip, Marker, MediaAsset, Project, Sequence, Track, TrackKind, Transition, TransitionKind,
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
    },
    /// Adds an empty sequence.
    CreateSequence {
        /// Stable ID selected before execution for deterministic replay.
        id: SequenceId,
        /// User-visible sequence name.
        name: String,
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
            } => set_media_proxy(project, media_id, proxy_path),
            Command::CreateSequence { id, name } => create_sequence(project, id, name),
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
                kind,
            } => add_transition(
                project,
                sequence_id,
                id,
                outgoing_clip,
                incoming_clip,
                duration,
                kind,
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

fn import_media(
    project: &mut Project,
    id: MediaAssetId,
    source_path: String,
) -> Result<Event, CommandError> {
    if source_path.trim().is_empty() {
        return Err(CommandError::EmptyField {
            field: "source_path",
        });
    }
    if project.media.contains_key(&id) {
        return Err(CommandError::DuplicateId);
    }
    if project
        .media
        .values()
        .any(|asset| asset.source_path == source_path)
    {
        return Err(CommandError::DuplicateMediaPath);
    }
    project.media.insert(
        id,
        MediaAsset {
            id,
            source_path,
            proxy_path: None,
        },
    );
    Ok(Event::MediaImported { id })
}

fn set_media_proxy(
    project: &mut Project,
    media_id: MediaAssetId,
    proxy_path: Option<String>,
) -> Result<Event, CommandError> {
    if proxy_path
        .as_deref()
        .is_some_and(|path| path.trim().is_empty())
    {
        return Err(CommandError::EmptyField {
            field: "proxy_path",
        });
    }
    let asset = project
        .media
        .get_mut(&media_id)
        .ok_or(CommandError::MediaNotFound)?;
    asset.proxy_path = proxy_path;
    Ok(Event::MediaProxyUpdated { id: media_id })
}

fn relink_media(
    project: &mut Project,
    media_id: MediaAssetId,
    source_path: String,
) -> Result<Event, CommandError> {
    if source_path.trim().is_empty() {
        return Err(CommandError::EmptyField {
            field: "source_path",
        });
    }
    if project
        .media
        .iter()
        .any(|(id, asset)| *id != media_id && asset.source_path == source_path)
    {
        return Err(CommandError::DuplicateMediaPath);
    }
    let asset = project
        .media
        .get_mut(&media_id)
        .ok_or(CommandError::MediaNotFound)?;
    asset.source_path = source_path;
    Ok(Event::MediaRelinked { id: media_id })
}

fn create_sequence(
    project: &mut Project,
    id: SequenceId,
    name: String,
) -> Result<Event, CommandError> {
    if name.trim().is_empty() {
        return Err(CommandError::EmptyField { field: "name" });
    }
    if project.sequences.contains_key(&id) {
        return Err(CommandError::DuplicateId);
    }
    project.sequences.insert(
        id,
        Sequence {
            id,
            name,
            markers: Vec::new(),
            transitions: Vec::new(),
            tracks: Vec::new(),
        },
    );
    Ok(Event::SequenceCreated { id })
}

fn add_marker(
    project: &mut Project,
    sequence_id: SequenceId,
    id: MarkerId,
    time: RationalTime,
    label: String,
) -> Result<Event, CommandError> {
    if !time.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "marker_time",
            constraint: "non-negative",
        });
    }
    if label.trim().is_empty() {
        return Err(CommandError::EmptyField { field: "label" });
    }
    if project
        .sequences
        .values()
        .any(|sequence| sequence.markers.iter().any(|marker| marker.id == id))
    {
        return Err(CommandError::DuplicateId);
    }
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let marker = Marker { id, time, label };
    let index = sequence
        .markers
        .partition_point(|existing| existing.time <= time);
    sequence.markers.insert(index, marker);
    Ok(Event::MarkerAdded { sequence_id, id })
}

fn delete_marker(
    project: &mut Project,
    sequence_id: SequenceId,
    marker_id: MarkerId,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let index = sequence
        .markers
        .iter()
        .position(|marker| marker.id == marker_id)
        .ok_or(CommandError::MarkerNotFound)?;
    sequence.markers.remove(index);
    Ok(Event::MarkerDeleted {
        sequence_id,
        id: marker_id,
    })
}

fn add_transition(
    project: &mut Project,
    sequence_id: SequenceId,
    id: TransitionId,
    outgoing_clip: ClipId,
    incoming_clip: ClipId,
    duration: RationalTime,
    kind: TransitionKind,
) -> Result<Event, CommandError> {
    if !duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "transition_duration",
            constraint: "positive",
        });
    }
    if project.sequences.values().any(|sequence| {
        sequence
            .transitions
            .iter()
            .any(|transition| transition.id == id)
    }) {
        return Err(CommandError::DuplicateId);
    }
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let track = sequence
        .tracks
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && track.clips.iter().any(|clip| clip.id == outgoing_clip)
                && track.clips.iter().any(|clip| clip.id == incoming_clip)
        })
        .ok_or(CommandError::ClipNotFound)?;
    let outgoing = track
        .clips
        .iter()
        .find(|clip| clip.id == outgoing_clip)
        .ok_or(CommandError::ClipNotFound)?;
    let incoming = track
        .clips
        .iter()
        .find(|clip| clip.id == incoming_clip)
        .ok_or(CommandError::ClipNotFound)?;
    let outgoing_end = outgoing
        .timeline_start
        .checked_add(outgoing.source_duration)
        .map_err(|_| CommandError::InvalidTime {
            field: "transition_boundary",
            constraint: "representable",
        })?;
    if incoming.timeline_start != outgoing_end
        || duration > outgoing.source_duration
        || duration > incoming.source_duration
    {
        return Err(CommandError::InvalidTransition);
    }
    sequence.transitions.push(Transition {
        id,
        outgoing_clip,
        incoming_clip,
        duration,
        kind,
    });
    Ok(Event::TransitionAdded { sequence_id, id })
}

fn delete_transition(
    project: &mut Project,
    sequence_id: SequenceId,
    transition_id: TransitionId,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let index = sequence
        .transitions
        .iter()
        .position(|transition| transition.id == transition_id)
        .ok_or(CommandError::TransitionNotFound)?;
    sequence.transitions.remove(index);
    Ok(Event::TransitionDeleted {
        sequence_id,
        id: transition_id,
    })
}

fn create_track(
    project: &mut Project,
    sequence_id: SequenceId,
    id: TrackId,
    kind: TrackKind,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    if sequence.tracks.iter().any(|track| track.id == id) {
        return Err(CommandError::DuplicateId);
    }
    sequence.tracks.push(Track {
        id,
        kind,
        clips: Vec::new(),
        locked: false,
        visible: true,
        muted: false,
    });
    Ok(Event::TrackCreated { sequence_id, id })
}

fn set_track_locked(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    locked: bool,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    track.locked = locked;
    Ok(Event::TrackLockChanged { track_id, locked })
}

fn set_track_visible(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    visible: bool,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    track.visible = visible;
    Ok(Event::TrackVisibilityChanged { track_id, visible })
}

fn set_track_muted(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    muted: bool,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    track.muted = muted;
    Ok(Event::TrackMuteChanged { track_id, muted })
}

fn reorder_track(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    destination_index: usize,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let current_index = sequence
        .tracks
        .iter()
        .position(|track| track.id == track_id)
        .ok_or(CommandError::TrackNotFound)?;
    if destination_index >= sequence.tracks.len() {
        return Err(CommandError::InvalidTrackIndex);
    }
    if current_index != destination_index {
        let track = sequence.tracks.remove(current_index);
        sequence.tracks.insert(destination_index, track);
    }
    Ok(Event::TrackReordered {
        sequence_id,
        track_id,
        destination_index,
    })
}

#[derive(Clone, Copy)]
struct ClipLocation {
    sequence_id: SequenceId,
    track_id: TrackId,
    id: ClipId,
    media_id: MediaAssetId,
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
}

fn insert_clip(project: &mut Project, location: ClipLocation) -> Result<Event, CommandError> {
    validate_clip_time(
        location.timeline_start,
        location.source_start,
        location.source_duration,
    )?;
    if !project.media.contains_key(&location.media_id) {
        return Err(CommandError::MediaNotFound);
    }
    let track = find_track_mut(project, location.sequence_id, location.track_id)?;
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    if track.clips.iter().any(|clip| clip.id == location.id) {
        return Err(CommandError::DuplicateId);
    }
    ensure_range_available(
        track,
        location.timeline_start,
        location.source_duration,
        None,
    )?;
    track.clips.push(Clip {
        id: location.id,
        media_id: location.media_id,
        timeline_start: location.timeline_start,
        source_start: location.source_start,
        source_duration: location.source_duration,
    });
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipInserted {
        track_id: location.track_id,
        id: location.id,
    })
}

fn duplicate_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_clip_id: ClipId,
    timeline_start: RationalTime,
) -> Result<Event, CommandError> {
    let (media_id, source_start, source_duration) = {
        let track = find_track_mut(project, sequence_id, track_id)?;
        track
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .map(|clip| (clip.media_id, clip.source_start, clip.source_duration))
            .ok_or(CommandError::ClipNotFound)?
    };
    insert_clip(
        project,
        ClipLocation {
            sequence_id,
            track_id,
            id: new_clip_id,
            media_id,
            timeline_start,
            source_start,
            source_duration,
        },
    )?;
    Ok(Event::ClipDuplicated {
        source_id: clip_id,
        id: new_clip_id,
    })
}

fn move_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    timeline_start: RationalTime,
) -> Result<Event, CommandError> {
    if !timeline_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "timeline_start",
            constraint: "non-negative",
        });
    }
    let track = find_track_mut(project, sequence_id, track_id)?;
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    let duration = track
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?
        .source_duration;
    ensure_range_available(track, timeline_start, duration, Some(clip_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    clip.timeline_start = timeline_start;
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipMoved {
        id: clip_id,
        timeline_start,
    })
}

fn slip_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    source_delta: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let source_start =
        clip.source_start
            .checked_add(source_delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "source_delta",
                constraint: "representable",
            })?;
    if !source_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "source_delta",
            constraint: "source start must remain non-negative",
        });
    }
    clip.source_start = source_start;
    Ok(Event::ClipSlipped { id: clip_id })
}

fn roll_edit(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    left_clip_id: ClipId,
    right_clip_id: ClipId,
    delta: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let left_index = track
        .clips
        .iter()
        .position(|clip| clip.id == left_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let right_index = track
        .clips
        .iter()
        .position(|clip| clip.id == right_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    if right_index != left_index + 1 {
        return Err(CommandError::InvalidTime {
            field: "clip_boundary",
            constraint: "clips must be adjacent",
        });
    }
    let left_duration = track.clips[left_index].source_duration;
    let right_duration = track.clips[right_index].source_duration;
    let new_left_duration =
        left_duration
            .checked_add(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    let new_right_duration =
        right_duration
            .checked_sub(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    if !new_left_duration.is_positive() || !new_right_duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "resulting durations must be positive",
        });
    }
    let right_source_start = track.clips[right_index]
        .source_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    if !right_source_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "source start must remain non-negative",
        });
    }
    track.clips[left_index].source_duration = new_left_duration;
    track.clips[right_index].timeline_start = track.clips[right_index]
        .timeline_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    track.clips[right_index].source_start = right_source_start;
    track.clips[right_index].source_duration = new_right_duration;
    Ok(Event::ClipsRolled {
        left_clip_id,
        right_clip_id,
    })
}

fn slide_edit(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    previous_clip_id: ClipId,
    clip_id: ClipId,
    next_clip_id: ClipId,
    delta: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let previous_index = track
        .clips
        .iter()
        .position(|clip| clip.id == previous_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let clip_index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let next_index = track
        .clips
        .iter()
        .position(|clip| clip.id == next_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    if clip_index != previous_index + 1 || next_index != clip_index + 1 {
        return Err(CommandError::InvalidTime {
            field: "clip_neighbors",
            constraint: "clips must be adjacent",
        });
    }
    let previous_duration = track.clips[previous_index].source_duration;
    let next_duration = track.clips[next_index].source_duration;
    let new_previous_duration =
        previous_duration
            .checked_add(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    let new_next_duration =
        next_duration
            .checked_sub(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    if !new_previous_duration.is_positive() || !new_next_duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "neighbor durations must remain positive",
        });
    }
    let new_clip_start = track.clips[clip_index]
        .timeline_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    if !new_clip_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "clip start must remain non-negative",
        });
    }
    let new_next_source_start = track.clips[next_index]
        .source_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    if !new_next_source_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "source start must remain non-negative",
        });
    }
    track.clips[previous_index].source_duration = new_previous_duration;
    track.clips[clip_index].timeline_start = new_clip_start;
    track.clips[next_index].timeline_start = track.clips[next_index]
        .timeline_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    track.clips[next_index].source_start = new_next_source_start;
    track.clips[next_index].source_duration = new_next_duration;
    Ok(Event::ClipsSlid { id: clip_id })
}

#[allow(clippy::too_many_arguments)]
fn trim_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
) -> Result<Event, CommandError> {
    validate_clip_time(timeline_start, source_start, source_duration)?;
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    if !track.clips.iter().any(|clip| clip.id == clip_id) {
        return Err(CommandError::ClipNotFound);
    }
    ensure_range_available(track, timeline_start, source_duration, Some(clip_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    clip.timeline_start = timeline_start;
    clip.source_start = source_start;
    clip.source_duration = source_duration;
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipTrimmed { id: clip_id })
}

fn split_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    right_clip_id: ClipId,
    timeline_position: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    if track.clips.iter().any(|clip| clip.id == right_clip_id) {
        return Err(CommandError::DuplicateId);
    }
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let original = track.clips[index].clone();
    let original_end = original
        .timeline_start
        .checked_add(original.source_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    if timeline_position <= original.timeline_start || timeline_position >= original_end {
        return Err(CommandError::InvalidTime {
            field: "timeline_position",
            constraint: "strictly inside the clip",
        });
    }
    let left_duration = timeline_position
        .checked_sub(original.timeline_start)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    let right_source_start = original
        .source_start
        .checked_add(left_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    let right_duration = original
        .source_duration
        .checked_sub(left_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    track.clips[index].source_duration = left_duration;
    track.clips.insert(
        index + 1,
        Clip {
            id: right_clip_id,
            media_id: original.media_id,
            timeline_start: timeline_position,
            source_start: right_source_start,
            source_duration: right_duration,
        },
    );
    Ok(Event::ClipSplit {
        left_id: clip_id,
        right_id: right_clip_id,
    })
}

fn delete_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    track.clips.remove(index);
    Ok(Event::ClipDeleted { id: clip_id })
}

fn ripple_delete(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let removed = track.clips.remove(index);
    let mut shifted_count = 0;
    for clip in track
        .clips
        .iter_mut()
        .filter(|clip| clip.timeline_start > removed.timeline_start)
    {
        clip.timeline_start = clip
            .timeline_start
            .checked_sub(removed.source_duration)
            .map_err(|_| CommandError::InvalidTime {
                field: "timeline_start",
                constraint: "representable",
            })?;
        shifted_count += 1;
    }
    Ok(Event::ClipRippleDeleted {
        id: clip_id,
        shifted_count,
    })
}

fn ensure_track_editable(track: &Track) -> Result<(), CommandError> {
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    Ok(())
}

fn find_track_mut(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
) -> Result<&mut Track, CommandError> {
    project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?
        .tracks
        .iter_mut()
        .find(|track| track.id == track_id)
        .ok_or(CommandError::TrackNotFound)
}

fn validate_clip_time(
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
) -> Result<(), CommandError> {
    for (field, value) in [
        ("timeline_start", timeline_start),
        ("source_start", source_start),
    ] {
        if !value.is_non_negative() {
            return Err(CommandError::InvalidTime {
                field,
                constraint: "non-negative",
            });
        }
    }
    if !source_duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "source_duration",
            constraint: "positive",
        });
    }
    Ok(())
}

fn ensure_range_available(
    track: &Track,
    start: RationalTime,
    duration: RationalTime,
    ignored_clip: Option<ClipId>,
) -> Result<(), CommandError> {
    let end = start
        .checked_add(duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    for clip in &track.clips {
        if ignored_clip == Some(clip.id) {
            continue;
        }
        let clip_end = clip
            .timeline_start
            .checked_add(clip.source_duration)
            .map_err(|error| CommandError::Commit(error.to_string()))?;
        if start < clip_end && clip.timeline_start < end {
            return Err(CommandError::ClipOverlap);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Command, CommandEngine, CommandError, CommandRegistry, Event};
    use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
    use prism_project::{Project, TrackKind, TransitionKind};
    use prism_timeline::{RationalTime, TimeError};

    #[test]
    fn registry_exposes_stable_descriptors_and_context_predicates() -> Result<(), CommandError> {
        let project = Project::new("Test");
        let undo = CommandRegistry::find("timeline.undo").ok_or(CommandError::EmptyTransaction)?;
        assert_eq!(undo.label, "Undo");
        assert_eq!(undo.shortcut, Some("Cmd+Z"));
        assert!(!undo.is_enabled(&project));
        let play =
            CommandRegistry::find("playback.play_pause").ok_or(CommandError::EmptyTransaction)?;
        assert!(play.is_enabled(&project));
        assert!(CommandRegistry::find("project.set_proxy").is_some());
        assert!(CommandRegistry::find("timeline.insert_clip").is_some());
        assert!(CommandRegistry::find("timeline.ripple_delete").is_some());
        Ok(())
    }

    #[test]
    fn registry_execution_keeps_validation_and_undo_transactional() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let create =
            CommandRegistry::find("project.import_media").ok_or(CommandError::EmptyTransaction)?;
        let media_id = MediaAssetId::new();
        create.execute(
            &mut project,
            Command::ImportMedia {
                id: media_id,
                source_path: "/tmp/source.mp4".to_owned(),
            },
        )?;
        assert_eq!(project.media.len(), 1);
        assert_eq!(project.revision(), 1);
        Ok(())
    }

    #[test]
    fn relink_media_updates_path_and_is_undoable() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let media_id = MediaAssetId::new();
        CommandEngine::execute(
            &mut project,
            Command::ImportMedia {
                id: media_id,
                source_path: "/offline/original.mov".to_owned(),
            },
        )?;
        let relink =
            CommandRegistry::find("project.relink_media").ok_or(CommandError::EmptyTransaction)?;
        let outcome = relink.execute(
            &mut project,
            Command::RelinkMedia {
                media_id,
                source_path: "/Volumes/Media/original.mov".to_owned(),
            },
        )?;
        assert_eq!(outcome, vec![Event::MediaRelinked { id: media_id }]);
        assert_eq!(
            project.media[&media_id].source_path,
            "/Volumes/Media/original.mov"
        );
        Ok(())
    }

    #[test]
    fn rejected_command_does_not_advance_revision() {
        let mut project = Project::new("Test");
        let result = CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: SequenceId::new(),
                name: "  ".to_owned(),
            },
        );
        assert_eq!(result, Err(CommandError::EmptyField { field: "name" }));
        assert_eq!(project.revision(), 0);
        assert!(project.sequences.is_empty());
    }

    #[test]
    fn creates_sequence_and_track_through_commands() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: sequence_id,
                name: "Main".to_owned(),
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::CreateTrack {
                sequence_id,
                id: track_id,
                kind: TrackKind::Video,
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::SetTrackLocked {
                sequence_id,
                track_id,
                locked: true,
            },
        )?;
        assert!(project.sequences[&sequence_id].tracks[0].locked);
        assert_eq!(project.revision(), 3);
        assert_eq!(project.sequences[&sequence_id].tracks.len(), 1);
        Ok(())
    }

    #[test]
    fn media_proxy_command_sets_clears_and_validates_relinks() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let media_id = MediaAssetId::new();
        CommandEngine::execute(
            &mut project,
            Command::ImportMedia {
                id: media_id,
                source_path: "source.mp4".to_owned(),
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::SetMediaProxy {
                media_id,
                proxy_path: Some("proxy.mp4".to_owned()),
            },
        )?;
        assert_eq!(
            project.media[&media_id].proxy_path.as_deref(),
            Some("proxy.mp4")
        );
        CommandEngine::execute(
            &mut project,
            Command::SetMediaProxy {
                media_id,
                proxy_path: None,
            },
        )?;
        assert!(!project.media[&media_id].has_proxy_path());
        assert_eq!(
            CommandEngine::execute(
                &mut project,
                Command::SetMediaProxy {
                    media_id,
                    proxy_path: Some("  ".to_owned()),
                },
            ),
            Err(CommandError::EmptyField {
                field: "proxy_path"
            })
        );
        Ok(())
    }

    #[test]
    fn reorders_tracks_with_stable_ids() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let first = TrackId::new();
        let second = TrackId::new();
        CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: sequence_id,
                name: "Main".to_owned(),
            },
        )?;
        for id in [first, second] {
            CommandEngine::execute(
                &mut project,
                Command::CreateTrack {
                    sequence_id,
                    id,
                    kind: TrackKind::Video,
                },
            )?;
        }
        let event = CommandEngine::execute(
            &mut project,
            Command::ReorderTrack {
                sequence_id,
                track_id: first,
                destination_index: 1,
            },
        )?;
        assert_eq!(
            event,
            vec![Event::TrackReordered {
                sequence_id,
                track_id: first,
                destination_index: 1,
            }]
        );
        assert_eq!(
            project.sequences[&sequence_id]
                .tracks
                .iter()
                .map(|track| track.id)
                .collect::<Vec<_>>(),
            vec![second, first]
        );
        assert!(matches!(
            CommandEngine::execute(
                &mut project,
                Command::ReorderTrack {
                    sequence_id,
                    track_id: first,
                    destination_index: 2,
                }
            ),
            Err(CommandError::InvalidTrackIndex)
        ));
        Ok(())
    }

    #[test]
    fn updates_track_visibility_and_mute_state() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: sequence_id,
                name: "Main".to_owned(),
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::CreateTrack {
                sequence_id,
                id: track_id,
                kind: TrackKind::Video,
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::SetTrackVisible {
                sequence_id,
                track_id,
                visible: false,
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::SetTrackMuted {
                sequence_id,
                track_id,
                muted: true,
            },
        )?;
        let track = &project.sequences[&sequence_id].tracks[0];
        assert!(!track.visible);
        assert!(track.muted);
        Ok(())
    }

    #[test]
    fn markers_are_sorted_and_removable() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: sequence_id,
                name: "Main".to_owned(),
            },
        )?;
        let late = MarkerId::new();
        let early = MarkerId::new();
        let late_time = RationalTime::new(10, 1)?;
        let early_time = RationalTime::new(2, 1)?;
        CommandEngine::execute(
            &mut project,
            Command::AddMarker {
                sequence_id,
                id: late,
                time: late_time,
                label: "Late".to_owned(),
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::AddMarker {
                sequence_id,
                id: early,
                time: early_time,
                label: "Early".to_owned(),
            },
        )?;
        assert_eq!(
            project.sequences[&sequence_id]
                .markers
                .iter()
                .map(|marker| marker.id)
                .collect::<Vec<_>>(),
            vec![early, late]
        );
        CommandEngine::execute(
            &mut project,
            Command::DeleteMarker {
                sequence_id,
                marker_id: early,
            },
        )?;
        assert_eq!(project.sequences[&sequence_id].markers.len(), 1);
        Ok(())
    }

    #[test]
    fn markers_reject_invalid_values() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: sequence_id,
                name: "Main".to_owned(),
            },
        )?;
        let id = MarkerId::new();
        let negative_time = RationalTime::new(-1, 1)?;
        assert!(matches!(
            CommandEngine::execute(
                &mut project,
                Command::AddMarker {
                    sequence_id,
                    id,
                    time: negative_time,
                    label: "x".to_owned()
                }
            ),
            Err(CommandError::InvalidTime { .. })
        ));
        assert!(matches!(
            CommandEngine::execute(
                &mut project,
                Command::AddMarker {
                    sequence_id,
                    id,
                    time: RationalTime::ZERO,
                    label: " ".to_owned()
                }
            ),
            Err(CommandError::EmptyField { field: "label" })
        ));
        CommandEngine::execute(
            &mut project,
            Command::AddMarker {
                sequence_id,
                id,
                time: RationalTime::ZERO,
                label: "x".to_owned(),
            },
        )?;
        assert!(matches!(
            CommandEngine::execute(
                &mut project,
                Command::AddMarker {
                    sequence_id,
                    id,
                    time: RationalTime::ZERO,
                    label: "duplicate".to_owned()
                }
            ),
            Err(CommandError::DuplicateId)
        ));
        Ok(())
    }

    #[test]
    fn duplicate_clip_reuses_source_range_at_requested_position()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let clip_id = ClipId::new();
        let duplicate_id = ClipId::new();
        CommandEngine::execute_transaction(
            &mut project,
            "Build",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "source.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: clip_id,
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::new(5, 1)?,
                    source_duration: RationalTime::new(2, 1)?,
                },
            ],
        )?;
        let event = CommandEngine::execute(
            &mut project,
            Command::DuplicateClip {
                sequence_id,
                track_id,
                clip_id,
                new_clip_id: duplicate_id,
                timeline_start: RationalTime::new(4, 1)?,
            },
        )?;
        assert_eq!(
            event,
            vec![Event::ClipDuplicated {
                source_id: clip_id,
                id: duplicate_id
            }]
        );
        let clips = &project.sequences[&sequence_id].tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[1].timeline_start, RationalTime::new(4, 1)?);
        assert_eq!(clips[1].source_start, RationalTime::new(5, 1)?);
        Ok(())
    }

    #[test]
    fn ripple_delete_closes_gap_without_touching_source_ranges()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let first = ClipId::new();
        let removed = ClipId::new();
        let later = ClipId::new();
        CommandEngine::execute_transaction(
            &mut project,
            "Build",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "source.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: first,
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: RationalTime::new(2, 1)?,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: removed,
                    media_id,
                    timeline_start: RationalTime::new(2, 1)?,
                    source_start: RationalTime::new(8, 1)?,
                    source_duration: RationalTime::new(3, 1)?,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: later,
                    media_id,
                    timeline_start: RationalTime::new(5, 1)?,
                    source_start: RationalTime::new(20, 1)?,
                    source_duration: RationalTime::new(1, 1)?,
                },
            ],
        )?;
        let events = CommandEngine::execute(
            &mut project,
            Command::RippleDelete {
                sequence_id,
                track_id,
                clip_id: removed,
            },
        )?;
        assert_eq!(
            events,
            vec![Event::ClipRippleDeleted {
                id: removed,
                shifted_count: 1
            }]
        );
        let clips = &project.sequences[&sequence_id].tracks[0].clips;
        let later_clip = clips
            .iter()
            .find(|clip| clip.id == later)
            .ok_or("missing later clip")?;
        assert_eq!(later_clip.timeline_start, RationalTime::new(2, 1)?);
        assert_eq!(later_clip.source_start, RationalTime::new(20, 1)?);
        Ok(())
    }

    #[test]
    fn transitions_require_adjacent_video_clips() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let media_id = MediaAssetId::new();
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let outgoing = ClipId::new();
        let incoming = ClipId::new();
        CommandEngine::execute_transaction(
            &mut project,
            "Build",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "source.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: outgoing,
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: RationalTime::new(2, 1)?,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: incoming,
                    media_id,
                    timeline_start: RationalTime::new(2, 1)?,
                    source_start: RationalTime::new(2, 1)?,
                    source_duration: RationalTime::new(2, 1)?,
                },
            ],
        )?;
        let transition_id = TransitionId::new();
        let events = CommandEngine::execute(
            &mut project,
            Command::AddTransition {
                sequence_id,
                id: transition_id,
                outgoing_clip: outgoing,
                incoming_clip: incoming,
                duration: RationalTime::new(1, 2)?,
                kind: TransitionKind::Dissolve,
            },
        )?;
        assert_eq!(
            events,
            vec![Event::TransitionAdded {
                sequence_id,
                id: transition_id
            }]
        );
        assert_eq!(
            project.sequences[&sequence_id].transitions[0].duration,
            RationalTime::new(1, 2)?
        );
        let deleted = CommandEngine::execute(
            &mut project,
            Command::DeleteTransition {
                sequence_id,
                transition_id,
            },
        )?;
        assert_eq!(
            deleted,
            vec![Event::TransitionDeleted {
                sequence_id,
                id: transition_id
            }]
        );
        Ok(())
    }

    #[test]
    fn failed_batch_rolls_back_every_command() {
        let mut project = Project::new("Test");
        let created_id = SequenceId::new();
        let result = CommandEngine::execute_transaction(
            &mut project,
            "Invalid batch",
            vec![
                Command::CreateSequence {
                    id: created_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id: SequenceId::new(),
                    id: TrackId::new(),
                    kind: TrackKind::Video,
                },
            ],
        );
        assert!(matches!(result, Err(CommandError::SequenceNotFound)));
        assert!(!project.sequences.contains_key(&created_id));
        assert_eq!(project.revision(), 0);
    }

    #[test]
    fn dry_run_reports_events_without_mutating_project() -> Result<(), CommandError> {
        let project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let events = CommandEngine::validate_transaction(
            &project,
            vec![Command::CreateSequence {
                id: sequence_id,
                name: "Preview".to_owned(),
            }],
        )?;
        assert_eq!(events.len(), 1);
        assert_eq!(project.revision(), 0);
        assert!(project.sequences.is_empty());
        Ok(())
    }

    #[test]
    fn undo_and_redo_restore_complete_transaction() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let outcome = CommandEngine::execute_transaction(
            &mut project,
            "Create timeline",
            vec![Command::CreateSequence {
                id: sequence_id,
                name: "Main".to_owned(),
            }],
        )?;
        assert_eq!(project.revision(), 1);
        assert!(project.sequences.contains_key(&sequence_id));

        let redo = outcome.undo.apply(&mut project)?;
        assert_eq!(project.revision(), 0);
        assert!(!project.sequences.contains_key(&sequence_id));

        let _undo_again = redo.apply(&mut project)?;
        assert_eq!(project.revision(), 1);
        assert!(project.sequences.contains_key(&sequence_id));
        Ok(())
    }

    #[test]
    fn clip_commands_enforce_non_overlapping_ranges() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let media_id = MediaAssetId::new();
        let first_clip_id = ClipId::new();
        let second_clip_id = ClipId::new();
        let second = RationalTime::new(2, 1)?;
        let duration = RationalTime::new(2, 1)?;

        CommandEngine::execute_transaction(
            &mut project,
            "Build timeline",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "/media/interview.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: first_clip_id,
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: duration,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: second_clip_id,
                    media_id,
                    timeline_start: second,
                    source_start: RationalTime::ZERO,
                    source_duration: duration,
                },
            ],
        )?;

        let overlap = CommandEngine::execute(
            &mut project,
            Command::MoveClip {
                sequence_id,
                track_id,
                clip_id: second_clip_id,
                timeline_start: RationalTime::new(1, 1)?,
            },
        );
        assert!(matches!(overlap, Err(CommandError::ClipOverlap)));
        let track = &project.sequences[&sequence_id].tracks[0];
        assert_eq!(track.clips[1].timeline_start, second);
        Ok(())
    }

    #[test]
    fn locked_tracks_reject_all_clip_mutations() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let media_id = MediaAssetId::new();
        let clip_id = ClipId::new();
        let duration = RationalTime::new(4, 1)?;
        CommandEngine::execute_transaction(
            &mut project,
            "Create locked timeline",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "/media/source.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: clip_id,
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: duration,
                },
                Command::SetTrackLocked {
                    sequence_id,
                    track_id,
                    locked: true,
                },
            ],
        )?;

        let commands = [
            Command::InsertClip {
                sequence_id,
                track_id,
                id: ClipId::new(),
                media_id,
                timeline_start: RationalTime::new(5, 1)?,
                source_start: RationalTime::ZERO,
                source_duration: duration,
            },
            Command::MoveClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start: RationalTime::new(1, 1)?,
            },
            Command::TrimClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start: RationalTime::ZERO,
                source_start: RationalTime::ZERO,
                source_duration: RationalTime::new(2, 1)?,
            },
            Command::SplitClip {
                sequence_id,
                track_id,
                clip_id,
                right_clip_id: ClipId::new(),
                timeline_position: RationalTime::new(2, 1)?,
            },
            Command::DeleteClip {
                sequence_id,
                track_id,
                clip_id,
            },
        ];
        for command in commands {
            assert!(matches!(
                CommandEngine::execute(&mut project, command),
                Err(CommandError::TrackLocked)
            ));
        }
        assert_eq!(project.revision(), 1);
        assert_eq!(project.sequences[&sequence_id].tracks[0].clips.len(), 1);
        Ok(())
    }

    #[test]
    fn exact_time_ordering_handles_different_timebases() -> Result<(), TimeError> {
        assert!(RationalTime::new(1, 24)? < RationalTime::new(1, 23)?);
        Ok(())
    }

    #[test]
    fn stale_revision_cannot_commit() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: SequenceId::new(),
                name: "Current".to_owned(),
            },
        )?;
        let result = CommandEngine::execute_transaction_at_revision(
            &mut project,
            0,
            "Stale edit",
            vec![Command::CreateSequence {
                id: SequenceId::new(),
                name: "Stale".to_owned(),
            }],
        );
        assert!(matches!(
            result,
            Err(CommandError::RevisionConflict {
                expected: 0,
                actual: 1
            })
        ));
        assert_eq!(project.sequences.len(), 1);
        Ok(())
    }

    #[test]
    fn stale_undo_cannot_discard_later_edits() -> Result<(), CommandError> {
        let mut project = Project::new("Test");
        let first = CommandEngine::execute_transaction(
            &mut project,
            "First",
            vec![Command::CreateSequence {
                id: SequenceId::new(),
                name: "First".to_owned(),
            }],
        )?;
        CommandEngine::execute(
            &mut project,
            Command::CreateSequence {
                id: SequenceId::new(),
                name: "Later".to_owned(),
            },
        )?;
        let result = first.undo.apply(&mut project);
        assert!(matches!(
            result,
            Err(CommandError::RevisionConflict {
                expected: 1,
                actual: 2
            })
        ));
        assert_eq!(project.sequences.len(), 2);
        Ok(())
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn split_trim_and_delete_preserve_exact_source_ranges() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let media_id = MediaAssetId::new();
        let left_id = ClipId::new();
        let right_id = ClipId::new();
        let four = RationalTime::new(4, 1)?;
        let two = RationalTime::new(2, 1)?;

        CommandEngine::execute_transaction(
            &mut project,
            "Create clip",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "/media/source.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: left_id,
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: four,
                },
            ],
        )?;
        CommandEngine::execute(
            &mut project,
            Command::SlipClip {
                sequence_id,
                track_id,
                clip_id: left_id,
                source_delta: RationalTime::new(1, 2)?,
            },
        )?;
        assert_eq!(
            project.sequences[&sequence_id].tracks[0].clips[0].source_start,
            RationalTime::new(1, 2)?
        );
        CommandEngine::execute(
            &mut project,
            Command::SplitClip {
                sequence_id,
                track_id,
                clip_id: left_id,
                right_clip_id: right_id,
                timeline_position: two,
            },
        )?;
        let clips = &project.sequences[&sequence_id].tracks[0].clips;
        assert_eq!(clips[0].source_duration, two);
        assert_eq!(clips[1].timeline_start, two);
        assert_eq!(clips[1].source_start, RationalTime::new(5, 2)?);
        assert_eq!(clips[1].source_duration, two);

        CommandEngine::execute(
            &mut project,
            Command::RollEdit {
                sequence_id,
                track_id,
                left_clip_id: left_id,
                right_clip_id: right_id,
                delta: RationalTime::new(1, 2)?,
            },
        )?;
        let clips = &project.sequences[&sequence_id].tracks[0].clips;
        assert_eq!(clips[0].source_duration, RationalTime::new(5, 2)?);
        assert_eq!(clips[1].timeline_start, RationalTime::new(5, 2)?);
        assert_eq!(clips[1].source_start, RationalTime::new(3, 1)?);
        assert_eq!(clips[1].source_duration, RationalTime::new(3, 2)?);

        CommandEngine::execute(
            &mut project,
            Command::TrimClip {
                sequence_id,
                track_id,
                clip_id: right_id,
                timeline_start: RationalTime::new(3, 1)?,
                source_start: RationalTime::new(3, 1)?,
                source_duration: RationalTime::new(1, 1)?,
            },
        )?;
        CommandEngine::execute(
            &mut project,
            Command::DeleteClip {
                sequence_id,
                track_id,
                clip_id: left_id,
            },
        )?;
        let clips = &project.sequences[&sequence_id].tracks[0].clips;
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].id, right_id);
        assert_eq!(clips[0].timeline_start, RationalTime::new(3, 1)?);
        Ok(())
    }

    #[test]
    fn slide_edit_moves_clip_and_adjusts_neighbors() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Test");
        let sequence_id = SequenceId::new();
        let track_id = TrackId::new();
        let media_id = MediaAssetId::new();
        let previous = ClipId::new();
        let selected = ClipId::new();
        let next = ClipId::new();
        let duration = RationalTime::new(2, 1)?;
        CommandEngine::execute_transaction(
            &mut project,
            "Create slide timeline",
            vec![
                Command::ImportMedia {
                    id: media_id,
                    source_path: "/media/source.mov".to_owned(),
                },
                Command::CreateSequence {
                    id: sequence_id,
                    name: "Main".to_owned(),
                },
                Command::CreateTrack {
                    sequence_id,
                    id: track_id,
                    kind: TrackKind::Video,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: previous,
                    media_id,
                    timeline_start: RationalTime::ZERO,
                    source_start: RationalTime::ZERO,
                    source_duration: duration,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: selected,
                    media_id,
                    timeline_start: duration,
                    source_start: RationalTime::ZERO,
                    source_duration: duration,
                },
                Command::InsertClip {
                    sequence_id,
                    track_id,
                    id: next,
                    media_id,
                    timeline_start: RationalTime::new(4, 1)?,
                    source_start: RationalTime::ZERO,
                    source_duration: duration,
                },
            ],
        )?;
        CommandEngine::execute(
            &mut project,
            Command::SlideEdit {
                sequence_id,
                track_id,
                previous_clip_id: previous,
                clip_id: selected,
                next_clip_id: next,
                delta: RationalTime::new(1, 2)?,
            },
        )?;
        let clips = &project.sequences[&sequence_id].tracks[0].clips;
        assert_eq!(clips[0].source_duration, RationalTime::new(5, 2)?);
        assert_eq!(clips[1].timeline_start, RationalTime::new(5, 2)?);
        assert_eq!(clips[1].source_duration, duration);
        assert_eq!(clips[2].timeline_start, RationalTime::new(9, 2)?);
        assert_eq!(clips[2].source_start, RationalTime::new(1, 2)?);
        assert_eq!(clips[2].source_duration, RationalTime::new(3, 2)?);
        Ok(())
    }
}
