//! Allocation-free building blocks for realtime-safe audio processing.
//!
//! The first slice operates on interleaved `f32` buffers. It deliberately does
//! not perform I/O, locking, logging, or allocation inside `process`, making it
//! suitable for a future realtime callback or dedicated audio render thread.

use std::f32::consts::FRAC_PI_4;
use thiserror::Error;

const LOUDNESS_BLOCK_FRAMES: usize = 19_200;
const LOUDNESS_STEP_FRAMES: usize = 4_800;
const LOUDNESS_ABSOLUTE_GATE: f64 = -70.0;

/// Audio processing validation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum AudioError {
    /// A stereo processor requires an even number of interleaved samples.
    #[error("interleaved stereo buffer has an odd sample count")]
    OddInterleavedBuffer,
    /// All tracks in one mix must have the same number of samples as output.
    #[error("audio track and output buffers must have the same sample count")]
    MismatchedBufferLength,
    /// Pan must remain in the normalized range.
    #[error("pan must be between -1.0 and 1.0")]
    InvalidPan,
    /// Gain must be finite.
    #[error("gain must be finite")]
    InvalidGain,
    /// Peak normalization targets must be finite and positive.
    #[error("target peak must be finite and greater than zero")]
    InvalidTargetPeak,
    /// Realtime processing rejects NaN and infinite sample values.
    #[error("audio buffer contains a non-finite sample")]
    NonFiniteSample,
    /// Sample rates must be positive.
    #[error("sample rates must be greater than zero")]
    InvalidSampleRate,
    /// Dynamics processors require finite, physically meaningful parameters.
    #[error("invalid dynamics processor parameter")]
    InvalidDynamicsParameter,
    /// Waveform windows must contain at least one sample.
    #[error("waveform window size must be greater than zero")]
    InvalidWaveformWindow,
    /// The streaming source-position counter cannot advance further.
    #[error("streaming resampler source position overflowed")]
    SampleCountOverflow,
    /// A routed track referenced an unavailable bus.
    #[error("audio route references an unavailable bus")]
    InvalidBusIndex,
    /// Fade gains must be finite.
    #[error("fade gain must be finite")]
    InvalidFadeGain,
    /// Crossfade progress must be normalized.
    #[error("crossfade progress must be between 0.0 and 1.0")]
    InvalidCrossfadeProgress,
    /// Filter sample rate and cutoff parameters are invalid.
    #[error("invalid audio filter parameter")]
    InvalidFilterParameter,
    /// Delay time, feedback, or wet mix is invalid.
    #[error("invalid stereo delay parameter")]
    InvalidDelayParameter,
    /// Integrated loudness currently has validated coefficients for 48 kHz.
    #[error("integrated loudness measurement requires a 48000 Hz sample rate")]
    UnsupportedLoudnessSampleRate,
}

/// Result of an ITU-R BS.1770 / EBU R128 integrated loudness measurement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntegratedLoudness {
    /// Gated programme loudness in LUFS. Digital silence is negative infinity.
    pub lufs: f64,
    /// Number of overlapping 400 ms blocks considered before gating.
    pub block_count: usize,
    /// Number of blocks retained by both the -70 LUFS absolute gate and the
    /// relative gate 10 LU below the absolute-gated mean.
    pub gated_block_count: usize,
}

/// Measures stereo programme loudness using BS.1770 K-weighting and EBU R128
/// integrated gating.
///
/// Input must be interleaved stereo at 48 kHz. The meter evaluates 400 ms
/// blocks at 100 ms intervals, applies the -70 LUFS absolute gate, then the
/// relative gate 10 LU below the remaining programme mean. A shorter final
/// fragment is intentionally excluded. The implementation allocates working
/// buffers and is intended for offline analysis, not a realtime callback.
///
/// # Errors
///
/// Returns [`AudioError::UnsupportedLoudnessSampleRate`],
/// [`AudioError::OddInterleavedBuffer`], or [`AudioError::NonFiniteSample`]
/// for invalid input.
pub fn measure_integrated_loudness(
    samples: &[f32],
    sample_rate: u32,
) -> Result<IntegratedLoudness, AudioError> {
    if sample_rate != 48_000 {
        return Err(AudioError::UnsupportedLoudnessSampleRate);
    }
    if !samples.len().is_multiple_of(2) {
        return Err(AudioError::OddInterleavedBuffer);
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(AudioError::NonFiniteSample);
    }

    let frame_count = samples.len() / 2;
    let block_count = frame_count
        .saturating_sub(LOUDNESS_BLOCK_FRAMES)
        .checked_div(LOUDNESS_STEP_FRAMES)
        .map_or(0, |count| {
            count + usize::from(frame_count >= LOUDNESS_BLOCK_FRAMES)
        });
    let mut energies = Vec::with_capacity(block_count);
    let mut left = KWeighting::default();
    let mut right = KWeighting::default();
    let mut weighted = Vec::with_capacity(samples.len());
    for frame in samples.chunks_exact(2) {
        weighted.push(left.process(f64::from(frame[0])));
        weighted.push(right.process(f64::from(frame[1])));
    }
    for start in
        (0..=frame_count.saturating_sub(LOUDNESS_BLOCK_FRAMES)).step_by(LOUDNESS_STEP_FRAMES)
    {
        if start + LOUDNESS_BLOCK_FRAMES > frame_count {
            break;
        }
        let mut sum = 0.0;
        for frame in weighted[start * 2..(start + LOUDNESS_BLOCK_FRAMES) * 2].chunks_exact(2) {
            sum += frame[0] * frame[0] + frame[1] * frame[1];
        }
        energies.push(sum / 19_200.0);
    }

    let absolute: Vec<f64> = energies
        .iter()
        .copied()
        .filter(|energy| energy_to_lufs(*energy) >= LOUDNESS_ABSOLUTE_GATE)
        .collect();
    if absolute.is_empty() {
        return Ok(IntegratedLoudness {
            lufs: f64::NEG_INFINITY,
            block_count: energies.len(),
            gated_block_count: 0,
        });
    }
    #[allow(clippy::cast_precision_loss)]
    let absolute_mean = absolute.iter().sum::<f64>() / absolute.len() as f64;
    let relative_gate = energy_to_lufs(absolute_mean) - 10.0;
    let gated: Vec<f64> = absolute
        .into_iter()
        .filter(|energy| energy_to_lufs(*energy) >= relative_gate)
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let mean = gated.iter().sum::<f64>() / gated.len() as f64;
    Ok(IntegratedLoudness {
        lufs: energy_to_lufs(mean),
        block_count: energies.len(),
        gated_block_count: gated.len(),
    })
}

