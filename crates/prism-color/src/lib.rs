//! Validated color-management and display-transform primitives.
//!
//! These functions operate on normalized, non-negative channel values. They
//! are building blocks for a color-managed render graph, including a bounded
//! metadata-driven PQ Rec.2020 to SDR Rec.709 display transform.

use thiserror::Error;

/// Transfer functions supported by the initial color foundation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferFunction {
    /// IEC 61966-2-1 sRGB OETF/EOTF pair.
    Srgb,
    /// ITU-R BT.709 SDR transfer curve.
    Rec709,
    /// SMPTE ST 2084 perceptual quantizer, normalized to 10,000 cd/m².
    Pq,
    /// ARIB STD-B67 hybrid log-gamma transfer curve.
    Hlg,
}

/// D65 RGB primaries supported by the color-management foundation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorPrimaries {
    /// ITU-R BT.709 / sRGB primaries.
    Rec709,
    /// Display P3 primaries with a D65 white point.
    DisplayP3,
    /// ITU-R BT.2020 primaries.
    Rec2020,
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
    /// HDR mastering or content-light metadata was inconsistent.
    #[error("HDR static metadata is invalid")]
    InvalidHdrMetadata,
    /// HDR display-transform luminance parameters were invalid.
    #[error("HDR display transform parameters are invalid")]
    InvalidDisplayTransform,
}

/// Static HDR mastering and content-light metadata in cd/m² (nits).
///
/// This is the validated engine representation of the luminance fields
/// commonly carried by mastering-display and content-light-level metadata.
/// Chromaticity coordinates remain the responsibility of the declared color
/// primaries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrStaticMetadata {
    /// Mastering display minimum luminance.
    pub mastering_min_luminance: f32,
    /// Mastering display peak luminance.
    pub mastering_max_luminance: f32,
    /// Maximum content light level.
    pub max_content_light_level: f32,
    /// Maximum frame-average light level.
    pub max_frame_average_light_level: f32,
}

impl HdrStaticMetadata {
    /// Validates finite, ordered luminance metadata.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::InvalidHdrMetadata`] if values are non-finite,
    /// negative, unordered, or outside the 10,000-nit PQ system range.
    pub fn validate(self) -> Result<(), ColorError> {
        let values = [
            self.mastering_min_luminance,
            self.mastering_max_luminance,
            self.max_content_light_level,
            self.max_frame_average_light_level,
        ];
        if values.iter().any(|value| !value.is_finite())
            || self.mastering_min_luminance < 0.0
            || self.mastering_max_luminance <= self.mastering_min_luminance
            || self.mastering_max_luminance > 10_000.0
            || self.max_content_light_level <= 0.0
            || self.max_content_light_level > 10_000.0
            || self.max_frame_average_light_level <= 0.0
            || self.max_frame_average_light_level > self.max_content_light_level
        {
            return Err(ColorError::InvalidHdrMetadata);
        }
        Ok(())
    }

    fn effective_peak(self) -> f32 {
        self.max_content_light_level
    }
}

/// Deterministic PQ Rec.2020 to SDR Rec.709 display transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PqToSdrDisplayTransform {
    /// Validated source mastering/content-light metadata.
    pub metadata: HdrStaticMetadata,
    /// Target SDR display peak in cd/m².
    pub target_peak_luminance: f32,
    /// Fraction of target peak below which luminance remains unchanged.
    pub knee_start: f32,
}

/// Configurable HDR-to-SDR display transform. This is the shared path for PQ
/// and HLG sources, with explicit source primaries and transfer metadata.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrToSdrDisplayTransform {
    /// Source transfer function (PQ or HLG).
    pub source_transfer: TransferFunction,
    /// Source RGB primaries.
    pub source_primaries: ColorPrimaries,
    /// Validated mastering/content-light metadata.
    pub metadata: HdrStaticMetadata,
    /// Target SDR display peak in cd/m².
    pub target_peak_luminance: f32,
    /// Fraction of target peak below which luminance remains unchanged.
    pub knee_start: f32,
}

