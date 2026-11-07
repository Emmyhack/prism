//! Validated synchronization and live-switch planning for multicamera edits.

use prism_core::MediaAssetId;
use prism_timeline::{RationalTime, TimeError};
use std::collections::BTreeSet;
use thiserror::Error;

/// One synchronized camera source in a multicamera group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraAngle {
    /// Stable name used by switch decisions, such as `wide` or `camera-b`.
    pub id: String,
    /// Nondestructive source-media reference for this angle.
    pub media_id: MediaAssetId,
    /// Exact source time corresponding to multicamera timeline zero.
    ///
    /// A positive offset means the source began before the shared sync point;
    /// a negative offset means it does not yet have media at group time zero.
    pub sync_offset: RationalTime,
}

impl CameraAngle {
    /// Maps shared multicamera time to exact source-media time.
    ///
    /// # Errors
    ///
    /// Returns [`MulticamError::NegativeTimelineTime`] for negative group
    /// time, [`MulticamError::SourceUnavailable`] before this angle begins,
    /// or an arithmetic error if the exact sum overflows.
    pub fn source_time_at(
        &self,
        timeline_time: RationalTime,
    ) -> Result<RationalTime, MulticamError> {
        if timeline_time.is_negative() {
            return Err(MulticamError::NegativeTimelineTime);
        }
        let source_time = timeline_time.checked_add(self.sync_offset)?;
        if source_time.is_negative() {
            return Err(MulticamError::SourceUnavailable {
                angle_id: self.id.clone(),
            });
        }
        Ok(source_time)
    }
}

/// One cut decision on the shared multicamera timeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MulticamSwitch {
    /// Exact time at which this angle becomes active.
    pub timeline_time: RationalTime,
    /// Referenced [`CameraAngle::id`].
    pub angle_id: String,
}

/// Multicamera synchronization and switching validation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum MulticamError {
    /// At least one synchronized source is required.
    #[error("a multicamera plan requires at least one camera angle")]
    NoAngles,
    /// Angle IDs are user-facing stable references and cannot be blank.
    #[error("camera angle IDs must not be blank")]
    EmptyAngleId,
    /// Angle identifiers must resolve without ambiguity.
    #[error("camera angle ID `{0}` is duplicated")]
    DuplicateAngleId(String),
    /// Each source asset may represent only one angle in a group.
    #[error("a media asset is assigned to more than one camera angle")]
    DuplicateMedia,
    /// A switching plan must define its initial active angle.
    #[error("a multicamera plan requires at least one switch")]
    NoSwitches,
    /// Deterministic plans begin with an angle selected at timeline zero.
    #[error("the first multicamera switch must be at timeline zero")]
    NonZeroFirstSwitch,
    /// Cuts must be ordered and cannot share the same instant.
    #[error("multicamera switches must be strictly increasing")]
    NonIncreasingSwitches,
    /// A cut references an angle outside this group.
    #[error("multicamera switch references unknown angle `{0}`")]
    UnknownAngle(String),
    /// Consecutive cuts to the same angle are redundant and likely accidental.
    #[error("consecutive switches must select different angles")]
    RedundantSwitch,
    /// Queries use clip-local nonnegative time.
    #[error("multicamera timeline time must not be negative")]
    NegativeTimelineTime,
    /// The synchronized source has not begun at this shared time.
    #[error("camera angle `{angle_id}` has no source media at this time")]
    SourceUnavailable {
        /// Angle whose exact source time would be negative.
        angle_id: String,
    },
    /// Exact time arithmetic failed.
    #[error("multicamera time arithmetic failed: {0}")]
    Time(#[from] TimeError),
}

/// A validated, deterministic multicamera switching plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MulticamPlan {
    angles: Vec<CameraAngle>,
    switches: Vec<MulticamSwitch>,
}

impl MulticamPlan {
    /// Builds a synchronized switching plan.
    ///
    /// Angles may be supplied in display order. Switches must begin at zero,
    /// increase strictly, reference known angles, and alternate angle IDs.
    ///
    /// # Errors
    ///
    /// Returns [`MulticamError`] when an angle or switch invariant is invalid.
    pub fn new(
        angles: impl Into<Vec<CameraAngle>>,
        switches: impl Into<Vec<MulticamSwitch>>,
    ) -> Result<Self, MulticamError> {
        let angles = angles.into();
        let switches = switches.into();
        if angles.is_empty() {
            return Err(MulticamError::NoAngles);
        }
        let mut angle_ids = BTreeSet::new();
        let mut media_ids = BTreeSet::new();
        for angle in &angles {
            if angle.id.trim().is_empty() {
                return Err(MulticamError::EmptyAngleId);
            }
            if !angle_ids.insert(angle.id.clone()) {
                return Err(MulticamError::DuplicateAngleId(angle.id.clone()));
            }
            if !media_ids.insert(angle.media_id) {
                return Err(MulticamError::DuplicateMedia);
            }
        }
        if switches.is_empty() {
            return Err(MulticamError::NoSwitches);
        }
        if !switches[0].timeline_time.is_zero() {
            return Err(MulticamError::NonZeroFirstSwitch);
        }
        for (index, switch) in switches.iter().enumerate() {
            if !angle_ids.contains(&switch.angle_id) {
                return Err(MulticamError::UnknownAngle(switch.angle_id.clone()));
            }
            if let Some(previous) = index.checked_sub(1).map(|i| &switches[i]) {
                if previous.timeline_time >= switch.timeline_time {
                    return Err(MulticamError::NonIncreasingSwitches);
                }
                if previous.angle_id == switch.angle_id {
                    return Err(MulticamError::RedundantSwitch);
                }
            }
        }
        Ok(Self { angles, switches })
    }

