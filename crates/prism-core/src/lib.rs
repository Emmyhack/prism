//! Stable, UI-independent domain primitives shared across Prism.

use std::{
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
};
use uuid::Uuid;

/// A strongly typed, globally unique identifier.
pub struct Id<T> {
    value: Uuid,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Id<T> {}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T> Eq for Id<T> {}

impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

impl<T> Hash for Id<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl<T> Id<T> {
    /// Creates a new random identifier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            value: Uuid::new_v4(),
            marker: PhantomData,
        }
    }

    /// Reconstructs an identifier at a serialization boundary.
    #[must_use]
    pub const fn from_uuid(value: Uuid) -> Self {
        Self {
            value,
            marker: PhantomData,
        }
    }

    /// Returns the boundary-safe UUID value.
    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.value
    }
}

impl<T> Default for Id<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(formatter)
    }
}

/// Marker for a project identifier.
pub enum ProjectTag {}
/// Marker for a sequence identifier.
pub enum SequenceTag {}
/// Marker for a track identifier.
pub enum TrackTag {}
/// Marker for a clip identifier.
pub enum ClipTag {}
/// Marker for a media asset identifier.
pub enum MediaAssetTag {}
/// Marker for a sequence marker identifier.
pub enum MarkerTag {}
/// Marker for a sequence transition identifier.
pub enum TransitionTag {}

/// Project identifier.
pub type ProjectId = Id<ProjectTag>;
/// Sequence identifier.
pub type SequenceId = Id<SequenceTag>;
/// Track identifier.
pub type TrackId = Id<TrackTag>;
/// Clip identifier.
pub type ClipId = Id<ClipTag>;
/// Media asset identifier.
pub type MediaAssetId = Id<MediaAssetTag>;
/// Sequence marker identifier.
pub type MarkerId = Id<MarkerTag>;
/// Stable identifier for a sequence transition.
pub type TransitionId = Id<TransitionTag>;