impl HdrToSdrDisplayTransform {
    /// Validates the transform configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when metadata or transform parameters are invalid.
    pub fn validate(self) -> Result<(), ColorError> {
        self.metadata.validate()?;
        if !matches!(
            self.source_transfer,
            TransferFunction::Pq | TransferFunction::Hlg
        ) || !self.target_peak_luminance.is_finite()
            || !(48.0..=1_000.0).contains(&self.target_peak_luminance)
            || !self.knee_start.is_finite()
            || !(0.0..1.0).contains(&self.knee_start)
        {
            return Err(ColorError::InvalidDisplayTransform);
        }
        Ok(())
    }

    /// Maps HDR source encoding into normalized Rec.709 SDR encoding.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] when the configuration or input RGB is invalid.
    pub fn apply(self, encoded: Rgb) -> Result<Rgb, ColorError> {
        self.validate()?;
        let decoded = encoded.decode(self.source_transfer)?;
        let nits = [
            decoded.red * 10_000.0,
            decoded.green * 10_000.0,
            decoded.blue * 10_000.0,
        ];
        let luminance = 0.2627 * nits[0] + 0.6780 * nits[1] + 0.0593 * nits[2];
        let target = self.target_peak_luminance;
        let knee = target * self.knee_start;
        let mapped = if luminance <= knee {
            luminance
        } else {
            let source_peak = self.metadata.effective_peak().max(target);
            let distance = target - knee;
            let source_distance = source_peak - knee;
            let normalization = 1.0 - (-source_distance / distance).exp();
            (knee
                + distance * (1.0 - (-(luminance.min(source_peak) - knee) / distance).exp())
                    / normalization)
                .min(target)
        };
        let scale = if luminance <= f32::EPSILON {
            0.0
        } else {
            mapped / luminance
        };
        let target_linear = [
            nits[0] * scale / target,
            nits[1] * scale / target,
            nits[2] * scale / target,
        ];
        let xyz = multiply(primary_matrix(self.source_primaries), target_linear);
        let rec709 = multiply(inverse_primary_matrix(ColorPrimaries::Rec709), xyz);
        Rgb::new(
            rec709[0].clamp(0.0, 1.0),
            rec709[1].clamp(0.0, 1.0),
            rec709[2].clamp(0.0, 1.0),
        )?
        .encode(TransferFunction::Rec709)
    }
}

impl PqToSdrDisplayTransform {
    /// Validates transform metadata and display parameters.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError::InvalidHdrMetadata`] or
    /// [`ColorError::InvalidDisplayTransform`] for invalid configuration.
    pub fn validate(self) -> Result<(), ColorError> {
        self.metadata.validate()?;
        if !self.target_peak_luminance.is_finite()
            || !(48.0..=1_000.0).contains(&self.target_peak_luminance)
            || !self.knee_start.is_finite()
            || !(0.0..1.0).contains(&self.knee_start)
        {
            return Err(ColorError::InvalidDisplayTransform);
        }
        Ok(())
    }

    /// Maps normalized PQ Rec.2020 into normalized Rec.709 display encoding.
    ///
    /// The transform decodes absolute PQ luminance, applies a smooth
    /// luminance-preserving highlight roll-off whose source endpoint is driven
    /// by validated static metadata, converts primaries in linear light, clips
    /// the target gamut, and applies the Rec.709 transfer function.
    ///
    /// # Errors
    ///
    /// Returns [`ColorError`] for invalid configuration or input RGB.
    pub fn apply(self, encoded_rec2020: Rgb) -> Result<Rgb, ColorError> {
        HdrToSdrDisplayTransform {
            source_transfer: TransferFunction::Pq,
            source_primaries: ColorPrimaries::Rec2020,
            metadata: self.metadata,
            target_peak_luminance: self.target_peak_luminance,
            knee_start: self.knee_start,
        }
        .apply(encoded_rec2020)
    }
}