fn energy_to_lufs(energy: f64) -> f64 {
    if energy == 0.0 {
        f64::NEG_INFINITY
    } else {
        -0.691 + 10.0 * energy.log10()
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct BiquadState {
    input_1: f64,
    input_2: f64,
    output_1: f64,
    output_2: f64,
}

impl BiquadState {
    fn process(&mut self, input: f64, numerator: [f64; 3], denominator: [f64; 2]) -> f64 {
        let output =
            numerator[0] * input + numerator[1] * self.input_1 + numerator[2] * self.input_2
                - denominator[0] * self.output_1
                - denominator[1] * self.output_2;
        self.input_2 = self.input_1;
        self.input_1 = input;
        self.output_2 = self.output_1;
        self.output_1 = output;
        output
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct KWeighting {
    shelf: BiquadState,
    high_pass: BiquadState,
}

impl KWeighting {
    fn process(&mut self, input: f64) -> f64 {
        // BS.1770 reference coefficients for 48 kHz.
        let shelved = self.shelf.process(
            input,
            [
                1.535_124_859_586_97,
                -2.691_696_189_406_38,
                1.198_392_810_852_85,
            ],
            [-1.690_659_293_182_41, 0.732_480_774_215_85],
        );
        self.high_pass.process(
            shelved,
            [1.0, -2.0, 1.0],
            [-1.990_047_454_833_98, 0.990_072_250_366_21],
        )
    }
}

/// Deterministic amplitude summary for one waveform display bucket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformPoint {
    /// Lowest sample value in the bucket.
    pub min: f32,
    /// Highest sample value in the bucket.
    pub max: f32,
    /// Root-mean-square level in the bucket.
    pub rms: f32,
}

impl WaveformPoint {
    /// Returns whether every amplitude summary is exactly silent.
    #[must_use]
    pub fn is_silent(self) -> bool {
        self.min == 0.0 && self.max == 0.0 && self.rms == 0.0
    }

    /// Returns the bucket peak in dBFS; silence is represented as negative
    /// infinity.
    #[must_use]
    pub fn peak_dbfs(self) -> f32 {
        let peak = self.min.abs().max(self.max.abs());
        if peak == 0.0 {
            f32::NEG_INFINITY
        } else {
            20.0 * peak.log10()
        }
    }
}

/// Deterministic left/right amplitude summary for one stereo display bucket.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StereoWaveformPoint {
    /// Left-channel waveform summary.
    pub left: WaveformPoint,
    /// Right-channel waveform summary.
    pub right: WaveformPoint,
}

/// Generates waveform buckets from interleaved stereo samples.
///
/// `frames_per_point` counts stereo frames rather than scalar samples.
///
/// # Errors
///
/// Returns [`AudioError::OddInterleavedBuffer`] for an incomplete frame,
/// [`AudioError::InvalidWaveformWindow`] for a zero bucket, or
/// [`AudioError::NonFiniteSample`] for NaN/infinite input.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn generate_stereo_waveform(
    samples: &[f32],
    frames_per_point: usize,
) -> Result<Vec<StereoWaveformPoint>, AudioError> {
    if !samples.len().is_multiple_of(2) {
        return Err(AudioError::OddInterleavedBuffer);
    }
    if frames_per_point == 0 {
        return Err(AudioError::InvalidWaveformWindow);
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(AudioError::NonFiniteSample);
    }
    let frame_count = samples.len() / 2;
    let mut points = Vec::with_capacity(frame_count.div_ceil(frames_per_point));
    for bucket in samples.chunks(frames_per_point * 2) {
        let (mut left_min, mut left_max, mut left_sum) =
            (f32::INFINITY, f32::NEG_INFINITY, 0.0_f64);
        let (mut right_min, mut right_max, mut right_sum) =
            (f32::INFINITY, f32::NEG_INFINITY, 0.0_f64);
        for frame in bucket.chunks_exact(2) {
            left_min = left_min.min(frame[0]);
            left_max = left_max.max(frame[0]);
            left_sum += f64::from(frame[0]) * f64::from(frame[0]);
            right_min = right_min.min(frame[1]);
            right_max = right_max.max(frame[1]);
            right_sum += f64::from(frame[1]) * f64::from(frame[1]);
        }
        let frames = bucket.len() / 2;
        points.push(StereoWaveformPoint {
            left: WaveformPoint {
                min: left_min,
                max: left_max,
                rms: (left_sum / frames as f64).sqrt() as f32,
            },
            right: WaveformPoint {
                min: right_min,
                max: right_max,
                rms: (right_sum / frames as f64).sqrt() as f32,
            },
        });
    }
    Ok(points)
}

/// Generates waveform buckets from decoded mono samples.
///
/// The final bucket may contain fewer samples than `samples_per_point`. The
/// function allocates only the returned point vector and performs no implicit
/// resampling or normalization, making the output stable for cache keys and
/// visual regression fixtures.
///
/// # Errors
///
/// Returns [`AudioError::InvalidWaveformWindow`] for zero-sized buckets or
/// [`AudioError::NonFiniteSample`] when decoded samples contain NaN/infinity.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
pub fn generate_waveform(
    samples: &[f32],
    samples_per_point: usize,
) -> Result<Vec<WaveformPoint>, AudioError> {
    if samples_per_point == 0 {
        return Err(AudioError::InvalidWaveformWindow);
    }
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(AudioError::NonFiniteSample);
    }
    let mut points = Vec::with_capacity(samples.len().div_ceil(samples_per_point));
    for bucket in samples.chunks(samples_per_point) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        let mut sum = 0.0_f64;
        for &sample in bucket {
            min = min.min(sample);
            max = max.max(sample);
            sum += f64::from(sample) * f64::from(sample);
        }
        points.push(WaveformPoint {
            min,
            max,
            rms: (sum / bucket.len() as f64).sqrt() as f32,
        });
    }
    Ok(points)
}

