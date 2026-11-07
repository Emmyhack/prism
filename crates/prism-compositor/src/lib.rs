//! Validated CPU compositing primitives for linear-light RGBA values.
//!
//! The API is intentionally independent of GPU handles. The same equations
//! can later be used by a wgpu render graph and a CPU/export fallback.

use thiserror::Error;

/// Blend equations supported by the CPU compositor foundation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlendMode {
    /// The foreground replaces the background according to alpha.
    Normal,
    /// Component-wise multiplication.
    Multiply,
    /// Component-wise screen blend.
    Screen,
    /// Component-wise additive blend, clamped to the normalized range.
    Additive,
    /// Contrast-preserving overlay blend.
    Overlay,
}

/// CPU transition operations supported by the compositor foundation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    /// Linear interpolation from outgoing to incoming pixels.
    Dissolve,
}

/// Channel used to derive a normalized compositing matte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatteSource {
    /// Use source alpha directly.
    Alpha,
    /// Use one minus source alpha.
    InvertedAlpha,
    /// Use linear Rec.709 luma multiplied by source alpha.
    Luma,
    /// Use one minus alpha-weighted linear Rec.709 luma.
    InvertedLuma,
}

/// Boolean-style operation for combining two coverage masks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaskOperation {
    /// Replace the destination with the incoming mask.
    Replace,
    /// Add coverage and clamp it to one.
    Add,
    /// Remove incoming coverage from the destination.
    Subtract,
    /// Keep only coverage shared by both masks.
    Intersect,
}

/// Linear-light, normalized RGBA color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    /// Red channel in `[0, 1]`.
    pub red: f32,
    /// Green channel in `[0, 1]`.
    pub green: f32,
    /// Blue channel in `[0, 1]`.
    pub blue: f32,
    /// Alpha channel in `[0, 1]`.
    pub alpha: f32,
}

/// Compositor input validation failures.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CompositorError {
    /// A channel was NaN or infinite.
    #[error("RGBA channels must be finite")]
    NonFinite,
    /// A channel was outside the normalized range.
    #[error("RGBA channels must be between 0.0 and 1.0")]
    OutOfRange,
    /// Transition progress was outside `[0, 1]`.
    #[error("transition progress must be between 0.0 and 1.0")]
    InvalidProgress,
    /// Mask dimensions were zero, overflowing, or did not match storage.
    #[error("mask dimensions do not match its coverage values")]
    InvalidMaskDimensions,
    /// A mask coverage value was non-finite or outside `[0, 1]`.
    #[error("mask coverage must be finite and between 0.0 and 1.0")]
    InvalidMaskCoverage,
    /// Mask, matte, and pixel buffers must have identical dimensions.
    #[error("mask and image dimensions do not match")]
    MaskSizeMismatch,
}

/// Owned normalized per-pixel coverage mask.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageMask {
    /// Mask width in pixels.
    pub width: usize,
    /// Mask height in pixels.
    pub height: usize,
    /// Row-major normalized coverage values.
    pub coverage: Box<[f32]>,
}

