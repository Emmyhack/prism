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
        BlendMode, CompositorError, Rgba, TransitionKind, composite_over, transition_pixel,
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
}
