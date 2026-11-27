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
fn vectorscope_counts_chroma_coordinates_and_rejects_invalid_samples() -> Result<(), ColorError> {
    let scope = Vectorscope::from_pixels(&[Rgb::new(1.0, 0.0, 0.0)?, Rgb::new(0.5, 0.5, 0.5)?])?;
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
        Rgb::new(nits / 10_000.0, nits / 10_000.0, nits / 10_000.0)?.encode(TransferFunction::Pq)
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
