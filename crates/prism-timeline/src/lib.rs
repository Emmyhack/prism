//! Exact timeline time types and arithmetic policies.

use std::cmp::Ordering;
use thiserror::Error;

/// Errors produced by exact time arithmetic.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum TimeError {
    /// A timebase denominator must be positive.
    #[error("timebase must be greater than zero")]
    InvalidTimebase,
    /// The exact result cannot fit in the representation.
    #[error("time arithmetic overflow")]
    Overflow,
    /// Division by zero was requested.
    #[error("cannot divide time by zero")]
    DivisionByZero,
}

/// Exact rational time represented as `value / timescale` seconds.
///
/// Values are normalized, and `timescale` is always positive. Operations are
/// checked and never silently round. Conversions requiring rounding must name
/// a [`RoundingMode`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RationalTime {
    value: i64,
    timescale: u32,
}

impl PartialOrd for RationalTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RationalTime {
    fn cmp(&self, other: &Self) -> Ordering {
        (i128::from(self.value) * i128::from(other.timescale))
            .cmp(&(i128::from(other.value) * i128::from(self.timescale)))
    }
}

/// Explicit policy for conversion to an integer timebase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoundingMode {
    /// Round toward negative infinity.
    Floor,
    /// Round toward positive infinity.
    Ceil,
    /// Round to nearest; exact halves round away from zero.
    NearestTiesAway,
}

impl RationalTime {
    /// Zero seconds using a unit timebase.
    pub const ZERO: Self = Self {
        value: 0,
        timescale: 1,
    };

    /// Creates and normalizes exact time.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::InvalidTimebase`] for a zero denominator, or
    /// [`TimeError::Overflow`] if normalization cannot fit the representation.
    pub fn new(value: i64, timescale: u32) -> Result<Self, TimeError> {
        if timescale == 0 {
            return Err(TimeError::InvalidTimebase);
        }
        if value == 0 {
            return Ok(Self::ZERO);
        }
        let divisor = gcd(value.unsigned_abs(), u64::from(timescale));
        let reduced_value = i128::from(value) / i128::from(divisor);
        let reduced_scale = u64::from(timescale) / divisor;
        Ok(Self {
            value: i64::try_from(reduced_value).map_err(|_| TimeError::Overflow)?,
            timescale: u32::try_from(reduced_scale).map_err(|_| TimeError::Overflow)?,
        })
    }

    /// The signed numerator.
    #[must_use]
    pub const fn value(self) -> i64 {
        self.value
    }

    /// The positive denominator in ticks per second.
    #[must_use]
    pub const fn timescale(self) -> u32 {
        self.timescale
    }

    /// Returns whether this time is zero or later.
    #[must_use]
    pub const fn is_non_negative(self) -> bool {
        self.value >= 0
    }

    /// Returns whether this duration is strictly positive.
    #[must_use]
    pub const fn is_positive(self) -> bool {
        self.value > 0
    }

    /// Adds two exact times with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::Overflow`] if the exact result is not representable.
    pub fn checked_add(self, other: Self) -> Result<Self, TimeError> {
        let numerator = i128::from(self.value) * i128::from(other.timescale)
            + i128::from(other.value) * i128::from(self.timescale);
        let denominator = u64::from(self.timescale) * u64::from(other.timescale);
        normalize_wide(numerator, denominator)
    }

    /// Subtracts two exact times with checked arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::Overflow`] if the exact result is not representable.
    pub fn checked_sub(self, other: Self) -> Result<Self, TimeError> {
        let numerator = i128::from(self.value) * i128::from(other.timescale)
            - i128::from(other.value) * i128::from(self.timescale);
        let denominator = u64::from(self.timescale) * u64::from(other.timescale);
        normalize_wide(numerator, denominator)
    }

