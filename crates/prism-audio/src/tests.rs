use super::{
    AudioAnalysis, AudioBus, AudioBusMixer, AudioError, AudioFades, AudioMixer, BusRoute,
    DynamicsCompressor, FilterKind, Limiter, LinearResampler, OnePoleFilter, PeakNormalizer,
    RoutedStereoTrack, StereoDelay, StereoGainPan, StereoMeter, StereoTrack,
    StreamingLinearResampler, WaveformPoint, generate_stereo_waveform, generate_waveform,
    measure_integrated_loudness,
};

#[test]
fn neutral_processor_preserves_stereo_energy() -> Result<(), AudioError> {
    let mut samples = [1.0, 0.5, -0.25, 0.75];
    StereoGainPan::new().process(&mut samples)?;
    assert!((samples[0] - 0.707_106_77).abs() < 1e-6);
    assert!((samples[1] - 0.353_553_38).abs() < 1e-6);
    Ok(())
}

#[test]
fn stereo_meter_tracks_channels_hold_and_clipping() -> Result<(), AudioError> {
    let mut meter = StereoMeter::default();
    meter.update(&[0.25, -0.5, 1.1, 0.75])?;
    let snapshot = meter.snapshot();
    assert!((snapshot.left_peak - 1.1).abs() < f32::EPSILON);
    assert!((snapshot.right_peak - 0.75).abs() < f32::EPSILON);
    assert!((snapshot.left_hold - 1.1).abs() < f32::EPSILON);
    assert!(snapshot.left_peak_dbfs() > 0.0);
    assert!(snapshot.right_peak_dbfs() < 0.0);
    assert!(snapshot.clipped);
    meter.update(&[0.1, 0.2])?;
    assert!((meter.snapshot().left_hold - 1.1).abs() < f32::EPSILON);
    meter.reset_peaks();
    assert!(!meter.snapshot().clipped);
    assert!((meter.snapshot().left_hold - 0.1).abs() < f32::EPSILON);
    assert_eq!(meter.update(&[0.1]), Err(AudioError::OddInterleavedBuffer));
    Ok(())
}

#[test]
fn pan_and_validation_are_explicit() -> Result<(), AudioError> {
    let mut processor = StereoGainPan::new();
    assert_eq!(processor.set_pan(2.0), Err(AudioError::InvalidPan));
    processor.set_pan(-1.0)?;
    let mut samples = [1.0, 1.0];
    processor.process(&mut samples)?;
    assert!(samples[0] > 0.99 && samples[1].abs() < 1e-6);
    assert_eq!(
        processor.process(&mut [1.0]),
        Err(AudioError::OddInterleavedBuffer)
    );
    Ok(())
}

#[test]
fn analysis_reports_peak_and_rms() {
    let result = AudioAnalysis::analyze(&[1.0, -1.0, 0.0, 0.0]);
    assert!((result.peak - 1.0).abs() < f32::EPSILON);
    assert!((result.rms - 0.707_106_77).abs() < 1e-6);
    assert!(result.peak_dbfs().abs() < f32::EPSILON);
    assert!((result.rms_dbfs() + 3.010_3).abs() < 1e-3);
    assert!(AudioAnalysis::analyze(&[]).rms.abs() < f32::EPSILON);
    let silence_dbfs = AudioAnalysis::analyze(&[]).rms_dbfs();
    assert!(silence_dbfs.is_infinite() && silence_dbfs.is_sign_negative());
    assert_eq!(
        AudioAnalysis::try_analyze(&[f32::NAN]),
        Err(AudioError::NonFiniteSample)
    );
}

#[test]
fn mixer_accumulates_tracks_without_allocating() -> Result<(), AudioError> {
    let left = [1.0, 0.0, 1.0, 0.0];
    let right = [0.0, 1.0, 0.0, 1.0];
    let tracks = [
        StereoTrack {
            samples: &left,
            processor: StereoGainPan::new(),
        },
        StereoTrack {
            samples: &right,
            processor: StereoGainPan::new(),
        },
    ];
    let mut output = [0.0; 4];
    AudioMixer::mix(&tracks, &mut output)?;
    assert!((output[0] - 0.707_106_77).abs() < 1e-6);
    assert!((output[1] - 0.707_106_77).abs() < 1e-6);
    assert_eq!(
        AudioMixer::mix(&tracks, &mut [0.0, 0.0]),
        Err(AudioError::MismatchedBufferLength)
    );
    Ok(())
}

