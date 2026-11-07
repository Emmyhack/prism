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

/// Validation failures for deterministic point tracking.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum TrackingError {
    /// Frame dimensions are zero or do not match the pixel buffer.
    #[error("tracking frame dimensions do not match its pixels")]
    InvalidFrame,
    /// Reference and current frames must have identical dimensions.
    #[error("tracking frames must have identical dimensions")]
    FrameSizeMismatch,
    /// The patch or search radius is unsupported.
    #[error("tracking patch and search radii must be valid")]
    InvalidRadius,
    /// The requested reference patch extends outside the frame.
    #[error("tracking point patch extends outside the frame")]
    PatchOutsideFrame,
}

/// Borrowed 8-bit grayscale frame used by the CPU point tracker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrayFrame<'a> {
    /// Frame width in pixels.
    pub width: usize,
    /// Frame height in pixels.
    pub height: usize,
    /// Row-major luma pixels with no row padding.
    pub pixels: &'a [u8],
}

impl<'a> GrayFrame<'a> {
    /// Creates a frame after validating dimensions and storage length.
    ///
    /// # Errors
    ///
    /// Returns [`TrackingError::InvalidFrame`] for zero or overflowing
    /// dimensions, or a pixel-count mismatch.
    pub fn new(width: usize, height: usize, pixels: &'a [u8]) -> Result<Self, TrackingError> {
        if width == 0
            || height == 0
            || width
                .checked_mul(height)
                .is_none_or(|count| count != pixels.len())
        {
            return Err(TrackingError::InvalidFrame);
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    fn pixel(self, x: usize, y: usize) -> u8 {
        self.pixels[y * self.width + x]
    }
}

/// One deterministic point-tracking result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointTrackMatch {
    /// Matched horizontal pixel coordinate in the current frame.
    pub x: usize,
    /// Matched vertical pixel coordinate in the current frame.
    pub y: usize,
    /// Mean absolute luma error for the winning patch, in `0..=255`.
    pub mean_absolute_error: f32,
    /// Normalized patch similarity, where one is an exact match.
    pub confidence: f32,
    /// Euclidean forward/backward consistency error in pixels.
    pub forward_backward_error: f32,
}

/// Tracks a point between equal-sized luma frames using bounded block matching.
///
/// Candidate patches are scored by sum of absolute differences. Equal scores
/// resolve deterministically by shortest displacement and then row-major
/// coordinate. A reverse match supplies a forward/backward consistency error,
/// allowing callers to reject occlusions or ambiguous matches. This CPU
/// primitive is suitable for offline tracking and reference results; it does
/// not claim subpixel or optical-flow precision.
///
/// # Errors
///
/// Returns [`TrackingError`] for malformed frames, unsupported radii, or a
/// reference patch that crosses an image boundary.
pub fn track_point(
    reference: GrayFrame<'_>,
    current: GrayFrame<'_>,
    x: usize,
    y: usize,
    patch_radius: usize,
    search_radius: usize,
) -> Result<PointTrackMatch, TrackingError> {
    validate_tracking(reference, current, x, y, patch_radius, search_radius)?;
    let (matched_x, matched_y, error) =
        best_match(reference, current, x, y, patch_radius, search_radius);
    let (returned_x, returned_y, _) = best_match(
        current,
        reference,
        matched_x,
        matched_y,
        patch_radius,
        search_radius,
    );
    let side = patch_radius * 2 + 1;
    let sample_count = side * side;
    #[allow(clippy::cast_precision_loss)]
    let mean_absolute_error = error as f32 / sample_count as f32;
    #[allow(clippy::cast_precision_loss)]
    let consistency_x = returned_x.abs_diff(x) as f32;
    #[allow(clippy::cast_precision_loss)]
    let consistency_y = returned_y.abs_diff(y) as f32;
    Ok(PointTrackMatch {
        x: matched_x,
        y: matched_y,
        mean_absolute_error,
        confidence: 1.0 - mean_absolute_error / 255.0,
        forward_backward_error: consistency_x.hypot(consistency_y),
    })
}

fn validate_tracking(
    reference: GrayFrame<'_>,
    current: GrayFrame<'_>,
    x: usize,
    y: usize,
    patch_radius: usize,
    search_radius: usize,
) -> Result<(), TrackingError> {
    GrayFrame::new(reference.width, reference.height, reference.pixels)?;
    GrayFrame::new(current.width, current.height, current.pixels)?;
    if reference.width != current.width || reference.height != current.height {
        return Err(TrackingError::FrameSizeMismatch);
    }
    if patch_radius == 0 || patch_radius > 128 || search_radius > 512 {
        return Err(TrackingError::InvalidRadius);
    }
    if x < patch_radius
        || y < patch_radius
        || x.checked_add(patch_radius)
            .is_none_or(|right| right >= reference.width)
        || y.checked_add(patch_radius)
            .is_none_or(|bottom| bottom >= reference.height)
    {
        return Err(TrackingError::PatchOutsideFrame);
    }
    Ok(())
}

fn best_match(
    reference: GrayFrame<'_>,
    current: GrayFrame<'_>,
    x: usize,
    y: usize,
    patch_radius: usize,
    search_radius: usize,
) -> (usize, usize, u64) {
    let minimum_x = x.saturating_sub(search_radius).max(patch_radius);
    let maximum_x = x
        .saturating_add(search_radius)
        .min(current.width - patch_radius - 1);
    let minimum_y = y.saturating_sub(search_radius).max(patch_radius);
    let maximum_y = y
        .saturating_add(search_radius)
        .min(current.height - patch_radius - 1);
    let mut best = (x, y, u64::MAX, usize::MAX);
    for candidate_y in minimum_y..=maximum_y {
        for candidate_x in minimum_x..=maximum_x {
            let mut error = 0_u64;
            for offset_y in 0..=patch_radius * 2 {
                for offset_x in 0..=patch_radius * 2 {
                    let reference_x = x - patch_radius + offset_x;
                    let reference_y = y - patch_radius + offset_y;
                    let current_x = candidate_x - patch_radius + offset_x;
                    let current_y = candidate_y - patch_radius + offset_y;
                    error += u64::from(
                        reference
                            .pixel(reference_x, reference_y)
                            .abs_diff(current.pixel(current_x, current_y)),
                    );
                }
            }
            let distance = candidate_x.abs_diff(x).pow(2) + candidate_y.abs_diff(y).pow(2);
            if (error, distance, candidate_y, candidate_x) < (best.2, best.3, best.1, best.0) {
                best = (candidate_x, candidate_y, error, distance);
            }
        }
    }
    (best.0, best.1, best.2)
}

#[cfg(test)]
mod tests {
    use super::{AutomationCurve, CurveError, GrayFrame, Keyframe, TrackingError, track_point};
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