    /// Returns camera angles in their caller-defined display order.
    #[must_use]
    pub fn angles(&self) -> &[CameraAngle] {
        &self.angles
    }

    /// Returns exact-time cut decisions in chronological order.
    #[must_use]
    pub fn switches(&self) -> &[MulticamSwitch] {
        &self.switches
    }

    /// Resolves the angle active at an exact shared timeline time.
    ///
    /// Cuts are left-closed: a switch takes effect exactly at its timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`MulticamError::NegativeTimelineTime`] for negative queries.
    pub fn active_angle_at(
        &self,
        timeline_time: RationalTime,
    ) -> Result<&CameraAngle, MulticamError> {
        if timeline_time.is_negative() {
            return Err(MulticamError::NegativeTimelineTime);
        }
        let switch_index = self
            .switches
            .partition_point(|switch| switch.timeline_time <= timeline_time)
            - 1;
        let angle_id = &self.switches[switch_index].angle_id;
        self.angles
            .iter()
            .find(|angle| &angle.id == angle_id)
            .ok_or_else(|| MulticamError::UnknownAngle(angle_id.clone()))
    }

    /// Resolves both the active angle and its synchronized source time.
    ///
    /// # Errors
    ///
    /// Propagates query, source-availability, and exact arithmetic errors.
    pub fn source_at(
        &self,
        timeline_time: RationalTime,
    ) -> Result<(&CameraAngle, RationalTime), MulticamError> {
        let angle = self.active_angle_at(timeline_time)?;
        let source_time = angle.source_time_at(timeline_time)?;
        Ok((angle, source_time))
    }
}

#[cfg(test)]
mod tests {
    use super::{CameraAngle, MulticamError, MulticamPlan, MulticamSwitch};
    use prism_core::MediaAssetId;
    use prism_timeline::RationalTime;

    fn angle(id: &str, offset: RationalTime) -> CameraAngle {
        CameraAngle {
            id: id.to_owned(),
            media_id: MediaAssetId::new(),
            sync_offset: offset,
        }
    }

    fn cut(time: i64, id: &str) -> Result<MulticamSwitch, MulticamError> {
        Ok(MulticamSwitch {
            timeline_time: RationalTime::new(time, 1)?,
            angle_id: id.to_owned(),
        })
    }

    #[test]
    fn resolves_switch_boundaries_and_exact_offsets() -> Result<(), MulticamError> {
        let plan = MulticamPlan::new(
            vec![
                angle("wide", RationalTime::new(1, 2)?),
                angle("close", RationalTime::new(3, 2)?),
            ],
            vec![cut(0, "wide")?, cut(5, "close")?, cut(9, "wide")?],
        )?;
        assert_eq!(
            plan.active_angle_at(RationalTime::new(4999, 1000)?)?.id,
            "wide"
        );
        let (active, source_time) = plan.source_at(RationalTime::new(5, 1)?)?;
        assert_eq!(active.id, "close");
        assert_eq!(source_time, RationalTime::new(13, 2)?);
        assert_eq!(plan.active_angle_at(RationalTime::new(9, 1)?)?.id, "wide");
        Ok(())
    }

    #[test]
    fn reports_when_an_offset_places_source_before_zero() -> Result<(), MulticamError> {
        let plan = MulticamPlan::new(
            vec![angle("late", RationalTime::new(-2, 1)?)],
            vec![cut(0, "late")?],
        )?;
        assert_eq!(
            plan.source_at(RationalTime::new(1, 1)?),
            Err(MulticamError::SourceUnavailable {
                angle_id: "late".to_owned()
            })
        );
        assert_eq!(
            plan.source_at(RationalTime::new(2, 1)?)?.1,
            RationalTime::ZERO
        );
        Ok(())
    }

    #[test]
    fn rejects_invalid_switch_plans() -> Result<(), MulticamError> {
        let wide = angle("wide", RationalTime::ZERO);
        let close = angle("close", RationalTime::ZERO);
        assert_eq!(
            MulticamPlan::new(vec![wide.clone()], Vec::new()),
            Err(MulticamError::NoSwitches)
        );
        assert_eq!(
            MulticamPlan::new(vec![wide.clone()], vec![cut(1, "wide")?]),
            Err(MulticamError::NonZeroFirstSwitch)
        );
        assert_eq!(
            MulticamPlan::new(vec![wide.clone()], vec![cut(0, "missing")?]),
            Err(MulticamError::UnknownAngle("missing".to_owned()))
        );
        assert_eq!(
            MulticamPlan::new(vec![wide.clone()], vec![cut(0, "wide")?, cut(1, "wide")?]),
            Err(MulticamError::RedundantSwitch)
        );
        assert_eq!(
            MulticamPlan::new(vec![wide, close], vec![cut(0, "wide")?, cut(0, "close")?]),
            Err(MulticamError::NonIncreasingSwitches)
        );
        Ok(())
    }
}