/// Allocation-free stereo gain and constant-power panning processor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StereoGainPan {
    gain: f32,
    pan: f32,
}

impl Default for StereoGainPan {
    fn default() -> Self {
        Self {
            gain: 1.0,
            pan: 0.0,
        }
    }
}

impl StereoGainPan {
    /// Creates a neutral processor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the per-channel gains used by processing and mixing.
    ///
    /// Export and realtime callers share this mapping so pan-law semantics do
    /// not drift between pipelines.
    #[must_use]
    pub fn channel_gains(self) -> (f32, f32) {
        self.coefficients()
    }

    /// Sets a linear gain multiplier.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidGain`] for NaN or infinite values.
    pub fn set_gain(&mut self, gain: f32) -> Result<(), AudioError> {
        if !gain.is_finite() {
            return Err(AudioError::InvalidGain);
        }
        self.gain = gain;
        Ok(())
    }

    /// Sets normalized pan, where -1 is left and 1 is right.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidPan`] when outside the normalized range.
    pub fn set_pan(&mut self, pan: f32) -> Result<(), AudioError> {
        if !(-1.0..=1.0).contains(&pan) {
            return Err(AudioError::InvalidPan);
        }
        self.pan = pan;
        Ok(())
    }

    /// Processes interleaved stereo samples in place without allocation.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::OddInterleavedBuffer`] if the buffer is malformed.
    pub fn process(&self, samples: &mut [f32]) -> Result<(), AudioError> {
        if !samples.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        let (left, right) = self.coefficients();
        for frame in samples.chunks_exact_mut(2) {
            frame[0] *= left;
            frame[1] *= right;
        }
        Ok(())
    }

    fn coefficients(self) -> (f32, f32) {
        let angle = (self.pan + 1.0) * FRAC_PI_4;
        (self.gain * angle.cos(), self.gain * angle.sin())
    }
}

/// One interleaved stereo source in a realtime mix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StereoTrack<'a> {
    /// Source samples. The slice is borrowed for the duration of the mix.
    pub samples: &'a [f32],
    /// Per-track gain and pan processing.
    pub processor: StereoGainPan,
}

/// A stereo source with an authoritative mute state for timeline mixing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoutedStereoTrack<'a> {
    /// Source samples. The slice is borrowed for the duration of the mix.
    pub samples: &'a [f32],
    /// Per-track gain and pan processing.
    pub processor: StereoGainPan,
    /// Muted sources are validated but contribute no samples to the output.
    pub muted: bool,
}

/// A caller-owned stereo bus buffer with final processing controls.
pub struct AudioBus<'a> {
    /// Interleaved stereo samples accumulated for this bus.
    pub samples: &'a mut [f32],
    /// Gain/pan applied after routed tracks are accumulated.
    pub processor: StereoGainPan,
    /// Muted buses contribute no samples to the master output.
    pub muted: bool,
}

/// One track route into an indexed audio bus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BusRoute<'a> {
    /// Source samples for the route.
    pub samples: &'a [f32],
    /// Per-route gain/pan processing.
    pub processor: StereoGainPan,
    /// Destination bus index.
    pub bus_index: usize,
    /// Muted routes are validated but contribute no samples.
    pub muted: bool,
}

/// Allocation-free multibus mixer with a final master output.
pub struct AudioBusMixer;