#[test]
fn routed_mixer_skips_muted_tracks_but_validates_shapes() -> Result<(), AudioError> {
    let audible = [0.5, 0.5, 0.5, 0.5];
    let muted = [1.0, 1.0, 1.0, 1.0];
    let tracks = [
        RoutedStereoTrack {
            samples: &audible,
            processor: StereoGainPan::new(),
            muted: false,
        },
        RoutedStereoTrack {
            samples: &muted,
            processor: StereoGainPan::new(),
            muted: true,
        },
    ];
    let mut output = [0.0; 4];
    AudioMixer::mix_routed(&tracks, &mut output)?;
    assert!((output[0] - 0.353_553_38).abs() < 1e-6);
    assert!((output[1] - 0.353_553_38).abs() < 1e-6);
    assert_eq!(
        AudioMixer::mix_routed(
            &[RoutedStereoTrack {
                samples: &[1.0],
                processor: StereoGainPan::new(),
                muted: true,
            }],
            &mut output,
        ),
        Err(AudioError::OddInterleavedBuffer)
    );
    Ok(())
}

#[test]
fn bus_mixer_routes_tracks_and_applies_bus_controls() -> Result<(), AudioError> {
    let source = [1.0, 0.0, 0.5, 0.5];
    let muted = [2.0, 2.0, 2.0, 2.0];
    let mut bus_samples = [0.0; 4];
    let mut output = [0.0; 4];
    let routes = [
        BusRoute {
            samples: &source,
            processor: StereoGainPan::new(),
            bus_index: 0,
            muted: false,
        },
        BusRoute {
            samples: &muted,
            processor: StereoGainPan::new(),
            bus_index: 0,
            muted: true,
        },
    ];
    let mut buses = [AudioBus {
        samples: &mut bus_samples,
        processor: StereoGainPan::new(),
        muted: false,
    }];
    AudioBusMixer::mix(&routes, &mut buses, &mut output)?;
    assert!((output[0] - 0.5).abs() < 1e-6);
    assert!(output[1].abs() < 1e-6);
    assert_eq!(
        AudioBusMixer::mix(
            &[BusRoute {
                samples: &source,
                processor: StereoGainPan::new(),
                bus_index: 1,
                muted: false,
            }],
            &mut buses,
            &mut output,
        ),
        Err(AudioError::InvalidBusIndex)
    );
    Ok(())
}

#[test]
fn fades_and_crossfades_preserve_shape_and_use_equal_power() -> Result<(), AudioError> {
    let mut samples = [1.0, 1.0, 1.0, 1.0];
    AudioFades::linear(&mut samples, 0.0, 1.0)?;
    assert!(samples[0].abs() < f32::EPSILON);
    assert!((samples[2] - 1.0).abs() < f32::EPSILON);
    let outgoing = [1.0, 0.0];
    let incoming = [0.0, 1.0];
    let mut output = [0.0; 2];
    AudioFades::equal_power(&outgoing, &incoming, 0.5, &mut output)?;
    assert!((output[0] - 0.707_106_77).abs() < 1e-6);
    assert!((output[1] - 0.707_106_77).abs() < 1e-6);
    assert_eq!(
        AudioFades::equal_power(&outgoing, &[], 0.5, &mut output),
        Err(AudioError::MismatchedBufferLength)
    );
    assert_eq!(
        AudioFades::linear(&mut [1.0], 0.0, 1.0),
        Err(AudioError::OddInterleavedBuffer)
    );
    Ok(())
}

#[test]
fn one_pole_filters_validate_and_retain_block_state() -> Result<(), AudioError> {
    assert_eq!(
        OnePoleFilter::new(FilterKind::LowPass, 48_000, 24_000.0),
        Err(AudioError::InvalidFilterParameter)
    );
    let mut low = OnePoleFilter::new(FilterKind::LowPass, 48_000, 1_000.0)?;
    let mut step = [1.0, 1.0, 1.0, 1.0];
    low.process(&mut step)?;
    assert!(step[0] < step[2] && step[2] < 1.0);
    let mut high = OnePoleFilter::new(FilterKind::HighPass, 48_000, 1_000.0)?;
    let mut constant = [1.0, 1.0, 1.0, 1.0];
    high.process(&mut constant)?;
    assert!(constant[2].abs() < constant[0].abs());
    high.reset();
    assert_eq!(
        high.process(&mut [f32::NAN, 0.0]),
        Err(AudioError::NonFiniteSample)
    );
    Ok(())
}

