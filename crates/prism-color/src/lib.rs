//! Validated SDR transfer-function primitives.
//!
//! These functions operate on normalized, non-negative channel values. They
//! are building blocks for a future color-managed render graph; they do not
//! implement primaries conversion, HDR metadata, LUTs, or display transforms.

use thiserror::Error;

/// Transfer functions supported by the initial color foundation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferFunction {
    /// IEC 61966-2-1 sRGB OETF/EOTF pair.
    Srgb,
    /// ITU-R BT.709 SDR transfer curve.
    Rec709,
}

/// A normalized RGB triplet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    /// Red channel in the normalized range `[0, 1]`.
    pub red: f32,
    /// Green channel in the normalized range `[0, 1]`.
    pub green: f32,
    /// Blue channel in the normalized range `[0, 1]`.
    pub blue: f32,
}

/// Color value validation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ColorError {
    /// A channel was NaN or infinite.
    #[error("RGB channels must be finite")]
    NonFinite,
    /// A channel was outside the normalized SDR range.
    #[error("RGB channels must be between 0.0 and 1.0")]
    OutOfRange,
    /// A grading control was outside the supported SDR adjustment range.
    #[error("color adjustment parameter is outside its supported range")]
    InvalidAdjustment,
    /// A histogram bin count overflowed.
    #[error("histogram bin count overflowed")]
    HistogramOverflow,
    /// A waveform scope width was zero or too large to represent.
    #[error("waveform scope width is invalid")]
    InvalidScopeWidth,
    /// Pixel count was not an integer number of rows for the scope width.
    #[error("pixel count does not match waveform scope width")]
    InvalidScopePixelCount,
    /// A LUT had too few samples or contained an invalid value.
    #[error("LUT must contain at least two normalized finite samples")]
    InvalidLut,
}

/// Number of bins in the normalized RGB histogram scope.
pub const HISTOGRAM_BINS: usize = 256;

/// Number of bins per axis in the normalized vectorscope.
pub const VECTORSCOPE_BINS: usize = 256;

/// Exact integer RGB histogram for a validated pixel sample set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbHistogram {
    /// Red-channel sample counts, indexed from 0 through 255.
    pub red: [u64; HISTOGRAM_BINS],
    /// Green-channel sample counts, indexed from 0 through 255.
    pub green: [u64; HISTOGRAM_BINS],
    /// Blue-channel sample counts, indexed from 0 through 255.
    pub blue: [u64; HISTOGRAM_BINS],
}

impl RgbHistogram {
    /// Creates an empty histogram.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            red: [0; HISTOGRAM_BINS],
            green: [0; HISTOGRAM_BINS],
            blue: [0; HISTOGRAM_BINS],
        }
    }

    /// Accumulates one validated normalized RGB sample without allocation.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when the sample is invalid or a count would
    /// overflow its integer representation.
    pub fn accumulate(&mut self, sample: Rgb) -> Result<(), ColorError> {
        sample.validate()?;
        let channels = [
            (&mut self.red, histogram_bin(sample.red)),
            (&mut self.green, histogram_bin(sample.green)),
            (&mut self.blue, histogram_bin(sample.blue)),
        ];
        for (channel, bin) in channels {
            channel[bin] = channel[bin]
                .checked_add(1)
                .ok_or(ColorError::HistogramOverflow)?;
        }
        Ok(())
    }

    /// Builds a histogram from validated normalized pixels.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when any sample is invalid or counts overflow.
    pub fn from_pixels(pixels: &[Rgb]) -> Result<Self, ColorError> {
        let mut histogram = Self::new();
        for &pixel in pixels {
            histogram.accumulate(pixel)?;
        }
        Ok(histogram)
    }

    /// Returns per-channel sample percentages in the range `0.0..=1.0`.
    #[must_use]
    pub fn percentages(
        &self,
    ) -> (
        [f32; HISTOGRAM_BINS],
        [f32; HISTOGRAM_BINS],
        [f32; HISTOGRAM_BINS],
    ) {
        (
            normalize_bins(&self.red),
            normalize_bins(&self.green),
            normalize_bins(&self.blue),
        )
    }

    /// Returns the number of accumulated samples represented by each channel.
    #[must_use]
    pub fn sample_count(&self) -> u64 {
        self.red.iter().copied().sum()
    }
}