impl CoverageMask {
    /// Creates a validated coverage mask.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError::InvalidMaskDimensions`] or
    /// [`CompositorError::InvalidMaskCoverage`] for malformed input.
    pub fn new(width: usize, height: usize, coverage: Vec<f32>) -> Result<Self, CompositorError> {
        if width == 0
            || height == 0
            || width
                .checked_mul(height)
                .is_none_or(|count| count != coverage.len())
        {
            return Err(CompositorError::InvalidMaskDimensions);
        }
        if coverage
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(CompositorError::InvalidMaskCoverage);
        }
        Ok(Self {
            width,
            height,
            coverage: coverage.into_boxed_slice(),
        })
    }

    /// Derives coverage from validated straight-alpha RGBA pixels.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError`] for invalid dimensions or pixels.
    pub fn from_matte(
        width: usize,
        height: usize,
        pixels: &[Rgba],
        source: MatteSource,
    ) -> Result<Self, CompositorError> {
        if width == 0
            || height == 0
            || width
                .checked_mul(height)
                .is_none_or(|count| count != pixels.len())
        {
            return Err(CompositorError::InvalidMaskDimensions);
        }
        let mut coverage = Vec::with_capacity(pixels.len());
        for &pixel in pixels {
            pixel.validate()?;
            let luma =
                (0.2126 * pixel.red + 0.7152 * pixel.green + 0.0722 * pixel.blue) * pixel.alpha;
            coverage.push(match source {
                MatteSource::Alpha => pixel.alpha,
                MatteSource::InvertedAlpha => 1.0 - pixel.alpha,
                MatteSource::Luma => luma,
                MatteSource::InvertedLuma => 1.0 - luma,
            });
        }
        Self::new(width, height, coverage)
    }

    /// Combines another same-sized mask into this mask without allocation.
    ///
    /// Validation occurs before modification, so errors leave `self`
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError::MaskSizeMismatch`] when dimensions differ,
    /// or [`CompositorError::InvalidMaskCoverage`] for invalid values.
    pub fn combine(
        &mut self,
        incoming: &Self,
        operation: MaskOperation,
    ) -> Result<(), CompositorError> {
        if self.width != incoming.width || self.height != incoming.height {
            return Err(CompositorError::MaskSizeMismatch);
        }
        if self
            .coverage
            .iter()
            .chain(incoming.coverage.iter())
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(CompositorError::InvalidMaskCoverage);
        }
        for (destination, &source) in self.coverage.iter_mut().zip(&incoming.coverage) {
            *destination = match operation {
                MaskOperation::Replace => source,
                MaskOperation::Add => (*destination + source).min(1.0),
                MaskOperation::Subtract => (*destination - source).max(0.0),
                MaskOperation::Intersect => *destination * source,
            };
        }
        Ok(())
    }

    /// Applies coverage to straight-alpha pixels in place.
    ///
    /// RGB remains unmodified and alpha is multiplied by coverage. All inputs
    /// are validated before modification, providing atomic failure behavior.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError`] for a size mismatch or invalid pixel/mask.
    pub fn apply_to(&self, pixels: &mut [Rgba]) -> Result<(), CompositorError> {
        if pixels.len() != self.coverage.len() {
            return Err(CompositorError::MaskSizeMismatch);
        }
        if self
            .coverage
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(CompositorError::InvalidMaskCoverage);
        }
        for &pixel in pixels.iter() {
            pixel.validate()?;
        }
        for (pixel, &coverage) in pixels.iter_mut().zip(&self.coverage) {
            pixel.alpha *= coverage;
        }
        Ok(())
    }
}

/// Evaluates one transition pixel at normalized progress.
///
/// The operation is deterministic and operates in linear-light RGBA. At
/// progress `0` it returns the outgoing pixel; at `1` it returns the incoming
/// pixel. The current foundation supports a straight cross-dissolve.
///
/// # Errors
///
/// Returns [`CompositorError`] for invalid pixels or progress.
pub fn transition_pixel(
    outgoing: Rgba,
    incoming: Rgba,
    progress: f32,
    kind: TransitionKind,
) -> Result<Rgba, CompositorError> {
    outgoing.validate()?;
    incoming.validate()?;
    if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
        return Err(CompositorError::InvalidProgress);
    }
    match kind {
        TransitionKind::Dissolve => Rgba::new(
            outgoing.red + (incoming.red - outgoing.red) * progress,
            outgoing.green + (incoming.green - outgoing.green) * progress,
            outgoing.blue + (incoming.blue - outgoing.blue) * progress,
            outgoing.alpha + (incoming.alpha - outgoing.alpha) * progress,
        ),
    }
}

impl Rgba {
    /// Creates a validated linear-light color.
    ///
    /// # Errors
    ///
    /// Returns [`CompositorError::NonFinite`] or
    /// [`CompositorError::OutOfRange`] for invalid channels.
    pub fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Result<Self, CompositorError> {
        let value = Self {
            red,
            green,
            blue,
            alpha,
        };
        value.validate()?;
        Ok(value)
    }

    /// Validates every channel.
    ///
    /// # Errors
    ///
    /// Returns a [`CompositorError`] for non-finite or out-of-range values.
    pub fn validate(self) -> Result<(), CompositorError> {
        for channel in [self.red, self.green, self.blue, self.alpha] {
            if !channel.is_finite() {
                return Err(CompositorError::NonFinite);
            }
            if !(0.0..=1.0).contains(&channel) {
                return Err(CompositorError::OutOfRange);
            }
        }
        Ok(())
    }
}

/// Composites `foreground` over `background` using a straight-alpha equation.
///
/// RGB values are interpreted as linear-light values. The blend equation is
/// applied only to color channels; alpha remains source-over alpha.
///
/// # Errors
///
/// Returns a [`CompositorError`] when either input is invalid.
pub fn composite_over(
    background: Rgba,
    foreground: Rgba,
    mode: BlendMode,
) -> Result<Rgba, CompositorError> {
    background.validate()?;
    foreground.validate()?;
    let background_alpha = background.alpha;
    let foreground_alpha = foreground.alpha;
    let output_alpha = foreground_alpha + background_alpha * (1.0 - foreground_alpha);
    if output_alpha <= f32::EPSILON {
        return Rgba::new(0.0, 0.0, 0.0, 0.0);
    }
    let red = composite_channel(
        background.red,
        foreground.red,
        background_alpha,
        foreground_alpha,
        output_alpha,
        mode,
    );
    let green = composite_channel(
        background.green,
        foreground.green,
        background_alpha,
        foreground_alpha,
        output_alpha,
        mode,
    );
    let blue = composite_channel(
        background.blue,
        foreground.blue,
        background_alpha,
        foreground_alpha,
        output_alpha,
        mode,
    );
    Rgba::new(red, green, blue, output_alpha)
}