impl AudioBusMixer {
    /// Routes tracks into caller-provided buses and sums buses into `output`.
    ///
    /// All buffers must be equal-length interleaved stereo. Validation occurs
    /// before any buffer is modified, and processing performs no allocation or
    /// locking.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::OddInterleavedBuffer`],
    /// [`AudioError::MismatchedBufferLength`], or
    /// [`AudioError::InvalidBusIndex`] for malformed graph inputs.
    pub fn mix(
        routes: &[BusRoute<'_>],
        buses: &mut [AudioBus<'_>],
        output: &mut [f32],
    ) -> Result<(), AudioError> {
        if !output.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        for bus in buses.iter() {
            if !bus.samples.len().is_multiple_of(2) {
                return Err(AudioError::OddInterleavedBuffer);
            }
            if bus.samples.len() != output.len() {
                return Err(AudioError::MismatchedBufferLength);
            }
        }
        for route in routes {
            if !route.samples.len().is_multiple_of(2) {
                return Err(AudioError::OddInterleavedBuffer);
            }
            if route.samples.len() != output.len() {
                return Err(AudioError::MismatchedBufferLength);
            }
            if route.bus_index >= buses.len() {
                return Err(AudioError::InvalidBusIndex);
            }
        }
        for bus in buses.iter_mut() {
            bus.samples.fill(0.0);
        }
        for route in routes {
            if route.muted {
                continue;
            }
            let (left, right) = route.processor.coefficients();
            let bus = &mut buses[route.bus_index];
            for (destination, source) in bus
                .samples
                .chunks_exact_mut(2)
                .zip(route.samples.chunks_exact(2))
            {
                destination[0] += source[0] * left;
                destination[1] += source[1] * right;
            }
        }
        output.fill(0.0);
        for bus in buses.iter() {
            if bus.muted {
                continue;
            }
            let (left, right) = bus.processor.coefficients();
            for (destination, source) in output.chunks_exact_mut(2).zip(bus.samples.chunks_exact(2))
            {
                destination[0] += source[0] * left;
                destination[1] += source[1] * right;
            }
        }
        Ok(())
    }
}

/// Allocation-free stereo fade and equal-power crossfade operations.
pub struct AudioFades;

impl AudioFades {
    /// Applies a linear gain ramp to an interleaved stereo buffer in place.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::OddInterleavedBuffer`],
    /// [`AudioError::InvalidFadeGain`], or [`AudioError::NonFiniteSample`].
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub fn linear(samples: &mut [f32], start_gain: f32, end_gain: f32) -> Result<(), AudioError> {
        if !samples.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        if !start_gain.is_finite() || !end_gain.is_finite() {
            return Err(AudioError::InvalidFadeGain);
        }
        let frames = samples.len() / 2;
        let denominator = frames.saturating_sub(1).max(1) as f32;
        for (index, frame) in samples.chunks_exact_mut(2).enumerate() {
            if !frame[0].is_finite() || !frame[1].is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
            let progress = index as f32 / denominator;
            let gain = start_gain + (end_gain - start_gain) * progress;
            frame[0] *= gain;
            frame[1] *= gain;
        }
        Ok(())
    }

    /// Blends equal-length stereo buffers using an equal-power curve.
    ///
    /// `progress = 0` selects `outgoing`; `progress = 1` selects `incoming`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidCrossfadeProgress`],
    /// [`AudioError::OddInterleavedBuffer`],
    /// [`AudioError::MismatchedBufferLength`], or
    /// [`AudioError::NonFiniteSample`].
    pub fn equal_power(
        outgoing: &[f32],
        incoming: &[f32],
        progress: f32,
        output: &mut [f32],
    ) -> Result<(), AudioError> {
        if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
            return Err(AudioError::InvalidCrossfadeProgress);
        }
        if !outgoing.len().is_multiple_of(2)
            || !incoming.len().is_multiple_of(2)
            || !output.len().is_multiple_of(2)
        {
            return Err(AudioError::OddInterleavedBuffer);
        }
        if outgoing.len() != incoming.len() || outgoing.len() != output.len() {
            return Err(AudioError::MismatchedBufferLength);
        }
        let angle = progress * std::f32::consts::FRAC_PI_2;
        let outgoing_gain = angle.cos();
        let incoming_gain = angle.sin();
        for ((destination, outgoing), incoming) in output.iter_mut().zip(outgoing).zip(incoming) {
            if !outgoing.is_finite() || !incoming.is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
            *destination = *outgoing * outgoing_gain + *incoming * incoming_gain;
        }
        Ok(())
    }
}

/// Filter response supported by [`OnePoleFilter`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterKind {
    /// Attenuates content above the cutoff.
    LowPass,
    /// Attenuates content below the cutoff.
    HighPass,
}

/// Stateful first-order stereo low/high-pass filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OnePoleFilter {
    kind: FilterKind,
    alpha: f32,
    previous_input_left: f32,
    previous_input_right: f32,
    previous_output_left: f32,
    previous_output_right: f32,
}

impl OnePoleFilter {
    /// Creates a filter with a cutoff strictly below Nyquist.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidFilterParameter`] for zero rates,
    /// non-finite values, or a cutoff outside `(0, sample_rate / 2)`.
    #[allow(clippy::cast_precision_loss)]
    pub fn new(kind: FilterKind, sample_rate: u32, cutoff_hz: f32) -> Result<Self, AudioError> {
        let nyquist = sample_rate as f32 * 0.5;
        if sample_rate == 0 || !cutoff_hz.is_finite() || cutoff_hz <= 0.0 || cutoff_hz >= nyquist {
            return Err(AudioError::InvalidFilterParameter);
        }
        let alpha = (-2.0 * std::f32::consts::PI * cutoff_hz / sample_rate as f32).exp();
        Ok(Self {
            kind,
            alpha,
            previous_input_left: 0.0,
            previous_input_right: 0.0,
            previous_output_left: 0.0,
            previous_output_right: 0.0,
        })
    }