impl Default for RgbHistogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact Rec.709 luma histogram for validated normalized RGB samples.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LumaHistogram {
    /// Luma sample counts, indexed from 0 through 255.
    pub bins: [u64; HISTOGRAM_BINS],
}

impl LumaHistogram {
    /// Creates an empty luma histogram.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bins: [0; HISTOGRAM_BINS],
        }
    }

    /// Accumulates one validated RGB sample using Rec.709 luma coefficients.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when the sample is invalid or a count would
    /// overflow its integer representation.
    pub fn accumulate(&mut self, sample: Rgb) -> Result<(), ColorError> {
        sample.validate()?;
        let luma = 0.2126 * sample.red + 0.7152 * sample.green + 0.0722 * sample.blue;
        let bin = histogram_bin(luma);
        self.bins[bin] = self.bins[bin]
            .checked_add(1)
            .ok_or(ColorError::HistogramOverflow)?;
        Ok(())
    }

    /// Builds a luma histogram from validated normalized RGB pixels.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when any sample is invalid or a count overflows.
    pub fn from_pixels(pixels: &[Rgb]) -> Result<Self, ColorError> {
        let mut histogram = Self::new();
        for &pixel in pixels {
            histogram.accumulate(pixel)?;
        }
        Ok(histogram)
    }

    /// Returns luma sample percentages in the range `0.0..=1.0`.
    #[must_use]
    pub fn percentages(&self) -> [f32; HISTOGRAM_BINS] {
        normalize_bins(&self.bins)
    }

    /// Returns the number of accumulated luma samples.
    #[must_use]
    pub fn sample_count(&self) -> u64 {
        self.bins.iter().copied().sum()
    }
}

#[allow(clippy::cast_precision_loss)]
fn normalize_bins(bins: &[u64; HISTOGRAM_BINS]) -> [f32; HISTOGRAM_BINS] {
    let total = bins.iter().copied().sum::<u64>();
    if total == 0 {
        return [0.0; HISTOGRAM_BINS];
    }
    let denominator = total as f32;
    std::array::from_fn(|index| bins[index] as f32 / denominator)
}

impl Default for LumaHistogram {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-column Rec.709 luma waveform scope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaveformScope {
    /// Number of horizontal columns represented by the scope.
    pub width: usize,
    /// Column-major bins indexed as `column * HISTOGRAM_BINS + luma_bin`.
    pub bins: Box<[u64]>,
}

impl WaveformScope {
    /// Creates an empty scope with `width` columns.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::InvalidScopeWidth`] for zero or overflowing
    /// widths.
    pub fn new(width: usize) -> Result<Self, ColorError> {
        if width == 0 {
            return Err(ColorError::InvalidScopeWidth);
        }
        let bin_count = width
            .checked_mul(HISTOGRAM_BINS)
            .ok_or(ColorError::InvalidScopeWidth)?;
        Ok(Self {
            width,
            bins: vec![0; bin_count].into_boxed_slice(),
        })
    }

    /// Accumulates one pixel at a horizontal column.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] for an invalid column, sample, or counter.
    pub fn accumulate(&mut self, column: usize, sample: Rgb) -> Result<(), ColorError> {
        if column >= self.width {
            return Err(ColorError::InvalidScopePixelCount);
        }
        sample.validate()?;
        let luma = 0.2126 * sample.red + 0.7152 * sample.green + 0.0722 * sample.blue;
        let index = column * HISTOGRAM_BINS + histogram_bin(luma);
        self.bins[index] = self.bins[index]
            .checked_add(1)
            .ok_or(ColorError::HistogramOverflow)?;
        Ok(())
    }

    /// Builds a scope from row-major pixels with a known frame width.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when the width or pixel count is invalid, or a
    /// sample/counter is invalid.
    pub fn from_pixels(width: usize, pixels: &[Rgb]) -> Result<Self, ColorError> {
        if width == 0 || !pixels.len().is_multiple_of(width) {
            return Err(if width == 0 {
                ColorError::InvalidScopeWidth
            } else {
                ColorError::InvalidScopePixelCount
            });
        }
        let mut scope = Self::new(width)?;
        for row in pixels.chunks_exact(width) {
            for (column, &sample) in row.iter().enumerate() {
                scope.accumulate(column, sample)?;
            }
        }
        Ok(scope)
    }