    #[test]
    fn point_tracker_recovers_integer_translation_with_consistency() -> Result<(), TrackingError> {
        let mut reference = vec![0_u8; 11 * 11];
        let mut current = vec![0_u8; 11 * 11];
        let pattern = [[10, 40, 90], [30, 220, 70], [15, 110, 180]];
        for (row, values) in pattern.iter().enumerate() {
            for (column, &value) in values.iter().enumerate() {
                reference[(4 + row) * 11 + 3 + column] = value;
                current[(3 + row) * 11 + 5 + column] = value;
            }
        }
        let matched = track_point(
            GrayFrame::new(11, 11, &reference)?,
            GrayFrame::new(11, 11, &current)?,
            4,
            5,
            1,
            3,
        )?;
        assert_eq!((matched.x, matched.y), (6, 4));
        assert!(matched.mean_absolute_error.abs() < f32::EPSILON);
        assert!((matched.confidence - 1.0).abs() < f32::EPSILON);
        assert!(matched.forward_backward_error.abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn point_tracker_ties_prefer_nearest_then_row_major() -> Result<(), TrackingError> {
        let pixels = vec![42_u8; 7 * 7];
        let frame = GrayFrame::new(7, 7, &pixels)?;
        let matched = track_point(frame, frame, 3, 3, 1, 2)?;
        assert_eq!((matched.x, matched.y), (3, 3));
        assert!((matched.confidence - 1.0).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn point_tracker_validates_frames_patches_and_radii() -> Result<(), TrackingError> {
        assert_eq!(
            GrayFrame::new(2, 2, &[0; 3]),
            Err(TrackingError::InvalidFrame)
        );
        let small_pixels = [0; 16];
        let large_pixels = [0; 25];
        let small = GrayFrame::new(4, 4, &small_pixels)?;
        let large = GrayFrame::new(5, 5, &large_pixels)?;
        assert_eq!(
            track_point(small, large, 2, 2, 1, 1),
            Err(TrackingError::FrameSizeMismatch)
        );
        assert_eq!(
            track_point(small, small, 2, 2, 0, 1),
            Err(TrackingError::InvalidRadius)
        );
        assert_eq!(
            track_point(small, small, 0, 0, 1, 1),
            Err(TrackingError::PatchOutsideFrame)
        );
        Ok(())
    }
}