#[test]
fn stereo_delay_validates_and_repeats_after_delay() -> Result<(), AudioError> {
    assert_eq!(
        StereoDelay::new(48_000, 0.0, 0.2, 0.5),
        Err(AudioError::InvalidDelayParameter)
    );
    let mut delay = StereoDelay::new(1_000, 2.0, 0.0, 1.0)?;
    let mut first = [1.0, -1.0, 0.0, 0.0, 0.0, 0.0];
    delay.process(&mut first)?;
    for (actual, expected) in first.into_iter().zip([0.0, 0.0, 0.0, 0.0, 1.0, -1.0]) {
        assert!((actual - expected).abs() < f32::EPSILON);
    }
    delay.reset();
    assert_eq!(
        delay.process(&mut [f32::INFINITY, 0.0]),
        Err(AudioError::NonFiniteSample)
    );
    Ok(())
}

#[test]
fn waveform_generation_preserves_bucket_extrema_and_rms() -> Result<(), AudioError> {
    let points = generate_waveform(&[-1.0, 0.5, 0.5, 0.0, 1.0], 2)?;
    assert_eq!(points.len(), 3);
    assert!((points[0].min + 1.0).abs() < f32::EPSILON);
    assert!((points[0].max - 0.5).abs() < f32::EPSILON);
    assert!((points[0].rms - 0.790_569_4).abs() < 1e-6);
    assert!((points[2].min - 1.0).abs() < f32::EPSILON);
    assert!((points[2].max - 1.0).abs() < f32::EPSILON);
    assert!((points[0].peak_dbfs() - 0.0).abs() < f32::EPSILON);
    assert!(
        WaveformPoint {
            min: 0.0,
            max: 0.0,
            rms: 0.0,
        }
        .peak_dbfs()
        .is_infinite()
    );
    assert_eq!(
        generate_waveform(&[0.0], 0),
        Err(AudioError::InvalidWaveformWindow)
    );
    assert_eq!(
        generate_waveform(&[f32::NAN], 1),
        Err(AudioError::NonFiniteSample)
    );
    Ok(())
}

#[test]
fn stereo_waveform_generation_preserves_channels() -> Result<(), AudioError> {
    let points = generate_stereo_waveform(&[-1.0, 0.25, 0.5, -0.5, 1.0, 0.75], 2)?;
    assert_eq!(points.len(), 2);
    assert!((points[0].left.min + 1.0).abs() < f32::EPSILON);
    assert!((points[0].left.max - 0.5).abs() < f32::EPSILON);
    assert!((points[0].right.min + 0.5).abs() < f32::EPSILON);
    assert!((points[0].right.max - 0.25).abs() < f32::EPSILON);
    assert_eq!(
        generate_stereo_waveform(&[0.0], 1),
        Err(AudioError::OddInterleavedBuffer)
    );
    Ok(())
}

#[test]
fn peak_normalizer_scales_signal_and_rejects_invalid_samples() -> Result<(), AudioError> {
    assert_eq!(PeakNormalizer::new(0.0), Err(AudioError::InvalidTargetPeak));
    let normalizer = PeakNormalizer::new(0.5)?;
    let mut samples = [-0.25, 0.1, 0.0];
    normalizer.process(&mut samples)?;
    assert!((samples[0] + 0.5).abs() < f32::EPSILON);
    assert!((samples[1] - 0.2).abs() < f32::EPSILON);
    assert_eq!(
        normalizer.process(&mut [f32::NAN]),
        Err(AudioError::NonFiniteSample)
    );
    Ok(())
}

#[test]
fn compressor_reduces_peaks_and_resets_detector() -> Result<(), AudioError> {
    assert_eq!(
        DynamicsCompressor::new(0, -12.0, 4.0, 5.0, 50.0, 0.0),
        Err(AudioError::InvalidDynamicsParameter)
    );
    let mut compressor = DynamicsCompressor::new(48_000, -12.0, 4.0, 0.0, 0.0, 0.0)?;
    let mut samples = [1.0, 1.0, 0.5, 0.5];
    compressor.process(&mut samples)?;
    assert!(samples[0] < 1.0);
    assert!(samples[1] < 1.0);
    assert!(samples[2] < 0.5);
    compressor.reset();
    assert!(compressor.envelope.abs() < f32::EPSILON);
    assert_eq!(
        compressor.process(&mut [f32::NAN, 0.0]),
        Err(AudioError::NonFiniteSample)
    );
    assert_eq!(
        compressor.process(&mut [0.0]),
        Err(AudioError::OddInterleavedBuffer)
    );
    Ok(())
}