    /// Returns the total number of samples accumulated across all columns.
    #[must_use]
    pub fn sample_count(&self) -> u64 {
        self.bins.iter().copied().sum()
    }

    /// Returns the 256 luma bins for one column, or `None` if out of range.
    #[must_use]
    pub fn column_bins(&self, column: usize) -> Option<&[u64]> {
        let start = column.checked_mul(HISTOGRAM_BINS)?;
        self.bins.get(start..start + HISTOGRAM_BINS)
    }

    /// Returns the highest populated luma bin for one column.
    #[must_use]
    pub fn column_peak(&self, column: usize) -> Option<usize> {
        self.column_bins(column)?
            .iter()
            .rposition(|&count| count > 0)
    }
}

/// Deterministic U/V chroma vectorscope for validated SDR RGB samples.
///
/// Coordinates use a BT.601-style luma estimate. The flattened array is
/// indexed as `v * VECTORSCOPE_BINS + u` for direct display/downsampling.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vectorscope {
    /// Chroma sample counts in row-major V/U order.
    pub bins: Box<[u64]>,
}

impl Vectorscope {
    /// Creates an empty vectorscope.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bins: vec![0; VECTORSCOPE_BINS * VECTORSCOPE_BINS].into_boxed_slice(),
        }
    }

    /// Accumulates one validated RGB sample into its chroma coordinate.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] for invalid RGB values or counter overflow.
    pub fn accumulate(&mut self, sample: Rgb) -> Result<(), ColorError> {
        sample.validate()?;
        let luma = 0.299 * sample.red + 0.587 * sample.green + 0.114 * sample.blue;
        let u = ((sample.blue - luma) + 0.5).clamp(0.0, 1.0);
        let v = ((sample.red - luma) + 0.5).clamp(0.0, 1.0);
        let index = scope_bin(v) * VECTORSCOPE_BINS + scope_bin(u);
        self.bins[index] = self.bins[index]
            .checked_add(1)
            .ok_or(ColorError::HistogramOverflow)?;
        Ok(())
    }

    /// Builds a vectorscope from validated normalized RGB pixels.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when any pixel is invalid or a counter overflows.
    pub fn from_pixels(pixels: &[Rgb]) -> Result<Self, ColorError> {
        let mut scope = Self::new();
        for &pixel in pixels {
            scope.accumulate(pixel)?;
        }
        Ok(scope)
    }

    /// Returns the total number of samples accumulated in the scope.
    #[must_use]
    pub fn sample_count(&self) -> u64 {
        self.bins.iter().copied().sum()
    }

    /// Returns the count for one chroma coordinate, or `None` out of range.
    #[must_use]
    pub fn bin_count(&self, u: usize, v: usize) -> Option<u64> {
        if u >= VECTORSCOPE_BINS || v >= VECTORSCOPE_BINS {
            return None;
        }
        Some(self.bins[u * VECTORSCOPE_BINS + v])
    }

    /// Returns the most populated chroma bin and its count.
    #[must_use]
    pub fn peak_bin(&self) -> Option<(usize, usize, u64)> {
        self.bins
            .iter()
            .copied()
            .enumerate()
            .max_by_key(|&(_, count)| count)
            .and_then(|(index, count)| {
                (count > 0).then_some((index % VECTORSCOPE_BINS, index / VECTORSCOPE_BINS, count))
            })
    }
}

impl Default for Vectorscope {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn histogram_bin(channel: f32) -> usize {
    ((channel * HISTOGRAM_BINS as f32).floor() as usize).min(HISTOGRAM_BINS - 1)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn scope_bin(channel: f32) -> usize {
    ((channel * VECTORSCOPE_BINS as f32).floor() as usize).min(VECTORSCOPE_BINS - 1)
}

/// One-dimensional normalized lookup table with linear interpolation.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut1d {
    /// Input-domain samples spanning `[0, 1]`.
    pub samples: Box<[f32]>,
}

impl Lut1d {
    /// Creates a LUT from normalized samples.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::InvalidLut`] for fewer than two samples or any
    /// non-finite/out-of-range sample.
    pub fn new(samples: Vec<f32>) -> Result<Self, ColorError> {
        if samples.len() < 2
            || samples
                .iter()
                .any(|sample| !sample.is_finite() || !(0.0..=1.0).contains(sample))
        {
            return Err(ColorError::InvalidLut);
        }
        Ok(Self {
            samples: samples.into_boxed_slice(),
        })
    }