    /// Resets state at a stream boundary.
    pub fn reset(&mut self) {
        self.previous_input_left = 0.0;
        self.previous_input_right = 0.0;
        self.previous_output_left = 0.0;
        self.previous_output_right = 0.0;
    }

    /// Processes interleaved stereo samples in place without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::OddInterleavedBuffer`] or
    /// [`AudioError::NonFiniteSample`] for malformed input.
    pub fn process(&mut self, samples: &mut [f32]) -> Result<(), AudioError> {
        if !samples.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        for frame in samples.chunks_exact_mut(2) {
            if !frame[0].is_finite() || !frame[1].is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
            let output_left = Self::process_channel(
                self.kind,
                self.alpha,
                frame[0],
                &mut self.previous_input_left,
                &mut self.previous_output_left,
            );
            let output_right = Self::process_channel(
                self.kind,
                self.alpha,
                frame[1],
                &mut self.previous_input_right,
                &mut self.previous_output_right,
            );
            frame[0] = output_left;
            frame[1] = output_right;
        }
        Ok(())
    }

    fn process_channel(
        kind: FilterKind,
        alpha: f32,
        input: f32,
        previous_input: &mut f32,
        previous_output: &mut f32,
    ) -> f32 {
        let low = (1.0 - alpha) * input + alpha * *previous_output;
        let output = match kind {
            FilterKind::LowPass => low,
            FilterKind::HighPass => alpha * (*previous_output + input - *previous_input),
        };
        *previous_input = input;
        *previous_output = low;
        output
    }
}

/// Stateful stereo feedback delay. The delay line is allocated at creation;
/// block processing itself performs no allocation.
#[derive(Clone, Debug, PartialEq)]
pub struct StereoDelay {
    buffer: Vec<f32>,
    cursor: usize,
    feedback: f32,
    mix: f32,
}

impl StereoDelay {
    /// Creates a delay from milliseconds at the given sample rate.
    ///
    /// Delay time must be in `(0, 2_000]` milliseconds, feedback and wet mix
    /// must be in `[0, 1]`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidDelayParameter`] for invalid rates,
    /// delay, feedback, mix, or an overflowing delay-line size.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn new(
        sample_rate: u32,
        delay_ms: f32,
        feedback: f32,
        mix: f32,
    ) -> Result<Self, AudioError> {
        if sample_rate == 0
            || !delay_ms.is_finite()
            || !(0.0..=2_000.0).contains(&delay_ms)
            || delay_ms == 0.0
            || !feedback.is_finite()
            || !(0.0..=1.0).contains(&feedback)
            || !mix.is_finite()
            || !(0.0..=1.0).contains(&mix)
        {
            return Err(AudioError::InvalidDelayParameter);
        }
        let frames = (delay_ms * sample_rate as f32 / 1_000.0).round() as usize;
        let samples = frames
            .checked_mul(2)
            .ok_or(AudioError::InvalidDelayParameter)?
            .max(2);
        Ok(Self {
            buffer: vec![0.0; samples],
            cursor: 0,
            feedback,
            mix,
        })
    }

    /// Resets the delay line at a stream boundary.
    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.cursor = 0;
    }

    /// Processes interleaved stereo samples in place without allocation.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::OddInterleavedBuffer`] for malformed stereo
    /// buffers or [`AudioError::NonFiniteSample`] for invalid samples.
    pub fn process(&mut self, samples: &mut [f32]) -> Result<(), AudioError> {
        if !samples.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        for frame in samples.chunks_exact_mut(2) {
            if !frame[0].is_finite() || !frame[1].is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
            let delayed_left = self.buffer[self.cursor];
            let delayed_right = self.buffer[self.cursor + 1];
            self.buffer[self.cursor] = frame[0] + delayed_left * self.feedback;
            self.buffer[self.cursor + 1] = frame[1] + delayed_right * self.feedback;
            frame[0] = frame[0] * (1.0 - self.mix) + delayed_left * self.mix;
            frame[1] = frame[1] * (1.0 - self.mix) + delayed_right * self.mix;
            self.cursor = (self.cursor + 2) % self.buffer.len();
        }
        Ok(())
    }
}

/// Allocation-free multitrack stereo mixer.
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioMixer;