/// Converts a linear-light RGB value between D65 working spaces.
///
/// Matrix conversion is followed by deterministic `[0, 1]` gamut clipping so
/// the result remains representable by [`Rgb`]. Callers that need scene-linear
/// values outside that display range should use a wider-float frame type.
///
/// # Errors
///
/// Returns [`ColorError::NonFinite`] or [`ColorError::OutOfRange`] when the
/// input is not a normalized RGB value.
pub fn convert_linear(
    value: Rgb,
    source: ColorPrimaries,
    destination: ColorPrimaries,
) -> Result<Rgb, ColorError> {
    value.validate()?;
    if source == destination {
        return Ok(value);
    }
    let xyz = multiply(primary_matrix(source), [value.red, value.green, value.blue]);
    let rgb = multiply(inverse_primary_matrix(destination), xyz);
    Rgb::new(
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
    )
}

fn multiply(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

#[allow(clippy::excessive_precision, clippy::unreadable_literal)]
fn primary_matrix(primaries: ColorPrimaries) -> [[f32; 3]; 3] {
    match primaries {
        ColorPrimaries::Rec709 => [
            [0.4123908, 0.3575843, 0.1804808],
            [0.2126390, 0.7151687, 0.0721923],
            [0.0193308, 0.1191948, 0.9505322],
        ],
        ColorPrimaries::DisplayP3 => [
            [0.48657095, 0.26566769, 0.19821729],
            [0.22897456, 0.69173852, 0.07928691],
            [0.0, 0.04511338, 1.04394437],
        ],
        ColorPrimaries::Rec2020 => [
            [0.63695805, 0.14461690, 0.16888097],
            [0.26270021, 0.67799807, 0.05930172],
            [0.0, 0.02807269, 1.06098506],
        ],
    }
}

#[allow(clippy::excessive_precision, clippy::unreadable_literal)]
fn inverse_primary_matrix(primaries: ColorPrimaries) -> [[f32; 3]; 3] {
    match primaries {
        ColorPrimaries::Rec709 => [
            [3.24096994, -1.53738318, -0.49861076],
            [-0.96924364, 1.8759675, 0.04155506],
            [0.05563008, -0.20397696, 1.05697151],
        ],
        ColorPrimaries::DisplayP3 => [
            [2.49349691, -0.93138362, -0.40271078],
            [-0.82948897, 1.76266406, 0.02362469],
            [0.03584583, -0.07617239, 0.95688452],
        ],
        ColorPrimaries::Rec2020 => [
            [1.71665119, -0.35567078, -0.25336628],
            [-0.66668435, 1.61648124, 0.01576855],
            [0.01763986, -0.04277061, 0.94210312],
        ],
    }
}

/// Number of bins in the normalized RGB histogram scope.
pub const HISTOGRAM_BINS: usize = 256;

/// Number of bins per axis in the normalized vectorscope.
pub const VECTORSCOPE_BINS: usize = 256;

/// Density scaling used to prepare exact scope counts for presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeDensityScale {
    /// Normalize counts directly against the peak bin.
    Linear,
    /// Apply `ln(1 + count)` before peak normalization to reveal sparse detail.
    Logarithmic,
}

/// Backend-neutral normalized scope density image.
#[derive(Clone, Debug, PartialEq)]
pub struct ScopePresentation {
    /// Presentation width in bins.
    pub width: usize,
    /// Presentation height in bins.
    pub height: usize,
    /// Row-major density values in `[0, 1]`.
    pub density: Box<[f32]>,
    /// Highest exact source-bin count used for normalization.
    pub peak_count: u64,
    /// Scaling applied to source counts.
    pub scale: ScopeDensityScale,
}

impl ScopePresentation {
    fn from_counts(
        width: usize,
        height: usize,
        counts: impl IntoIterator<Item = u64>,
        scale: ScopeDensityScale,
    ) -> Self {
        let counts: Vec<u64> = counts.into_iter().collect();
        let peak_count = counts.iter().copied().max().unwrap_or(0);
        let density = if peak_count == 0 {
            vec![0.0; counts.len()]
        } else {
            #[allow(clippy::cast_precision_loss)]
            let peak = peak_count as f32;
            counts
                .into_iter()
                .map(|count| {
                    #[allow(clippy::cast_precision_loss)]
                    let count = count as f32;
                    match scale {
                        ScopeDensityScale::Linear => count / peak,
                        ScopeDensityScale::Logarithmic => count.ln_1p() / peak.ln_1p(),
                    }
                })
                .collect()
        };
        Self {
            width,
            height,
            density: density.into_boxed_slice(),
            peak_count,
            scale,
        }
    }