    /// Applies the table to one normalized input using linear interpolation.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::OutOfRange`] or [`ColorError::NonFinite`] for an
    /// invalid input value.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn apply(&self, input: f32) -> Result<f32, ColorError> {
        if !input.is_finite() {
            return Err(ColorError::NonFinite);
        }
        if !(0.0..=1.0).contains(&input) {
            return Err(ColorError::OutOfRange);
        }
        let position = input * (self.samples.len() - 1) as f32;
        let lower = position.floor() as usize;
        let upper = lower.min(self.samples.len() - 2) + 1;
        let fraction = position - lower as f32;
        Ok(self.samples[lower] + (self.samples[upper] - self.samples[lower]) * fraction)
    }
}

/// Independent one-dimensional LUTs for RGB channels.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbLut {
    /// Red-channel LUT.
    pub red: Lut1d,
    /// Green-channel LUT.
    pub green: Lut1d,
    /// Blue-channel LUT.
    pub blue: Lut1d,
}

impl RgbLut {
    /// Applies channel LUTs to a normalized RGB sample.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] if the input sample or any LUT lookup is invalid.
    pub fn apply(&self, sample: Rgb) -> Result<Rgb, ColorError> {
        sample.validate()?;
        Rgb::new(
            self.red.apply(sample.red)?,
            self.green.apply(sample.green)?,
            self.blue.apply(sample.blue)?,
        )
    }
}

/// Normalized RGB 3D LUT sampled on a regular cube with trilinear lookup.
#[derive(Clone, Debug, PartialEq)]
pub struct Lut3d {
    /// Number of samples along each cube axis.
    pub size: usize,
    /// Cube samples in red-fastest, then green, then blue order.
    pub samples: Box<[Rgb]>,
}

impl Lut3d {
    /// Creates a validated cube LUT.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::InvalidLut`] when the size is below two, the
    /// sample count is not `size³`, or any output channel is invalid.
    pub fn new(size: usize, samples: Vec<Rgb>) -> Result<Self, ColorError> {
        let expected = size
            .checked_mul(size)
            .and_then(|value| value.checked_mul(size))
            .ok_or(ColorError::InvalidLut)?;
        if size < 2 || samples.len() != expected {
            return Err(ColorError::InvalidLut);
        }
        for sample in &samples {
            sample.validate().map_err(|_| ColorError::InvalidLut)?;
        }
        Ok(Self {
            size,
            samples: samples.into_boxed_slice(),
        })
    }

    /// Applies trilinear interpolation to one normalized RGB input.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::NonFinite`] or [`ColorError::OutOfRange`] for an
    /// invalid input sample.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn apply(&self, input: Rgb) -> Result<Rgb, ColorError> {
        input.validate()?;
        let (r0, r1, rf) = cube_axis(input.red, self.size);
        let (g0, g1, gf) = cube_axis(input.green, self.size);
        let (b0, b1, bf) = cube_axis(input.blue, self.size);
        let sample = |r, g, b| self.samples[r + self.size * (g + self.size * b)];
        let c000 = sample(r0, g0, b0);
        let c100 = sample(r1, g0, b0);
        let c010 = sample(r0, g1, b0);
        let c110 = sample(r1, g1, b0);
        let c001 = sample(r0, g0, b1);
        let c101 = sample(r1, g0, b1);
        let c011 = sample(r0, g1, b1);
        let c111 = sample(r1, g1, b1);
        let lower = mix_rgb(mix_rgb(c000, c100, rf), mix_rgb(c010, c110, rf), gf);
        let upper = mix_rgb(mix_rgb(c001, c101, rf), mix_rgb(c011, c111, rf), gf);
        Ok(mix_rgb(lower, upper, bf))
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn cube_axis(value: f32, size: usize) -> (usize, usize, f32) {
    let position = value * (size - 1) as f32;
    let lower = position.floor() as usize;
    let upper = lower.min(size - 2) + 1;
    (lower, upper, position - lower as f32)
}

fn mix_rgb(left: Rgb, right: Rgb, amount: f32) -> Rgb {
    Rgb {
        red: left.red + (right.red - left.red) * amount,
        green: left.green + (right.green - left.green) * amount,
        blue: left.blue + (right.blue - left.blue) * amount,
    }
}

/// Deterministic normalized SDR grading controls applied in linear RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorAdjustment {
    /// Exposure change in stops, limited to a practical SDR range.
    pub exposure_stops: f32,
    /// Contrast around middle gray, where `0` is neutral.
    pub contrast: f32,
    /// Saturation change, where `0` is neutral and `-1` is monochrome.
    pub saturation: f32,
    /// Warm/cool shift, where positive values warm the image.
    pub temperature: f32,
    /// Green/magenta shift, where positive values move toward magenta.
    pub tint: f32,
}

