use prism_project::ClipTransform;
use std::fmt::Write as _;
use thiserror::Error;

/// Failures while compiling the authoritative clip-transform semantics.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClipTransformFilterError {
    /// Output dimensions must both be positive.
    #[error("clip transform output dimensions must be positive")]
    InvalidDimensions,
    /// The domain transform was not valid.
    #[error("clip transform contains invalid values")]
    InvalidTransform,
}

/// Compiles the approved crop, anchor, scale, rotation, position and opacity
/// order into one `FFmpeg` filter fragment on a transparent output-sized canvas.
/// Preview and export consume this same compiler.
///
/// # Errors
///
/// Returns an error for zero dimensions or an invalid transform.
pub fn ffmpeg_clip_transform_filter(
    input_label: &str,
    output_label: &str,
    width: u32,
    height: u32,
    transform: ClipTransform,
) -> Result<String, ClipTransformFilterError> {
    if width == 0 || height == 0 {
        return Err(ClipTransformFilterError::InvalidDimensions);
    }
    if !transform.is_valid() {
        return Err(ClipTransformFilterError::InvalidTransform);
    }
    if transform == ClipTransform::default() {
        return Ok(format!(
            "[{input_label}]scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black[{output_label}]"
        ));
    }
    let angle = f64::from(transform.rotation_degrees).to_radians();
    let mut filter = String::new();
    let _ = write!(
        filter,
        "[{input_label}]crop=iw*(1-{left:.9}-{right:.9}):ih*(1-{top:.9}-{bottom:.9}):iw*{left:.9}:ih*{top:.9},scale={width}:{height}:force_original_aspect_ratio=decrease,format=rgba,pad=ceil(2*max(iw*{anchor_x:.9}\\,iw*(1-{anchor_x:.9}))):ceil(2*max(ih*{anchor_y:.9}\\,ih*(1-{anchor_y:.9}))):(ow/2-iw*{anchor_x:.9}):(oh/2-ih*{anchor_y:.9}):color=black@0,scale=iw*{scale_x:.9}:ih*{scale_y:.9},rotate={angle:.12}:ow=rotw({angle:.12}):oh=roth({angle:.12}):c=none,colorchannelmixer=aa={opacity:.9}[{output_label}_layer];color=c=black@0:s={width}x{height},format=rgba[{output_label}_canvas];[{output_label}_canvas][{output_label}_layer]overlay=x=(W-w)/2+{position_x:.9}:y=(H-h)/2+{position_y:.9}:format=auto[{output_label}]",
        left = transform.crop_left,
        right = transform.crop_right,
        top = transform.crop_top,
        bottom = transform.crop_bottom,
        anchor_x = transform.anchor_x,
        anchor_y = transform.anchor_y,
        scale_x = transform.scale_x,
        scale_y = transform.scale_y,
        opacity = transform.opacity,
        position_x = transform.position_x,
        position_y = transform.position_y,
    );
    Ok(filter)
}

/// Compiles a clip transform and composites its transparent result over the
/// sequence's black program canvas. This is the terminal program-preview form;
/// multitrack export performs its sequence composition afterward.
///
/// # Errors
///
/// Returns the same validation failures as [`ffmpeg_clip_transform_filter`].
pub fn ffmpeg_composited_clip_transform_filter(
    input_label: &str,
    output_label: &str,
    width: u32,
    height: u32,
    transform: ClipTransform,
) -> Result<String, ClipTransformFilterError> {
    let layer_label = format!("{output_label}_layer");
    let canvas_label = format!("{output_label}_background");
    let mut filter =
        ffmpeg_clip_transform_filter(input_label, &layer_label, width, height, transform)?;
    let _ = write!(
        filter,
        ";color=c=black:s={width}x{height},format=rgba[{canvas_label}];[{canvas_label}][{layer_label}]overlay=shortest=1:format=auto[{output_label}]"
    );
    Ok(filter)
}

#[cfg(test)]
mod tests {
    use super::{ClipTransformFilterError, ffmpeg_clip_transform_filter};
    use prism_project::ClipTransform;

    #[test]
    fn identity_filter_preserves_existing_fit_with_letterbox()
    -> Result<(), ClipTransformFilterError> {
        let filter =
            ffmpeg_clip_transform_filter("0:v:0", "clip", 1920, 1080, ClipTransform::default())?;
        assert_eq!(
            filter,
            "[0:v:0]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:color=black[clip]"
        );
        Ok(())
    }

    #[test]
    fn custom_filter_is_deterministic_and_rejects_invalid_input()
    -> Result<(), ClipTransformFilterError> {
        let transform = ClipTransform {
            position_x: 24.0,
            position_y: -12.0,
            scale_x: 1.25,
            scale_y: 0.75,
            rotation_degrees: 90.0,
            opacity: 0.5,
            crop_left: 0.1,
            ..ClipTransform::default()
        };
        let first = ffmpeg_clip_transform_filter("in", "out", 1080, 1920, transform)?;
        assert_eq!(
            first,
            ffmpeg_clip_transform_filter("in", "out", 1080, 1920, transform)?
        );
        assert!(first.contains("rotate=1.570796326795"));
        let positions = [
            "crop=",
            "pad=",
            "scale=iw*",
            "rotate=",
            "colorchannelmixer=",
            "overlay=",
        ]
        .map(|token| {
            first
                .find(token)
                .ok_or(ClipTransformFilterError::InvalidTransform)
        })
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            ffmpeg_clip_transform_filter("in", "out", 0, 1080, transform),
            Err(ClipTransformFilterError::InvalidDimensions)
        );
        assert_eq!(
            ffmpeg_clip_transform_filter(
                "in",
                "out",
                1080,
                1920,
                ClipTransform {
                    opacity: f32::NAN,
                    ..transform
                }
            ),
            Err(ClipTransformFilterError::InvalidTransform)
        );
        Ok(())
    }

    #[test]
    fn composited_filter_terminates_on_black_program_canvas() -> Result<(), ClipTransformFilterError>
    {
        let filter = super::ffmpeg_composited_clip_transform_filter(
            "in",
            "program",
            320,
            180,
            ClipTransform {
                opacity: 0.5,
                ..ClipTransform::default()
            },
        )?;
        assert!(filter.contains("color=c=black:s=320x180"));
        assert!(filter.ends_with("[program]"));
        Ok(())
    }
}