    /// Returns one density value, or `None` for an out-of-range coordinate.
    #[must_use]
    pub fn value(&self, x: usize, y: usize) -> Option<f32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.density.get(y * self.width + x).copied()
    }
}

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

    /// Produces a row-major normalized density map for a scope renderer.
    ///
    /// The exact scope stores column-major bins; presentation rows are flipped
    /// so black is at the bottom and white is at the top.
    #[must_use]
    pub fn presentation(&self, scale: ScopeDensityScale) -> ScopePresentation {
        let counts = (0..HISTOGRAM_BINS).flat_map(|row| {
            let luma_bin = HISTOGRAM_BINS - 1 - row;
            (0..self.width).map(move |column| self.bins[column * HISTOGRAM_BINS + luma_bin])
        });
        ScopePresentation::from_counts(self.width, HISTOGRAM_BINS, counts, scale)
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
        Some(self.bins[v * VECTORSCOPE_BINS + u])
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

    /// Produces a normalized density map retaining U/V coordinate layout.
    #[must_use]
    pub fn presentation(&self, scale: ScopeDensityScale) -> ScopePresentation {
        ScopePresentation::from_counts(
            VECTORSCOPE_BINS,
            VECTORSCOPE_BINS,
            self.bins.iter().copied(),
            scale,
        )
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
            red: function(self.red).clamp(0.0, 1.0),
            green: function(self.green).clamp(0.0, 1.0),
            blue: function(self.blue).clamp(0.0, 1.0),
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
        TransferFunction::Pq => {
            let m1 = 2610.0 / 16384.0;
            let m2 = 2523.0 / 32.0;
            let c1 = 3424.0 / 4096.0;
            let c2 = 2413.0 / 128.0;
            let c3 = 2392.0 / 128.0;
            let powered = channel.powf(1.0 / m2);
            ((powered - c1).max(0.0) / (c2 - c3 * powered))
                .max(0.0)
                .powf(1.0 / m1)
        }
        TransferFunction::Hlg => {
            const A: f32 = 0.178_832_77;
            const B: f32 = 0.284_668_92;
            const C: f32 = 0.559_910_7;
            if channel <= 0.5 {
                channel * channel / 3.0
            } else {
                (((channel - C) / A).exp() + B) / 12.0
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
        TransferFunction::Pq => {
            let m1 = 2610.0 / 16384.0;
            let m2 = 2523.0 / 32.0;
            let c1 = 3424.0 / 4096.0;
            let c2 = 2413.0 / 128.0;
            let c3 = 2392.0 / 128.0;
            let powered = channel.powf(m1);
            ((c1 + c2 * powered) / (1.0 + c3 * powered)).powf(m2)
        }
        TransferFunction::Hlg => {
            const A: f32 = 0.178_832_77;
            const B: f32 = 0.284_668_92;
            const C: f32 = 0.559_910_7;
            if channel <= 1.0 / 12.0 {
                (3.0 * channel).sqrt()
            } else {
                A * (12.0 * channel - B).ln() + C
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColorAdjustment, ColorError, ColorPrimaries, HISTOGRAM_BINS, HdrStaticMetadata,
        HdrToSdrDisplayTransform, LumaHistogram, Lut1d, Lut3d, PqToSdrDisplayTransform, Rgb,
        RgbHistogram, RgbLut, ScopeDensityScale, TransferFunction, VECTORSCOPE_BINS, Vectorscope,
        WaveformScope, convert_linear,
    };

    #[test]
    fn transfer_functions_preserve_endpoints_and_round_trip() -> Result<(), ColorError> {
        for transfer in [
            TransferFunction::Srgb,
            TransferFunction::Rec709,
            TransferFunction::Pq,
            TransferFunction::Hlg,
        ] {
            let black = Rgb::new(0.0, 0.0, 0.0)?
                .decode(transfer)?
                .encode(transfer)?;
            let white = Rgb::new(1.0, 1.0, 1.0)?
                .decode(transfer)?
                .encode(transfer)?;
            assert!(black.red.abs() < 1e-5 && black.green.abs() < 1e-5 && black.blue.abs() < 1e-5);
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
    fn primaries_conversion_round_trips_neutral_and_validates_input() -> Result<(), ColorError> {
        let source = Rgb::new(0.18, 0.5, 0.82)?;
        for destination in [ColorPrimaries::DisplayP3, ColorPrimaries::Rec2020] {
            let converted = convert_linear(source, ColorPrimaries::Rec709, destination)?;
            let round_trip = convert_linear(converted, destination, ColorPrimaries::Rec709)?;
            assert!((round_trip.red - source.red).abs() < 2e-4);
            assert!((round_trip.green - source.green).abs() < 2e-4);
            assert!((round_trip.blue - source.blue).abs() < 2e-4);
        }
        let neutral = convert_linear(
            Rgb::new(0.5, 0.5, 0.5)?,
            ColorPrimaries::Rec709,
            ColorPrimaries::DisplayP3,
        )?;
        assert!((neutral.red - neutral.green).abs() < 2e-4);
        assert!((neutral.green - neutral.blue).abs() < 2e-4);
        assert_eq!(
            convert_linear(
                Rgb {
                    red: f32::NAN,
                    green: 0.0,
                    blue: 0.0,
                },
                ColorPrimaries::Rec709,
                ColorPrimaries::Rec2020,
            ),
            Err(ColorError::NonFinite)
        );
        Ok(())
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
        assert_eq!(scope.column_peak(0), Some(128));
        assert_eq!(scope.column_peak(1), Some(255));
        assert_eq!(scope.column_peak(2), None);
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
        assert!(matches!(scope.peak_bin(), Some((_, _, 1))));
        assert!(Vectorscope::new().peak_bin().is_none());
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

    #[test]
    fn waveform_presentation_is_row_major_flipped_and_scaled() -> Result<(), ColorError> {
        let scope = WaveformScope::from_pixels(
            1,
            &[
                Rgb::new(0.0, 0.0, 0.0)?,
                Rgb::new(0.0, 0.0, 0.0)?,
                Rgb::new(1.0, 1.0, 1.0)?,
            ],
        )?;
        let linear = scope.presentation(ScopeDensityScale::Linear);
        let logarithmic = scope.presentation(ScopeDensityScale::Logarithmic);
        assert_eq!(
            (linear.width, linear.height, linear.peak_count),
            (1, 256, 2)
        );
        assert!((linear.value(0, 255).unwrap_or_default() - 1.0).abs() < f32::EPSILON);
        assert!((linear.value(0, 0).unwrap_or_default() - 0.5).abs() < f32::EPSILON);
        assert!(logarithmic.value(0, 0).unwrap_or_default() > 0.5);
        assert_eq!(linear.value(1, 0), None);
        assert_eq!(linear.value(0, 256), None);
        Ok(())
    }

    #[test]
    fn vectorscope_presentation_preserves_uv_coordinates() -> Result<(), ColorError> {
        let red = Rgb::new(1.0, 0.0, 0.0)?;
        let green = Rgb::new(0.0, 1.0, 0.0)?;
        let scope = Vectorscope::from_pixels(&[red, red, green])?;
        let (u, v, count) = scope.peak_bin().unwrap_or_default();
        assert_eq!(count, 2);
        assert_eq!(scope.bin_count(u, v), Some(2));
        let presentation = scope.presentation(ScopeDensityScale::Linear);
        assert_eq!((presentation.width, presentation.height), (256, 256));
        assert_eq!(presentation.peak_count, 2);
        assert!((presentation.value(u, v).unwrap_or_default() - 1.0).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn empty_scope_presentation_is_zero_density() {
        let presentation = Vectorscope::new().presentation(ScopeDensityScale::Logarithmic);
        assert_eq!(presentation.peak_count, 0);
        assert!(presentation.density.iter().all(|value| *value == 0.0));
    }

    fn hdr_metadata() -> HdrStaticMetadata {
        HdrStaticMetadata {
            mastering_min_luminance: 0.005,
            mastering_max_luminance: 1_000.0,
            max_content_light_level: 1_000.0,
            max_frame_average_light_level: 400.0,
        }
    }

    #[test]
    fn hdr_static_metadata_rejects_inconsistent_luminance() {
        assert!(hdr_metadata().validate().is_ok());
        assert_eq!(
            HdrStaticMetadata {
                max_frame_average_light_level: 1_001.0,
                ..hdr_metadata()
            }
            .validate(),
            Err(ColorError::InvalidHdrMetadata)
        );
        assert_eq!(
            HdrStaticMetadata {
                mastering_max_luminance: f32::NAN,
                ..hdr_metadata()
            }
            .validate(),
            Err(ColorError::InvalidHdrMetadata)
        );
    }

    #[test]
    fn pq_display_transform_preserves_shadows_and_rolls_peak_to_white() -> Result<(), ColorError> {
        let transform = PqToSdrDisplayTransform {
            metadata: hdr_metadata(),
            target_peak_luminance: 100.0,
            knee_start: 0.75,
        };
        let pq_at_nits = |nits: f32| {
            Rgb::new(nits / 10_000.0, nits / 10_000.0, nits / 10_000.0)?
                .encode(TransferFunction::Pq)
        };
        let black = transform.apply(pq_at_nits(0.0)?)?;
        let shadow = transform.apply(pq_at_nits(50.0)?)?;
        let highlight = transform.apply(pq_at_nits(200.0)?)?;
        let peak = transform.apply(pq_at_nits(1_000.0)?)?;
        assert_eq!(black, Rgb::new(0.0, 0.0, 0.0)?);
        let expected_shadow = Rgb::new(0.5, 0.5, 0.5)?.encode(TransferFunction::Rec709)?;
        assert!((shadow.red - expected_shadow.red).abs() < 2.0e-4);
        assert!(shadow.red < highlight.red && highlight.red < peak.red);
        assert!((peak.red - 1.0).abs() < 2.0e-4);
        assert!((peak.red - peak.green).abs() < 2.0e-4);
        assert!((peak.green - peak.blue).abs() < 2.0e-4);
        Ok(())
    }

    #[test]
    fn pq_display_transform_validates_configuration_and_gamut_maps() -> Result<(), ColorError> {
        let invalid = PqToSdrDisplayTransform {
            metadata: hdr_metadata(),
            target_peak_luminance: 0.0,
            knee_start: 0.75,
        };
        assert_eq!(
            invalid.apply(Rgb::new(0.0, 0.0, 0.0)?),
            Err(ColorError::InvalidDisplayTransform)
        );
        let transform = PqToSdrDisplayTransform {
            metadata: hdr_metadata(),
            target_peak_luminance: 100.0,
            knee_start: 0.75,
        };
        let saturated_rec2020 = Rgb::new(0.1, 0.0, 0.0)?.encode(TransferFunction::Pq)?;
        let mapped = transform.apply(saturated_rec2020)?;
        mapped.validate()?;
        assert!(mapped.red >= mapped.green && mapped.red >= mapped.blue);
        Ok(())
    }

    #[test]
    fn configurable_transform_supports_hlg_and_rejects_sdr_sources() -> Result<(), ColorError> {
        let transform = HdrToSdrDisplayTransform {
            source_transfer: TransferFunction::Hlg,
            source_primaries: ColorPrimaries::Rec2020,
            metadata: hdr_metadata(),
            target_peak_luminance: 100.0,
            knee_start: 0.75,
        };
        let white = transform.apply(Rgb::new(1.0, 1.0, 1.0)?)?;
        assert!(white.red > 0.99 && white.green > 0.99 && white.blue > 0.99);
        let invalid = HdrToSdrDisplayTransform {
            source_transfer: TransferFunction::Rec709,
            ..transform
        };
        assert_eq!(invalid.validate(), Err(ColorError::InvalidDisplayTransform));
        Ok(())
    }
}