impl Default for ColorAdjustment {
    fn default() -> Self {
        Self {
            exposure_stops: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            temperature: 0.0,
            tint: 0.0,
        }
    }
}

impl ColorAdjustment {
    /// Creates neutral validated grading controls.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validates supported grading ranges.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::InvalidAdjustment`] for non-finite values or
    /// controls outside the bounded SDR foundation.
    pub fn validate(self) -> Result<(), ColorError> {
        if !self.exposure_stops.is_finite()
            || !(-16.0..=16.0).contains(&self.exposure_stops)
            || !self.contrast.is_finite()
            || !(-1.0..=1.0).contains(&self.contrast)
            || !self.saturation.is_finite()
            || !(-1.0..=4.0).contains(&self.saturation)
            || !self.temperature.is_finite()
            || !(-1.0..=1.0).contains(&self.temperature)
            || !self.tint.is_finite()
            || !(-1.0..=1.0).contains(&self.tint)
        {
            return Err(ColorError::InvalidAdjustment);
        }
        Ok(())
    }

    /// Applies grading controls to a normalized linear-light RGB value.
    ///
    /// The output is clamped to the SDR range so it can safely re-enter the
    /// validated [`Rgb`] representation. This is a deterministic CPU grading
    /// primitive, not a substitute for a display or gamut transform.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when the controls or input RGB value are invalid.
    pub fn apply_linear(self, rgb: Rgb) -> Result<Rgb, ColorError> {
        self.validate()?;
        rgb.validate()?;
        let exposure = 2.0_f32.powf(self.exposure_stops);
        let temperature_red = 1.0 + self.temperature * 0.1;
        let temperature_blue = 1.0 - self.temperature * 0.1;
        let tint_red_blue = 1.0 + self.tint * 0.05;
        let tint_green = 1.0 - self.tint * 0.1;
        let mut red = rgb.red * exposure * temperature_red * tint_red_blue;
        let mut green = rgb.green * exposure * tint_green;
        let mut blue = rgb.blue * exposure * temperature_blue * tint_red_blue;
        let contrast = 1.0 + self.contrast;
        red = (red - 0.5) * contrast + 0.5;
        green = (green - 0.5) * contrast + 0.5;
        blue = (blue - 0.5) * contrast + 0.5;
        let luma = red * 0.2126 + green * 0.7152 + blue * 0.0722;
        let saturation = 1.0 + self.saturation;
        red = luma + (red - luma) * saturation;
        green = luma + (green - luma) * saturation;
        blue = luma + (blue - luma) * saturation;
        Rgb::new(
            red.clamp(0.0, 1.0),
            green.clamp(0.0, 1.0),
            blue.clamp(0.0, 1.0),
        )
    }
}

impl Rgb {
    /// Returns whether all three channels are exactly equal.
    #[must_use]
    pub fn is_grayscale(self) -> bool {
        self.red.to_bits() == self.green.to_bits() && self.green.to_bits() == self.blue.to_bits()
    }

    /// Creates a validated normalized RGB value.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::NonFinite`] or [`ColorError::OutOfRange`] when a
    /// channel cannot be represented by this SDR foundation.
    pub fn new(red: f32, green: f32, blue: f32) -> Result<Self, ColorError> {
        let value = Self { red, green, blue };
        value.validate()?;
        Ok(value)
    }

