//! Exact, piecewise-linear source-time mapping for clip retiming.

use crate::{RationalTime, TimeError};
use thiserror::Error;

/// One control point relating clip-local timeline time to source-media time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetimePoint {
    /// Clip-local output time. Points must begin at zero and increase strictly.
    pub timeline_time: RationalTime,
    /// Source-media time. Equal or decreasing values create freezes or reverse motion.
    pub source_time: RationalTime,
}

/// Validation and evaluation failures for a [`TimeMap`].
#[derive(Debug, Error, Eq, PartialEq)]
pub enum RetimeError {
    /// A useful map needs at least a start and end control point.
    #[error("a time map requires at least two control points")]
    TooFewPoints,
    /// Clip-local maps have a stable origin at zero.
    #[error("the first timeline control point must be zero")]
    NonZeroOrigin,
    /// Segment durations must be positive and unambiguous.
    #[error("timeline control points must be strictly increasing")]
    NonIncreasingTimeline,
    /// Media source positions cannot precede the start of the source.
    #[error("source control points must not be negative")]
    NegativeSourceTime,
    /// Evaluation was requested outside the closed control-point domain.
    #[error("timeline time is outside the time map domain")]
    OutsideDomain,
    /// Frame indexes and durations must describe a real output frame.
    #[error("frame index must be nonnegative and frame duration must be positive")]
    InvalidFrame,
    /// Exact rational arithmetic failed.
    #[error("retiming arithmetic failed: {0}")]
    Time(#[from] TimeError),
}

/// A validated, deterministic piecewise-linear clip time map.
///
/// Increasing source values play forward, equal values hold a frame, and
/// decreasing values play in reverse. Evaluation is exact at every frame;
/// consumers choose any display rounding only after mapping source time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimeMap {
    points: Vec<RetimePoint>,
}

impl TimeMap {
    /// Validates and constructs a time map.
    ///
    /// # Errors
    ///
    /// Returns [`RetimeError`] when fewer than two points are supplied, the
    /// timeline does not begin at zero and increase strictly, or a source time
    /// is negative.
    pub fn new(points: impl Into<Vec<RetimePoint>>) -> Result<Self, RetimeError> {
        let points = points.into();
        if points.len() < 2 {
            return Err(RetimeError::TooFewPoints);
        }
        if !points[0].timeline_time.is_zero() {
            return Err(RetimeError::NonZeroOrigin);
        }
        if points.iter().any(|point| point.source_time.is_negative()) {
            return Err(RetimeError::NegativeSourceTime);
        }
        if points
            .windows(2)
            .any(|pair| pair[0].timeline_time >= pair[1].timeline_time)
        {
            return Err(RetimeError::NonIncreasingTimeline);
        }
        Ok(Self { points })
    }

    /// Returns the validated control points in timeline order.
    #[must_use]
    pub fn points(&self) -> &[RetimePoint] {
        &self.points
    }

    /// Maps an exact clip-local timeline time to exact source-media time.
    ///
    /// # Errors
    ///
    /// Returns [`RetimeError::OutsideDomain`] outside the closed map domain,
    /// or [`RetimeError::Time`] if the exact interpolation overflows.
    pub fn map_time(&self, timeline_time: RationalTime) -> Result<RationalTime, RetimeError> {
        let first = self.points[0];
        let last = self.points[self.points.len() - 1];
        if timeline_time < first.timeline_time || timeline_time > last.timeline_time {
            return Err(RetimeError::OutsideDomain);
        }
        match self
            .points
            .binary_search_by_key(&timeline_time, |point| point.timeline_time)
        {
            Ok(index) => Ok(self.points[index].source_time),
            Err(right_index) => {
                let left = self.points[right_index - 1];
                let right = self.points[right_index];
                let elapsed = timeline_time.checked_sub(left.timeline_time)?;
                let segment_duration = right.timeline_time.checked_sub(left.timeline_time)?;
                let source_delta = right.source_time.checked_sub(left.source_time)?;
                Ok(left
                    .source_time
                    .checked_add(source_delta.checked_mul_ratio(elapsed, segment_duration)?)?)
            }
        }
    }