fn composite_channel(
    background: f32,
    foreground: f32,
    background_alpha: f32,
    foreground_alpha: f32,
    output_alpha: f32,
    mode: BlendMode,
) -> f32 {
    let blended = match mode {
        BlendMode::Normal => foreground,
        BlendMode::Multiply => background * foreground,
        BlendMode::Screen => 1.0 - (1.0 - background) * (1.0 - foreground),
        BlendMode::Additive => (background + foreground).min(1.0),
        BlendMode::Overlay => {
            if background <= 0.5 {
                2.0 * background * foreground
            } else {
                1.0 - 2.0 * (1.0 - background) * (1.0 - foreground)
            }
        }
    };
    ((1.0 - foreground_alpha) * background_alpha * background
        + (1.0 - background_alpha) * foreground_alpha * foreground
        + foreground_alpha * background_alpha * blended)
        / output_alpha
}

#[cfg(test)]
mod tests {
    use super::{
        BlendMode, CompositorError, CoverageMask, MaskOperation, MatteSource, Rgba, TransitionKind,
        composite_over, transition_pixel,
    };

    #[test]
    fn normal_source_over_respects_alpha() -> Result<(), CompositorError> {
        let background = Rgba::new(0.2, 0.4, 0.6, 1.0)?;
        let foreground = Rgba::new(0.8, 0.6, 0.2, 0.5)?;
        let result = composite_over(background, foreground, BlendMode::Normal)?;
        assert!((result.red - 0.5).abs() < 1e-6);
        assert!((result.green - 0.5).abs() < 1e-6);
        assert!((result.blue - 0.4).abs() < 1e-6);
        assert!((result.alpha - 1.0).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn multiply_and_screen_are_distinct_for_opaque_layers() -> Result<(), CompositorError> {
        let background = Rgba::new(0.2, 0.4, 0.6, 1.0)?;
        let foreground = Rgba::new(0.8, 0.5, 0.25, 1.0)?;
        let multiply = composite_over(background, foreground, BlendMode::Multiply)?;
        let screen = composite_over(background, foreground, BlendMode::Screen)?;
        assert!((multiply.red - 0.16).abs() < 1e-6);
        assert!((multiply.green - 0.2).abs() < 1e-6);
        assert!((multiply.blue - 0.15).abs() < 1e-6);
        assert!((screen.red - 0.84).abs() < 1e-6);
        assert!((screen.green - 0.7).abs() < 1e-6);
        assert!((screen.blue - 0.7).abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn additive_and_overlay_are_deterministic_for_opaque_layers() -> Result<(), CompositorError> {
        let background = Rgba::new(0.25, 0.75, 0.5, 1.0)?;
        let foreground = Rgba::new(0.8, 0.4, 0.5, 1.0)?;
        let additive = composite_over(background, foreground, BlendMode::Additive)?;
        let overlay = composite_over(background, foreground, BlendMode::Overlay)?;
        assert!((additive.red - 1.0).abs() < f32::EPSILON);
        assert!((additive.green - 1.0).abs() < f32::EPSILON);
        assert!((additive.blue - 1.0).abs() < f32::EPSILON);
        assert!((overlay.red - 0.4).abs() < 1e-6);
        assert!((overlay.green - 0.7).abs() < 1e-6);
        assert!((overlay.blue - 0.5).abs() < 1e-6);
        Ok(())
    }

    #[test]
    fn transparent_layers_and_invalid_values_are_handled() -> Result<(), CompositorError> {
        let transparent = Rgba::new(0.0, 0.0, 0.0, 0.0)?;
        let result = composite_over(transparent, transparent, BlendMode::Normal)?;
        assert!(result.red.abs() < f32::EPSILON && result.alpha.abs() < f32::EPSILON);
        assert_eq!(
            Rgba::new(1.1, 0.0, 0.0, 1.0),
            Err(CompositorError::OutOfRange)
        );
        Ok(())
    }

    #[test]
    fn dissolve_interpolates_channels_and_validates_progress() -> Result<(), CompositorError> {
        let outgoing = Rgba::new(0.0, 0.2, 0.4, 1.0)?;
        let incoming = Rgba::new(1.0, 0.8, 0.6, 0.5)?;
        let midpoint = transition_pixel(outgoing, incoming, 0.5, TransitionKind::Dissolve)?;
        assert_eq!(midpoint, Rgba::new(0.5, 0.5, 0.5, 0.75)?);
        assert_eq!(
            transition_pixel(outgoing, incoming, f32::NAN, TransitionKind::Dissolve),
            Err(CompositorError::InvalidProgress)
        );
        assert_eq!(
            transition_pixel(outgoing, incoming, 1.1, TransitionKind::Dissolve),
            Err(CompositorError::InvalidProgress)
        );
        Ok(())
    }

    #[test]
    fn matte_sources_extract_alpha_and_alpha_weighted_luma() -> Result<(), CompositorError> {
        let pixels = [
            Rgba::new(1.0, 1.0, 1.0, 0.25)?,
            Rgba::new(0.0, 0.0, 0.0, 0.75)?,
        ];
        let alpha = CoverageMask::from_matte(2, 1, &pixels, MatteSource::Alpha)?;
        let inverted = CoverageMask::from_matte(2, 1, &pixels, MatteSource::InvertedAlpha)?;
        let luma = CoverageMask::from_matte(2, 1, &pixels, MatteSource::Luma)?;
        let inverted_luma = CoverageMask::from_matte(2, 1, &pixels, MatteSource::InvertedLuma)?;
        assert_eq!(&*alpha.coverage, &[0.25, 0.75]);
        assert_eq!(&*inverted.coverage, &[0.75, 0.25]);
        assert!((luma.coverage[0] - 0.25).abs() < 1.0e-6);
        assert!(luma.coverage[1].abs() < f32::EPSILON);
        assert!((inverted_luma.coverage[0] - 0.75).abs() < 1.0e-6);
        assert!((inverted_luma.coverage[1] - 1.0).abs() < f32::EPSILON);
        Ok(())
    }

    #[test]
    fn mask_operations_are_bounded_and_deterministic() -> Result<(), CompositorError> {
        let incoming = CoverageMask::new(2, 1, vec![0.75, 0.25])?;
        let mut add = CoverageMask::new(2, 1, vec![0.5, 0.5])?;
        add.combine(&incoming, MaskOperation::Add)?;
        assert_eq!(&*add.coverage, &[1.0, 0.75]);
        let mut subtract = CoverageMask::new(2, 1, vec![0.5, 0.5])?;
        subtract.combine(&incoming, MaskOperation::Subtract)?;
        assert_eq!(&*subtract.coverage, &[0.0, 0.25]);
        let mut intersect = CoverageMask::new(2, 1, vec![0.5, 0.5])?;
        intersect.combine(&incoming, MaskOperation::Intersect)?;
        assert_eq!(&*intersect.coverage, &[0.375, 0.125]);
        intersect.combine(&incoming, MaskOperation::Replace)?;
        assert_eq!(intersect, incoming);
        Ok(())
    }

    #[test]
    fn applying_mask_changes_only_alpha_and_fails_atomically() -> Result<(), CompositorError> {
        let mask = CoverageMask::new(2, 1, vec![0.5, 0.0])?;
        let mut pixels = [
            Rgba::new(0.2, 0.4, 0.6, 0.8)?,
            Rgba::new(1.0, 0.5, 0.25, 1.0)?,
        ];
        mask.apply_to(&mut pixels)?;
        assert_eq!(pixels[0], Rgba::new(0.2, 0.4, 0.6, 0.4)?);
        assert_eq!(pixels[1], Rgba::new(1.0, 0.5, 0.25, 0.0)?);

        let mut invalid_pixels = [
            Rgba::new(0.2, 0.4, 0.6, 0.8)?,
            Rgba {
                red: f32::NAN,
                green: 0.0,
                blue: 0.0,
                alpha: 1.0,
            },
        ];
        let original = invalid_pixels;
        assert_eq!(
            mask.apply_to(&mut invalid_pixels),
            Err(CompositorError::NonFinite)
        );
        assert_eq!(invalid_pixels[0], original[0]);
        assert!(invalid_pixels[1].red.is_nan());
        Ok(())
    }

    #[test]
    fn masks_reject_bad_dimensions_coverage_and_combinations() -> Result<(), CompositorError> {
        assert_eq!(
            CoverageMask::new(2, 2, vec![0.0; 3]),
            Err(CompositorError::InvalidMaskDimensions)
        );
        assert_eq!(
            CoverageMask::new(1, 1, vec![f32::NAN]),
            Err(CompositorError::InvalidMaskCoverage)
        );
        let mut one = CoverageMask::new(1, 1, vec![1.0])?;
        let two = CoverageMask::new(2, 1, vec![1.0, 1.0])?;
        assert_eq!(
            one.combine(&two, MaskOperation::Add),
            Err(CompositorError::MaskSizeMismatch)
        );
        assert_eq!(
            one.apply_to(&mut []),
            Err(CompositorError::MaskSizeMismatch)
        );
        Ok(())
    }
}