    /// Validates all channels.
    ///
    /// # Errors
    ///
    /// Returns a [`ColorError`] for non-finite or out-of-range channels.
    pub fn validate(self) -> Result<(), ColorError> {
        for channel in [self.red, self.green, self.blue] {
            if !channel.is_finite() {
                return Err(ColorError::NonFinite);
            }
            if !(0.0..=1.0).contains(&channel) {
                return Err(ColorError::OutOfRange);
            }
        }
        Ok(())
    }

    /// Decodes nonlinear channels into linear-light values.
    ///
    /// # Errors
    ///
    /// Returns a [`ColorError`] if this value is invalid.
    pub fn decode(self, transfer: TransferFunction) -> Result<Self, ColorError> {
        self.validate()?;
        Ok(self.map(|channel| decode_channel(channel, transfer)))
    }

    /// Encodes linear-light channels with a transfer function.
    ///
    /// # Errors
    ///
    /// Returns a [`ColorError`] if this value is invalid.
    pub fn encode(self, transfer: TransferFunction) -> Result<Self, ColorError> {
        self.validate()?;
        Ok(self.map(|channel| encode_channel(channel, transfer)))
    }

    fn map(self, function: impl Fn(f32) -> f32) -> Self {
        Self {
            red: function(self.red),
            green: function(self.green),
            blue: function(self.blue),
        }
    }
}

fn decode_channel(channel: f32, transfer: TransferFunction) -> f32 {
    match transfer {
        TransferFunction::Srgb => {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        }
        TransferFunction::Rec709 => {
            if channel < 0.081 {
                channel / 4.5
            } else {
                ((channel + 0.099) / 1.099).powf(1.0 / 0.45)
            }
        }
    }
}