    /// Maps a zero-based output frame index using an exact frame duration.
    ///
    /// # Errors
    ///
    /// Returns [`RetimeError::InvalidFrame`] for a negative index or
    /// nonpositive duration; mapping and arithmetic errors are propagated.
    pub fn map_frame(
        &self,
        frame_index: i64,
        frame_duration: RationalTime,
    ) -> Result<RationalTime, RetimeError> {
        if frame_index < 0 || !frame_duration.is_positive() {
            return Err(RetimeError::InvalidFrame);
        }
        self.map_time(frame_duration.checked_mul_i64(frame_index)?)
    }
}

#[cfg(test)]
mod tests {
    use super::{RetimeError, RetimePoint, TimeMap};
    use crate::RationalTime;

    fn point(timeline: i64, source: i64) -> Result<RetimePoint, RetimeError> {
        Ok(RetimePoint {
            timeline_time: RationalTime::new(timeline, 1)?,
            source_time: RationalTime::new(source, 1)?,
        })
    }

    #[test]
    fn maps_constant_and_variable_speed_exactly() -> Result<(), RetimeError> {
        let map = TimeMap::new(vec![point(0, 0)?, point(2, 4)?, point(6, 6)?])?;
        assert_eq!(
            map.map_time(RationalTime::new(1, 1)?)?,
            RationalTime::new(2, 1)?
        );
        assert_eq!(
            map.map_time(RationalTime::new(4, 1)?)?,
            RationalTime::new(5, 1)?
        );
        Ok(())
    }

    #[test]
    fn supports_freeze_and_reverse_segments() -> Result<(), RetimeError> {
        let map = TimeMap::new(vec![point(0, 5)?, point(2, 5)?, point(4, 3)?])?;
        assert_eq!(
            map.map_time(RationalTime::new(1, 1)?)?,
            RationalTime::new(5, 1)?
        );
        assert_eq!(
            map.map_time(RationalTime::new(3, 1)?)?,
            RationalTime::new(4, 1)?
        );
        Ok(())
    }

    #[test]
    fn maps_fractional_frame_rates_without_drift() -> Result<(), RetimeError> {
        let map = TimeMap::new(vec![
            RetimePoint {
                timeline_time: RationalTime::ZERO,
                source_time: RationalTime::ZERO,
            },
            RetimePoint {
                timeline_time: RationalTime::new(1001, 1000)?,
                source_time: RationalTime::new(1001, 500)?,
            },
        ])?;
        let frame = RationalTime::new(1001, 30_000)?;
        assert_eq!(
            map.map_frame(17, frame)?,
            RationalTime::new(17_017, 15_000)?
        );
        assert_eq!(map.map_frame(30, frame)?, RationalTime::new(1001, 500)?);
        Ok(())
    }

    #[test]
    fn rejects_ambiguous_maps_and_invalid_frames() -> Result<(), RetimeError> {
        assert_eq!(
            TimeMap::new(vec![point(0, 0)?]),
            Err(RetimeError::TooFewPoints)
        );
        assert_eq!(
            TimeMap::new(vec![point(1, 0)?, point(2, 1)?]),
            Err(RetimeError::NonZeroOrigin)
        );
        assert_eq!(
            TimeMap::new(vec![point(0, 0)?, point(0, 1)?]),
            Err(RetimeError::NonIncreasingTimeline)
        );
        let map = TimeMap::new(vec![point(0, 0)?, point(1, 1)?])?;
        assert_eq!(
            map.map_frame(-1, RationalTime::new(1, 30)?),
            Err(RetimeError::InvalidFrame)
        );
        assert_eq!(
            map.map_time(RationalTime::new(2, 1)?),
            Err(RetimeError::OutsideDomain)
        );
        Ok(())
    }
}