impl AudioMixer {
    /// Mixes equal-length interleaved stereo sources into `output`.
    ///
    /// The output is cleared before accumulation. No allocations, locks, or
    /// callbacks occur in this method, so callers can use it from a dedicated
    /// realtime render thread after validating their buffer sizes.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::OddInterleavedBuffer`] for malformed stereo
    /// buffers and [`AudioError::MismatchedBufferLength`] when a source does
    /// not match the output length.
    pub fn mix(tracks: &[StereoTrack<'_>], output: &mut [f32]) -> Result<(), AudioError> {
        if !output.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        for track in tracks {
            if !track.samples.len().is_multiple_of(2) {
                return Err(AudioError::OddInterleavedBuffer);
            }
            if track.samples.len() != output.len() {
                return Err(AudioError::MismatchedBufferLength);
            }
        }
        output.fill(0.0);
        for track in tracks {
            let (left, right) = track.processor.coefficients();
            for (out, input) in output
                .chunks_exact_mut(2)
                .zip(track.samples.chunks_exact(2))
            {
                out[0] += input[0] * left;
                out[1] += input[1] * right;
            }
        }
        Ok(())
    }

    /// Mixes routed stereo sources while honoring per-track mute state.
    ///
    /// Muted tracks still undergo shape validation so malformed timeline
    /// buffers fail deterministically rather than being silently hidden.
    /// Processing remains allocation-free and does not acquire locks.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::OddInterleavedBuffer`] for malformed stereo
    /// buffers and [`AudioError::MismatchedBufferLength`] for differing frame
    /// counts.
    pub fn mix_routed(
        tracks: &[RoutedStereoTrack<'_>],
        output: &mut [f32],
    ) -> Result<(), AudioError> {
        if !output.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        for track in tracks {
            if !track.samples.len().is_multiple_of(2) {
                return Err(AudioError::OddInterleavedBuffer);
            }
            if track.samples.len() != output.len() {
                return Err(AudioError::MismatchedBufferLength);
            }
        }
        output.fill(0.0);
        for track in tracks {
            if track.muted {
                continue;
            }
            let (left, right) = track.processor.coefficients();
            for (out, input) in output
                .chunks_exact_mut(2)
                .zip(track.samples.chunks_exact(2))
            {
                out[0] += input[0] * left;
                out[1] += input[1] * right;
            }
        }
        Ok(())
    }
}

/// Allocation-free linear interpolating resampler for one audio channel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinearResampler {
    source_rate: u32,
    target_rate: u32,
}

impl LinearResampler {
    /// Creates a block resampler with explicit source and target rates.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidSampleRate`] for a zero rate.
    pub fn new(source_rate: u32, target_rate: u32) -> Result<Self, AudioError> {
        if source_rate == 0 || target_rate == 0 {
            return Err(AudioError::InvalidSampleRate);
        }
        Ok(Self {
            source_rate,
            target_rate,
        })
    }

    /// Resamples a block into caller-provided storage without allocating.
    ///
    /// The returned count may be lower than `output.len()` when the input
    /// block does not contain enough samples for the next interpolation point.
    /// A production streaming adapter must carry the final source sample and
    /// phase into the next block.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::NonFiniteSample`] when any input sample is not
    /// finite.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn process(&self, input: &[f32], output: &mut [f32]) -> Result<usize, AudioError> {
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioError::NonFiniteSample);
        }
        if input.len() < 2 || output.is_empty() {
            return Ok(0);
        }
        let step = f64::from(self.source_rate) / f64::from(self.target_rate);
        let input_len = input.len() as f64;
        let mut position = 0.0_f64;
        let mut written = 0;
        while written < output.len() && position + 1.0 < input_len {
            let index = position.floor() as usize;
            let fraction = (position - index as f64) as f32;
            output[written] = input[index] + (input[index + 1] - input[index]) * fraction;
            written += 1;
            position += step;
        }
        Ok(written)
    }
}

/// Stateful block adapter that preserves one-sample history and source phase.
#[derive(Clone, Debug, PartialEq)]
pub struct StreamingLinearResampler {
    source_rate: u32,
    target_rate: u32,
    next_position: f64,
    source_offset: usize,
    previous_sample: Option<f32>,
}

impl StreamingLinearResampler {
    /// Creates a streaming resampler with explicit source and target rates.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidSampleRate`] for a zero rate.
    pub fn new(source_rate: u32, target_rate: u32) -> Result<Self, AudioError> {
        if source_rate == 0 || target_rate == 0 {
            return Err(AudioError::InvalidSampleRate);
        }
        Ok(Self {
            source_rate,
            target_rate,
            next_position: 0.0,
            source_offset: 0,
            previous_sample: None,
        })
    }

    /// Resets phase and boundary history to the start of a new stream.
    pub fn reset(&mut self) {
        self.next_position = 0.0;
        self.source_offset = 0;
        self.previous_sample = None;
    }

    /// Processes one mono block while preserving continuity across calls.
    ///
    /// The caller must provide enough output space for the desired block; any
    /// samples that do not fit are not retained for a later call.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::NonFiniteSample`] for invalid input or
    /// [`AudioError::SampleCountOverflow`] if the stream position overflows.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<usize, AudioError> {
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioError::NonFiniteSample);
        }
        if input.is_empty() || output.is_empty() {
            return Ok(0);
        }
        let block_start = if self.previous_sample.is_some() {
            self.source_offset.saturating_sub(1)
        } else {
            self.source_offset
        };
        let block_end = self
            .source_offset
            .checked_add(input.len())
            .ok_or(AudioError::SampleCountOverflow)?;
        let step = f64::from(self.source_rate) / f64::from(self.target_rate);
        let mut written = 0;
        while written < output.len() && self.next_position + 1.0 < block_end as f64 {
            if self.next_position < block_start as f64 {
                self.next_position = block_start as f64;
            }
            let index = self.next_position.floor() as usize;
            let fraction = (self.next_position - index as f64) as f32;
            let left = if self.previous_sample.is_some() && index == self.source_offset - 1 {
                if let Some(previous) = self.previous_sample {
                    previous
                } else {
                    return Err(AudioError::SampleCountOverflow);
                }
            } else {
                input[index - self.source_offset]
            };
            let right = input[index + 1 - self.source_offset];
            output[written] = left + (right - left) * fraction;
            written += 1;
            self.next_position += step;
        }
        self.previous_sample = input.last().copied();
        self.source_offset = block_end;
        Ok(written)
    }
}

/// Peak and RMS measurements for an interleaved sample buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioAnalysis {
    /// Maximum absolute sample value.
    pub peak: f32,
    /// Root-mean-square sample level.
    pub rms: f32,
}

