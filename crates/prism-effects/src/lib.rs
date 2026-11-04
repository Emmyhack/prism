//! Deterministic keyframe and automation primitives.
//!
//! Curves are independent of UI and render backends so preview, export, and
//! future scripting commands can evaluate the same values.

use prism_timeline::{RationalTime, TimeError};
use thiserror::Error;

/// One linear keyframe for a scalar effect parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Keyframe {
    /// Exact sequence time of the keyframe.
    pub time: RationalTime,
    /// Parameter value at that time.
    pub value: f32,
}

/// Automation curve validation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CurveError {
    /// The keyframe time is before sequence zero.
    #[error("keyframe time must not be negative")]
    NegativeTime,
    /// A keyframe value is NaN or infinite.
    #[error("keyframe value must be finite")]
    NonFiniteValue,
    /// A time already has a keyframe.
    #[error("a keyframe already exists at this time")]
    DuplicateTime,
    /// Exact timeline construction failed.
    #[error("invalid keyframe time: {0}")]
    Time(#[from] TimeError),
}

/// Ordered scalar automation curve with linear interpolation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutomationCurve {
    keyframes: Vec<Keyframe>,
}

impl AutomationCurve {
    /// Creates an empty curve.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a curve by inserting keyframes in any order.
    ///
    /// # Errors
    ///
    /// Returns a [`CurveError`] for invalid values, negative times, or
    /// duplicate times.
    pub fn from_keyframes(
        keyframes: impl IntoIterator<Item = Keyframe>,
    ) -> Result<Self, CurveError> {
        let mut curve = Self::new();
        for keyframe in keyframes {
            curve.insert(keyframe)?;
        }
        Ok(curve)
    }

    /// Inserts one keyframe while preserving exact-time ordering.
    ///
    /// # Errors
    ///
    /// Returns a [`CurveError`] for invalid values, negative times, or an
    /// existing keyframe at the same exact time.
    pub fn insert(&mut self, keyframe: Keyframe) -> Result<(), CurveError> {
        if !keyframe.time.is_non_negative() {
            return Err(CurveError::NegativeTime);
        }
        if !keyframe.value.is_finite() {
            return Err(CurveError::NonFiniteValue);
        }
        let position = self
            .keyframes
            .binary_search_by_key(&keyframe.time, |existing| existing.time);
        match position {
            Ok(_) => Err(CurveError::DuplicateTime),
            Err(index) => {
                self.keyframes.insert(index, keyframe);
                Ok(())
            }
        }
    }

    /// Returns keyframes in exact-time order.
    #[must_use]
    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    /// Evaluates the curve with endpoint clamping and linear interpolation.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn evaluate(&self, time: RationalTime) -> Option<f32> {
        let first = self.keyframes.first()?;
        let last = self.keyframes.last()?;
        if time <= first.time {
            return Some(first.value);
        }
        if time >= last.time {
            return Some(last.value);
        }
        let right_index = self
            .keyframes
            .partition_point(|keyframe| keyframe.time < time);
        let left = self.keyframes[right_index - 1];
        let right = self.keyframes[right_index];
        let elapsed = seconds_between(left.time, time);
        let span = seconds_between(left.time, right.time);
        let fraction = elapsed / span;
        Some(left.value + (right.value - left.value) * fraction)
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn seconds_between(start: RationalTime, end: RationalTime) -> f32 {
    let start_seconds = start.value() as f64 / f64::from(start.timescale());
    let end_seconds = end.value() as f64 / f64::from(end.timescale());
    (end_seconds - start_seconds) as f32
}

#[cfg(test)]
mod tests {
    use super::{AutomationCurve, CurveError, Keyframe};
    use prism_timeline::RationalTime;

    fn keyframe(value: i64, parameter: f32) -> Result<Keyframe, CurveError> {
        Ok(Keyframe {
            time: RationalTime::new(value, 1)?,
            value: parameter,
        })
    }

    #[test]
    fn inserts_out_of_order_and_interpolates_exact_times() -> Result<(), CurveError> {
        let mut curve = AutomationCurve::new();
        curve.insert(keyframe(2, 1.0)?)?;
        curve.insert(keyframe(0, 0.0)?)?;
        curve.insert(Keyframe {
            time: RationalTime::new(3, 2)?,
            value: 0.5,
        })?;
        assert_eq!(curve.keyframes()[0].time, RationalTime::ZERO);
        assert!((curve.evaluate(RationalTime::new(3, 4)?).unwrap_or_default() - 0.25).abs() < 1e-6);
        assert!((curve.evaluate(RationalTime::new(7, 4)?).unwrap_or_default() - 0.75).abs() < 1e-6);
        assert!((curve.evaluate(RationalTime::new(5, 1)?).unwrap_or_default() - 1.0).abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn rejects_duplicate_negative_and_non_finite_keyframes() -> Result<(), CurveError> {
        let mut curve = AutomationCurve::new();
        curve.insert(keyframe(1, 0.5)?)?;
        assert_eq!(
            curve.insert(keyframe(1, 0.7)?),
            Err(CurveError::DuplicateTime)
        );
        assert_eq!(
            curve.insert(Keyframe {
                time: RationalTime::new(-1, 1)?,
                value: 0.0,
            }),
            Err(CurveError::NegativeTime)
        );
        assert_eq!(
            curve.insert(Keyframe {
                time: RationalTime::new(2, 1)?,
                value: f32::NAN,
            }),
            Err(CurveError::NonFiniteValue)
        );
        Ok(())
    }
}