#[test]
fn limiter_clamps_and_rejects_invalid_samples() -> Result<(), AudioError> {
    assert_eq!(Limiter::new(0.0), Err(AudioError::InvalidTargetPeak));
    let limiter = Limiter::new(0.8)?;
    let mut samples = [1.0, -1.0, 0.25];
    limiter.process(&mut samples)?;
    assert!((samples[0] - 0.8).abs() < f32::EPSILON);
    assert!((samples[1] + 0.8).abs() < f32::EPSILON);
    assert!((samples[2] - 0.25).abs() < f32::EPSILON);
    assert_eq!(
        limiter.process(&mut [f32::INFINITY]),
        Err(AudioError::NonFiniteSample)
    );
    Ok(())
}

#[test]
fn resampler_interpolates_into_caller_storage() -> Result<(), AudioError> {
    assert_eq!(
        LinearResampler::new(0, 48_000),
        Err(AudioError::InvalidSampleRate)
    );
    let resampler = LinearResampler::new(48_000, 24_000)?;
    let mut output = [0.0; 3];
    let written = resampler.process(&[0.0, 1.0, 2.0, 3.0], &mut output)?;
    assert_eq!(written, 2);
    assert!((output[0] - 0.0).abs() < f32::EPSILON);
    assert!((output[1] - 2.0).abs() < f32::EPSILON);
    assert_eq!(
        resampler.process(&[0.0, f32::INFINITY], &mut output),
        Err(AudioError::NonFiniteSample)
    );
    Ok(())
}

#[test]
fn streaming_resampler_matches_contiguous_processing() -> Result<(), AudioError> {
    let mut streaming = StreamingLinearResampler::new(2, 4)?;
    let mut first_output = [0.0; 4];
    let first_count = streaming.process(&[0.0, 1.0], &mut first_output)?;
    let mut second_output = [0.0; 4];
    let second_count = streaming.process(&[2.0, 3.0], &mut second_output)?;
    assert_eq!(first_count, 2);
    assert_eq!(second_count, 4);
    assert_eq!(&first_output[..first_count], &[0.0, 0.5]);
    assert_eq!(&second_output[..second_count], &[1.0, 1.5, 2.0, 2.5]);
    streaming.reset();
    let mut reset_output = [0.0; 2];
    assert_eq!(streaming.process(&[0.0, 1.0], &mut reset_output)?, 2);
    Ok(())
}

#[test]
fn integrated_loudness_measures_silence_and_validates_input() -> Result<(), AudioError> {
    let silence = vec![0.0; 48_000 * 2];
    let measurement = measure_integrated_loudness(&silence, 48_000)?;
    assert!(measurement.lufs.is_infinite() && measurement.lufs.is_sign_negative());
    assert_eq!(measurement.block_count, 7);
    assert_eq!(measurement.gated_block_count, 0);
    assert_eq!(
        measure_integrated_loudness(&silence, 44_100),
        Err(AudioError::UnsupportedLoudnessSampleRate)
    );
    assert_eq!(
        measure_integrated_loudness(&[0.0], 48_000),
        Err(AudioError::OddInterleavedBuffer)
    );
    assert_eq!(
        measure_integrated_loudness(&[f32::NAN, 0.0], 48_000),
        Err(AudioError::NonFiniteSample)
    );
    Ok(())
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn integrated_loudness_tracks_known_gain_delta() -> Result<(), AudioError> {
    let mut full_scale = Vec::with_capacity(48_000 * 4);
    let mut half_scale = Vec::with_capacity(48_000 * 4);
    for frame in 0..48_000 * 2 {
        let sample =
            (2.0 * std::f64::consts::PI * 1_000.0 * f64::from(frame) / 48_000.0).sin() as f32;
        full_scale.extend_from_slice(&[sample, sample]);
        half_scale.extend_from_slice(&[sample * 0.5, sample * 0.5]);
    }
    let full = measure_integrated_loudness(&full_scale, 48_000)?;
    let half = measure_integrated_loudness(&half_scale, 48_000)?;
    assert!((full.lufs - 0.007).abs() < 0.01, "{}", full.lufs);
    assert!((full.lufs - half.lufs - 6.020_599_913).abs() < 1.0e-6);
    assert_eq!(full.block_count, 17);
    assert_eq!(full.gated_block_count, full.block_count);
    Ok(())
}

#[test]
fn integrated_loudness_relative_gate_excludes_quiet_programme() -> Result<(), AudioError> {
    let mut samples = vec![0.5; 48_000 * 2 * 2];
    samples.extend(vec![0.005; 48_000 * 2 * 2]);
    let measurement = measure_integrated_loudness(&samples, 48_000)?;
    assert!(measurement.gated_block_count < measurement.block_count);
    assert!(measurement.gated_block_count > 0);
    Ok(())
}