impl AudioAnalysis {
    /// Returns whether the analyzed signal contains no measurable amplitude.
    #[must_use]
    pub fn is_silent(self) -> bool {
        self.peak == 0.0 && self.rms == 0.0
    }

    /// Analyzes samples without allocating.
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub fn analyze(samples: &[f32]) -> Self {
        if samples.is_empty() {
            return Self {
                peak: 0.0,
                rms: 0.0,
            };
        }
        let mut peak = 0.0_f32;
        let mut sum = 0.0_f64;
        for &sample in samples {
            peak = peak.max(sample.abs());
            sum += f64::from(sample) * f64::from(sample);
        }
        Self {
            peak,
            rms: (sum / samples.len() as f64).sqrt() as f32,
        }
    }

    /// Analyzes samples while rejecting NaN and infinite input.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::NonFiniteSample`] when any sample is invalid.
    pub fn try_analyze(samples: &[f32]) -> Result<Self, AudioError> {
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioError::NonFiniteSample);
        }
        Ok(Self::analyze(samples))
    }

    /// Returns the peak level in dBFS. Silence is negative infinity.
    #[must_use]
    pub fn peak_dbfs(self) -> f32 {
        amplitude_to_dbfs(self.peak)
    }

    /// Returns the RMS level in dBFS. Silence is negative infinity.
    #[must_use]
    pub fn rms_dbfs(self) -> f32 {
        amplitude_to_dbfs(self.rms)
    }
}

/// Stateful stereo peak meter suitable for realtime UI updates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StereoMeter {
    left_peak: f32,
    right_peak: f32,
    left_hold: f32,
    right_hold: f32,
    left_rms: f32,
    right_rms: f32,
    clipped: bool,
}

/// A stable snapshot of the current stereo meter state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StereoMeterSnapshot {
    /// Current left-channel peak amplitude.
    pub left_peak: f32,
    /// Current right-channel peak amplitude.
    pub right_peak: f32,
    /// Peak-hold left-channel amplitude.
    pub left_hold: f32,
    /// Peak-hold right-channel amplitude.
    pub right_hold: f32,
    /// Current left-channel RMS amplitude.
    pub left_rms: f32,
    /// Current right-channel RMS amplitude.
    pub right_rms: f32,
    /// Whether any sample has clipped since the last reset.
    pub clipped: bool,
}

impl StereoMeterSnapshot {
    /// Current left-channel peak in dBFS; silence is negative infinity.
    #[must_use]
    pub fn left_peak_dbfs(self) -> f32 {
        amplitude_to_dbfs(self.left_peak)
    }

    /// Current right-channel peak in dBFS; silence is negative infinity.
    #[must_use]
    pub fn right_peak_dbfs(self) -> f32 {
        amplitude_to_dbfs(self.right_peak)
    }

    /// Current left-channel RMS in dBFS; silence is negative infinity.
    #[must_use]
    pub fn left_rms_dbfs(self) -> f32 {
        amplitude_to_dbfs(self.left_rms)
    }

    /// Current right-channel RMS in dBFS; silence is negative infinity.
    #[must_use]
    pub fn right_rms_dbfs(self) -> f32 {
        amplitude_to_dbfs(self.right_rms)
    }
}

impl StereoMeter {
    /// Updates the meter from one interleaved stereo block.
    ///
    /// # Errors
    ///
    /// Returns an error for odd interleaved buffers or non-finite samples.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    pub fn update(&mut self, samples: &[f32]) -> Result<(), AudioError> {
        if !samples.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(AudioError::NonFiniteSample);
        }
        if samples.is_empty() {
            self.left_peak = 0.0;
            self.right_peak = 0.0;
            self.left_rms = 0.0;
            self.right_rms = 0.0;
            return Ok(());
        }
        let mut left_sum = 0.0_f64;
        let mut right_sum = 0.0_f64;
        self.left_peak = 0.0;
        self.right_peak = 0.0;
        for frame in samples.chunks_exact(2) {
            self.left_peak = self.left_peak.max(frame[0].abs());
            self.right_peak = self.right_peak.max(frame[1].abs());
            left_sum += f64::from(frame[0]) * f64::from(frame[0]);
            right_sum += f64::from(frame[1]) * f64::from(frame[1]);
            self.clipped |= frame[0].abs() >= 1.0 || frame[1].abs() >= 1.0;
        }
        let frame_count = (samples.len() / 2) as f64;
        self.left_rms = (left_sum / frame_count).sqrt() as f32;
        self.right_rms = (right_sum / frame_count).sqrt() as f32;
        self.left_hold = self.left_hold.max(self.left_peak);
        self.right_hold = self.right_hold.max(self.right_peak);
        Ok(())
    }

    /// Clears peak-hold and clipping indicators without changing current levels.
    pub fn reset_peaks(&mut self) {
        self.left_hold = self.left_peak;
        self.right_hold = self.right_peak;
        self.clipped = false;
    }

    /// Returns a copy of the current meter state.
    #[must_use]
    pub const fn snapshot(self) -> StereoMeterSnapshot {
        StereoMeterSnapshot {
            left_peak: self.left_peak,
            right_peak: self.right_peak,
            left_hold: self.left_hold,
            right_hold: self.right_hold,
            left_rms: self.left_rms,
            right_rms: self.right_rms,
            clipped: self.clipped,
        }
    }
}

fn amplitude_to_dbfs(amplitude: f32) -> f32 {
    if amplitude == 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * amplitude.abs().log10()
    }
}