fn encode_channel(channel: f32, transfer: TransferFunction) -> f32 {
    match transfer {
        TransferFunction::Srgb => {
            if channel <= 0.003_130_8 {
                channel * 12.92
            } else {
                1.055 * channel.powf(1.0 / 2.4) - 0.055
            }
        }
        TransferFunction::Rec709 => {
            if channel < 0.018 {
                channel * 4.5
            } else {
                1.099 * channel.powf(0.45) - 0.099
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColorAdjustment, ColorError, HISTOGRAM_BINS, LumaHistogram, Lut1d, Lut3d, Rgb,
        RgbHistogram, RgbLut, TransferFunction, VECTORSCOPE_BINS, Vectorscope, WaveformScope,
    };

    #[test]
    fn transfer_functions_preserve_endpoints_and_round_trip() -> Result<(), ColorError> {
        for transfer in [TransferFunction::Srgb, TransferFunction::Rec709] {
            let black = Rgb::new(0.0, 0.0, 0.0)?
                .decode(transfer)?
                .encode(transfer)?;
            let white = Rgb::new(1.0, 1.0, 1.0)?
                .decode(transfer)?
                .encode(transfer)?;
            assert!(black.red.abs() < 1e-7 && black.green.abs() < 1e-7 && black.blue.abs() < 1e-7);
            assert!((white.red - 1.0).abs() < 1e-6);
            assert!((white.green - 1.0).abs() < 1e-6);
            assert!((white.blue - 1.0).abs() < 1e-6);

            let source = Rgb::new(0.18, 0.5, 0.82)?;
            let round_trip = source.decode(transfer)?.encode(transfer)?;
            assert!((round_trip.red - source.red).abs() < 1e-5);
            assert!((round_trip.green - source.green).abs() < 1e-5);
            assert!((round_trip.blue - source.blue).abs() < 1e-5);
        }
        Ok(())
    }

    #[test]
    fn validation_rejects_non_finite_and_hdr_values_in_sdr_foundation() {
        assert_eq!(Rgb::new(f32::NAN, 0.0, 0.0), Err(ColorError::NonFinite));
        assert_eq!(Rgb::new(1.1, 0.0, 0.0), Err(ColorError::OutOfRange));
        assert_eq!(Rgb::new(0.0, -0.1, 0.0), Err(ColorError::OutOfRange));
    }

    #[test]
    fn neutral_adjustment_is_identity_and_controls_are_bounded() -> Result<(), ColorError> {
        let source = Rgb::new(0.2, 0.5, 0.8)?;
        let adjusted = ColorAdjustment::new().apply_linear(source)?;
        assert!((adjusted.red - source.red).abs() < 1e-6);
        assert!((adjusted.green - source.green).abs() < 1e-6);
        assert!((adjusted.blue - source.blue).abs() < 1e-6);
        assert_eq!(
            ColorAdjustment {
                exposure_stops: f32::NAN,
                ..ColorAdjustment::new()
            }
            .validate(),
            Err(ColorError::InvalidAdjustment)
        );
        Ok(())
    }

    #[test]
    fn grading_is_deterministic_clamped_and_changes_expected_channels() -> Result<(), ColorError> {
        let warm = ColorAdjustment {
            temperature: 1.0,
            ..ColorAdjustment::new()
        }
        .apply_linear(Rgb::new(0.5, 0.5, 0.5)?)?;
        assert!(warm.red > warm.blue);
        let bright = ColorAdjustment {
            exposure_stops: 16.0,
            ..ColorAdjustment::new()
        }
        .apply_linear(Rgb::new(0.5, 0.5, 0.5)?)?;
        assert_eq!(bright, Rgb::new(1.0, 1.0, 1.0)?);
        Ok(())
    }

    #[test]
    fn histogram_counts_exact_bins_and_rejects_invalid_samples() -> Result<(), ColorError> {
        let histogram =
            RgbHistogram::from_pixels(&[Rgb::new(0.0, 0.5, 1.0)?, Rgb::new(0.0, 0.5, 1.0)?])?;
        assert_eq!(histogram.red[0], 2);
        assert_eq!(histogram.green[128], 2);
        assert_eq!(histogram.blue[255], 2);
        assert_eq!(histogram.sample_count(), 2);
        let (red, green, blue) = histogram.percentages();
        assert!((red[0] - 1.0).abs() < f32::EPSILON);
        assert!((green[128] - 1.0).abs() < f32::EPSILON);
        assert!((blue[255] - 1.0).abs() < f32::EPSILON);
        assert_eq!(
            RgbHistogram::from_pixels(&[Rgb {
                red: f32::NAN,
                green: 0.0,
                blue: 0.0,
            }]),
            Err(ColorError::NonFinite)
        );
        Ok(())
    }

    #[test]
    fn luma_histogram_uses_rec709_bins_and_validates_samples() -> Result<(), ColorError> {
        let histogram = LumaHistogram::from_pixels(&[
            Rgb::new(0.0, 0.0, 0.0)?,
            Rgb::new(1.0, 1.0, 1.0)?,
            Rgb::new(0.5, 0.5, 0.5)?,
        ])?;
        assert_eq!(histogram.bins[0], 1);
        assert_eq!(histogram.bins[128], 1);
        assert_eq!(histogram.bins[255], 1);
        assert_eq!(histogram.sample_count(), 3);
        let percentages = histogram.percentages();
        assert!((percentages[0] - (1.0 / 3.0)).abs() < 1e-6);
        assert!((LumaHistogram::new().percentages()[0]).abs() < f32::EPSILON);
        assert_eq!(
            LumaHistogram::from_pixels(&[Rgb {
                red: 0.0,
                green: f32::INFINITY,
                blue: 0.0,
            }]),
            Err(ColorError::NonFinite)
        );
        Ok(())
    }

    #[test]
    fn waveform_scope_preserves_columns_and_rejects_bad_dimensions() -> Result<(), ColorError> {
        assert_eq!(WaveformScope::new(0), Err(ColorError::InvalidScopeWidth));
        let scope = WaveformScope::from_pixels(
            2,
            &[
                Rgb::new(0.0, 0.0, 0.0)?,
                Rgb::new(1.0, 1.0, 1.0)?,
                Rgb::new(0.5, 0.5, 0.5)?,
                Rgb::new(0.0, 0.0, 0.0)?,
            ],
        )?;
        assert_eq!(scope.width, 2);
        assert_eq!(scope.bins.iter().sum::<u64>(), 4);
        assert_eq!(scope.sample_count(), 4);
        assert_eq!(scope.column_bins(0).map(<[u64]>::len), Some(256));
        assert!(scope.column_bins(2).is_none());
        assert_eq!(scope.bins[0], 1);
        assert_eq!(scope.bins[HISTOGRAM_BINS + 255], 1);
        assert_eq!(
            WaveformScope::from_pixels(3, &[Rgb::new(0.0, 0.0, 0.0)?]),
            Err(ColorError::InvalidScopePixelCount)
        );
        assert_eq!(
            WaveformScope::from_pixels(
                1,
                &[Rgb {
                    red: 0.0,
                    green: f32::NAN,
                    blue: 0.0,
                }]
            ),
            Err(ColorError::NonFinite)
        );
        Ok(())
    }

    #[test]
    fn one_dimensional_luts_interpolate_and_apply_per_channel() -> Result<(), ColorError> {
        assert_eq!(Lut1d::new(vec![0.0]), Err(ColorError::InvalidLut));
        let identity = Lut1d::new(vec![0.0, 1.0])?;
        assert!((identity.apply(0.25)? - 0.25).abs() < f32::EPSILON);
        let invert = Lut1d::new(vec![1.0, 0.0])?;
        let lut = RgbLut {
            red: invert,
            green: identity.clone(),
            blue: identity,
        };
        let mapped = lut.apply(Rgb::new(0.25, 0.5, 0.75)?)?;
        assert!((mapped.red - 0.75).abs() < f32::EPSILON);
        assert!((mapped.green - 0.5).abs() < f32::EPSILON);
        assert!((mapped.blue - 0.75).abs() < f32::EPSILON);
        assert_eq!(lut.red.apply(f32::NAN), Err(ColorError::NonFinite));
        assert_eq!(
            Lut1d::new(vec![0.0, f32::INFINITY]),
            Err(ColorError::InvalidLut)
        );
        Ok(())
    }

    #[test]
    fn three_dimensional_lut_trilinearly_interpolates_identity_cube() -> Result<(), ColorError> {
        let samples = [
            Rgb::new(0.0, 0.0, 0.0)?,
            Rgb::new(1.0, 0.0, 0.0)?,
            Rgb::new(0.0, 1.0, 0.0)?,
            Rgb::new(1.0, 1.0, 0.0)?,
            Rgb::new(0.0, 0.0, 1.0)?,
            Rgb::new(1.0, 0.0, 1.0)?,
            Rgb::new(0.0, 1.0, 1.0)?,
            Rgb::new(1.0, 1.0, 1.0)?,
        ];
        assert_eq!(Lut3d::new(1, samples.to_vec()), Err(ColorError::InvalidLut));
        let lut = Lut3d::new(2, samples.to_vec())?;
        let mapped = lut.apply(Rgb::new(0.25, 0.5, 0.75)?)?;
        assert!((mapped.red - 0.25).abs() < 1e-6);
        assert!((mapped.green - 0.5).abs() < 1e-6);
        assert!((mapped.blue - 0.75).abs() < 1e-6);
        assert_eq!(
            Lut3d::new(2, samples[..7].to_vec()),
            Err(ColorError::InvalidLut)
        );
        Ok(())
    }

    #[test]
    fn vectorscope_counts_chroma_coordinates_and_rejects_invalid_samples() -> Result<(), ColorError>
    {
        let scope =
            Vectorscope::from_pixels(&[Rgb::new(1.0, 0.0, 0.0)?, Rgb::new(0.5, 0.5, 0.5)?])?;
        assert_eq!(scope.bins.iter().sum::<u64>(), 2);
        assert_eq!(scope.sample_count(), 2);
        let neutral_bin = VECTORSCOPE_BINS / 2;
        assert_eq!(scope.bins[neutral_bin * VECTORSCOPE_BINS + neutral_bin], 1);
        assert_eq!(scope.bin_count(neutral_bin, neutral_bin), Some(1));
        assert_eq!(scope.bin_count(VECTORSCOPE_BINS, 0), None);
        assert_eq!(
            Vectorscope::from_pixels(&[Rgb {
                red: f32::NAN,
                green: 0.0,
                blue: 0.0,
            }]),
            Err(ColorError::NonFinite)
        );
        Ok(())
    }
}