    /// Converts this time to integer ticks at `target_timescale` using an
    /// explicit rounding policy.
    ///
    /// # Errors
    ///
    /// Returns [`TimeError::InvalidTimebase`] for a zero target timescale or
    /// [`TimeError::Overflow`] when the rounded tick count cannot fit in `i64`.
    pub fn to_ticks(self, target_timescale: u32, rounding: RoundingMode) -> Result<i64, TimeError> {
        if target_timescale == 0 {
            return Err(TimeError::InvalidTimebase);
        }
        let numerator = i128::from(self.value) * i128::from(target_timescale);
        let denominator = i128::from(self.timescale);
        let quotient = numerator / denominator;
        let remainder = numerator % denominator;
        let rounded = match rounding {
            RoundingMode::Floor if remainder < 0 => quotient - 1,
            RoundingMode::Ceil if remainder > 0 => quotient + 1,
            RoundingMode::NearestTiesAway
                if remainder != 0 && remainder.abs() * 2 >= denominator =>
            {
                quotient + numerator.signum()
            }
            _ => quotient,
        };
        i64::try_from(rounded).map_err(|_| TimeError::Overflow)
    }
}

fn normalize_wide(value: i128, timescale: u64) -> Result<RationalTime, TimeError> {
    let magnitude = u128::try_from(value.abs()).map_err(|_| TimeError::Overflow)?;
    let divisor = gcd_u128(magnitude, u128::from(timescale));
    let value = value / i128::try_from(divisor).map_err(|_| TimeError::Overflow)?;
    let timescale = u128::from(timescale) / divisor;
    RationalTime::new(
        i64::try_from(value).map_err(|_| TimeError::Overflow)?,
        u32::try_from(timescale).map_err(|_| TimeError::Overflow)?,
    )
}

const fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

const fn gcd_u128(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::{RationalTime, RoundingMode, TimeError};
    use proptest::prelude::*;

    #[test]
    fn normalizes_fractional_frame_time() -> Result<(), TimeError> {
        let time = RationalTime::new(2_000, 60_000)?;
        assert_eq!(time, RationalTime::new(1, 30)?);
        Ok(())
    }

    #[test]
    fn adds_different_timebases_without_drift() -> Result<(), TimeError> {
        let frame = RationalTime::new(1_001, 30_000)?;
        let sample = RationalTime::new(1, 48_000)?;
        assert_eq!(
            frame.checked_add(sample)?,
            RationalTime::new(8_013, 240_000)?
        );
        Ok(())
    }

    #[test]
    fn rejects_zero_timebase() {
        assert_eq!(RationalTime::new(1, 0), Err(TimeError::InvalidTimebase));
    }

    #[test]
    fn integer_tick_conversion_uses_explicit_rounding() -> Result<(), TimeError> {
        let positive = RationalTime::new(3, 2)?;
        assert_eq!(positive.to_ticks(1, RoundingMode::Floor)?, 1);
        assert_eq!(positive.to_ticks(1, RoundingMode::Ceil)?, 2);
        assert_eq!(positive.to_ticks(1, RoundingMode::NearestTiesAway)?, 2);
        let negative = RationalTime::new(-3, 2)?;
        assert_eq!(negative.to_ticks(1, RoundingMode::Floor)?, -2);
        assert_eq!(negative.to_ticks(1, RoundingMode::Ceil)?, -1);
        assert_eq!(negative.to_ticks(1, RoundingMode::NearestTiesAway)?, -2);
        Ok(())
    }

    proptest! {
        #[test]
        fn exact_addition_is_commutative_and_reversible(
            left_value in -1_000_000_i64..1_000_000,
            left_scale in 1_u32..1_000,
            right_value in -1_000_000_i64..1_000_000,
            right_scale in 1_u32..1_000,
        ) {
            let failure = |error: TimeError| TestCaseError::fail(error.to_string());
            let left = RationalTime::new(left_value, left_scale).map_err(&failure)?;
            let right = RationalTime::new(right_value, right_scale).map_err(&failure)?;
            let sum = left.checked_add(right).map_err(&failure)?;
            prop_assert_eq!(sum, right.checked_add(left).map_err(&failure)?);
            prop_assert_eq!(sum.checked_sub(right).map_err(&failure)?, left);
        }
    }
}