/// Deterministic peak normalizer for a decoded audio block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakNormalizer {
    target_peak: f32,
}

/// Stateful feed-forward compressor for interleaved mono or stereo samples.
///
/// The detector follows the absolute peak with independent attack and release
/// smoothing. Gain reduction is calculated in decibels and applied uniformly
/// to each interleaved frame, so channel balance is preserved. The processor
/// is allocation-free and can be reused across realtime blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicsCompressor {
    threshold_db: f32,
    ratio: f32,
    attack_coefficient: f32,
    release_coefficient: f32,
    makeup_gain: f32,
    envelope: f32,
}

impl DynamicsCompressor {
    /// Creates a compressor with dB threshold/ratio and millisecond timing.
    ///
    /// A ratio of `1.0` is transparent. Zero attack or release means the
    /// detector responds immediately for that direction.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidDynamicsParameter`] for invalid rates,
    /// non-finite values, negative timings, or a ratio below one.
    #[allow(clippy::cast_precision_loss)]
    pub fn new(
        sample_rate: u32,
        threshold_db: f32,
        ratio: f32,
        attack_ms: f32,
        release_ms: f32,
        makeup_db: f32,
    ) -> Result<Self, AudioError> {
        if sample_rate == 0
            || !threshold_db.is_finite()
            || !ratio.is_finite()
            || ratio < 1.0
            || !attack_ms.is_finite()
            || attack_ms < 0.0
            || !release_ms.is_finite()
            || release_ms < 0.0
            || !makeup_db.is_finite()
        {
            return Err(AudioError::InvalidDynamicsParameter);
        }
        let rate = sample_rate as f32;
        Ok(Self {
            threshold_db,
            ratio,
            attack_coefficient: time_coefficient(attack_ms, rate),
            release_coefficient: time_coefficient(release_ms, rate),
            makeup_gain: 10.0_f32.powf(makeup_db / 20.0),
            envelope: 0.0,
        })
    }

    /// Resets detector history at a stream boundary.
    pub fn reset(&mut self) {
        self.envelope = 0.0;
    }

    /// Processes an interleaved buffer in place without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::NonFiniteSample`] for NaN or infinite input.
    pub fn process(&mut self, samples: &mut [f32]) -> Result<(), AudioError> {
        if !samples.len().is_multiple_of(2) {
            return Err(AudioError::OddInterleavedBuffer);
        }
        for frame in samples.chunks_exact_mut(2) {
            if !frame[0].is_finite() || !frame[1].is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
            let peak = frame[0].abs().max(frame[1].abs());
            let coefficient = if peak > self.envelope {
                self.attack_coefficient
            } else {
                self.release_coefficient
            };
            self.envelope = peak + coefficient * (self.envelope - peak);
            let envelope_db = 20.0 * self.envelope.max(1.0e-12).log10();
            let over_db = (envelope_db - self.threshold_db).max(0.0);
            let reduction_db = over_db * (1.0 - 1.0 / self.ratio);
            let gain = self.makeup_gain * 10.0_f32.powf(-reduction_db / 20.0);
            frame[0] *= gain;
            frame[1] *= gain;
        }
        Ok(())
    }
}

fn time_coefficient(milliseconds: f32, sample_rate: f32) -> f32 {
    if milliseconds == 0.0 {
        0.0
    } else {
        (-1.0 / (milliseconds * 0.001 * sample_rate)).exp()
    }
}

/// Allocation-free hard limiter that constrains every interleaved sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limiter {
    ceiling: f32,
}

impl Limiter {
    /// Creates a limiter with a positive linear ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidTargetPeak`] for non-finite or
    /// non-positive ceilings.
    pub fn new(ceiling: f32) -> Result<Self, AudioError> {
        if !ceiling.is_finite() || ceiling <= 0.0 {
            return Err(AudioError::InvalidTargetPeak);
        }
        Ok(Self { ceiling })
    }

    /// Clamps samples to the configured ceiling without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::NonFiniteSample`] when an input is not finite.
    pub fn process(&self, samples: &mut [f32]) -> Result<(), AudioError> {
        for sample in samples {
            if !sample.is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
            *sample = sample.clamp(-self.ceiling, self.ceiling);
        }
        Ok(())
    }
}

impl PeakNormalizer {
    /// Creates a normalizer targeting a positive linear peak value.
    ///
    /// This is peak normalization, not an LUFS measurement or loudness
    /// standard implementation. It is suitable as a building block for a
    /// later loudness meter that can account for windows and channel weights.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidTargetPeak`] for non-positive or
    /// non-finite targets.
    pub fn new(target_peak: f32) -> Result<Self, AudioError> {
        if !target_peak.is_finite() || target_peak <= 0.0 {
            return Err(AudioError::InvalidTargetPeak);
        }
        Ok(Self { target_peak })
    }

    /// Scales a block in place so its maximum absolute sample reaches target.
    ///
    /// Silence is left unchanged. The method performs no allocation.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::NonFiniteSample`] if the input contains NaN or an
    /// infinite value.
    pub fn process(&self, samples: &mut [f32]) -> Result<(), AudioError> {
        let mut peak = 0.0_f32;
        for &sample in samples.iter() {
            if !sample.is_finite() {
                return Err(AudioError::NonFiniteSample);
            }
            peak = peak.max(sample.abs());
        }
        if peak == 0.0 {
            return Ok(());
        }
        let gain = self.target_peak / peak;
        for sample in samples {
            *sample *= gain;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
