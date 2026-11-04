//! Validated export adapters with temporary-file finalization.
//!
//! The initial adapter establishes a real single-source MP4 workflow using an
//! explicitly configured `FFmpeg` executable. Multi-clip timeline rendering,
//! effects, and production codec distribution remain separate milestones.

use prism_media::{FfprobeInspector, MediaError, StreamKind};
use prism_timeline::{RationalTime, RoundingMode, TimeError};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error;
use uuid::Uuid;

/// One exact source range exported to a new MP4 file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BasicMp4ExportRequest {
    /// Existing input media path.
    pub source: PathBuf,
    /// Nonnegative source timestamp.
    pub source_start: RationalTime,
    /// Strictly positive source duration.
    pub duration: RationalTime,
    /// New final output path. Existing files are never replaced.
    pub output: PathBuf,
}

/// Facts independently observed before finalizing an export.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportReport {
    /// Finalized output path.
    pub output: PathBuf,
    /// Finalized file length.
    pub byte_length: u64,
    /// Probed output duration.
    pub duration: RationalTime,
    /// Whether the finalized output contains a video stream.
    pub has_video: bool,
    /// Whether the finalized output contains an audio stream.
    pub has_audio: bool,
}

/// Basic export failures.
#[derive(Debug, Error)]
pub enum ExportError {
    /// Input source does not exist as a regular file.
    #[error("export source is not a regular file: {0}")]
    InvalidSource(PathBuf),
    /// Output already exists and will not be overwritten.
    #[error("export output already exists: {0}")]
    OutputExists(PathBuf),
    /// Output has no existing parent directory.
    #[error("export output parent is unavailable: {0}")]
    InvalidOutputParent(PathBuf),
    /// Source start was before zero or duration was not positive.
    #[error("export range must have a nonnegative start and positive duration")]
    InvalidRange,
    /// Exact timestamp conversion failed.
    #[error("export time conversion failed: {0}")]
    Time(#[from] TimeError),
    /// The encoder process could not start or file finalization failed.
    #[error("export I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The encoder process reported failure.
    #[error("encoder failed with status {status}: {stderr}")]
    Encoder {
        /// Process exit status when available.
        status: i32,
        /// Diagnostic output with lossy UTF-8 conversion.
        stderr: String,
    },
    /// Independent output inspection failed.
    #[error("export validation probe failed: {0}")]
    Probe(#[from] MediaError),
    /// The encoder output was empty or lacked required video/duration evidence.
    #[error("encoded output failed validation: {0}")]
    InvalidOutput(&'static str),
}

/// Explicit `FFmpeg`/ffprobe adapter for the initial validated MP4 workflow.
#[derive(Clone, Debug)]
pub struct BasicMp4Exporter {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

impl BasicMp4Exporter {
    /// Creates an exporter using explicit executable paths.
    #[must_use]
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
        }
    }

    /// Encodes, independently validates, and finalizes a basic MP4 export.
    ///
    /// The temporary file is created beside the destination so no-clobber link
    /// finalization remains on the same filesystem. Existing destinations are
    /// refused, including if one appears during encoding.
    ///
    /// # Errors
    ///
    /// Returns [`ExportError`] for invalid paths/ranges, encoder failure,
    /// validation failure, or atomic finalization failure.
    pub fn export(&self, request: &BasicMp4ExportRequest) -> Result<ExportReport, ExportError> {
        validate_request(request)?;
        let temporary = temporary_output_path(&request.output)?;
        let guard = TemporaryExport::new(temporary);
        let start = format_timestamp(request.source_start)?;
        let duration = format_timestamp(request.duration)?;
        let output = Command::new(&self.ffmpeg)
            .args(["-v", "error", "-ss", &start, "-t", &duration, "-i"])
            .arg(&request.source)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-c:v",
                "mpeg4",
                "-q:v",
                "4",
                "-c:a",
                "aac",
                "-movflags",
                "+faststart",
                "-f",
                "mp4",
                "-y",
            ])
            .arg(guard.path())
            .output()?;
        if !output.status.success() {
            return Err(ExportError::Encoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let metadata = FfprobeInspector::new(&self.ffprobe).inspect(guard.path())?;
        let byte_length = fs::metadata(guard.path())?.len();
        if byte_length == 0 {
            return Err(ExportError::InvalidOutput("file is empty"));
        }
        let duration = metadata
            .duration
            .ok_or(ExportError::InvalidOutput("duration is missing"))?;
        if !duration.is_positive() {
            return Err(ExportError::InvalidOutput("duration is not positive"));
        }
        let has_video = metadata
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Video(_)));
        if !has_video {
            return Err(ExportError::InvalidOutput("video stream is missing"));
        }
        let has_audio = metadata
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Audio(_)));
        guard.finalize(&request.output)?;
        Ok(ExportReport {
            output: request.output.clone(),
            byte_length,
            duration,
            has_video,
            has_audio,
        })
    }
}

fn validate_request(request: &BasicMp4ExportRequest) -> Result<(), ExportError> {
    if !request.source.is_file() {
        return Err(ExportError::InvalidSource(request.source.clone()));
    }
    if !request.source_start.is_non_negative() || !request.duration.is_positive() {
        return Err(ExportError::InvalidRange);
    }
    if request.output.exists() {
        return Err(ExportError::OutputExists(request.output.clone()));
    }
    let parent = request
        .output
        .parent()
        .filter(|parent| parent.is_dir())
        .ok_or_else(|| ExportError::InvalidOutputParent(request.output.clone()))?;
    if parent.as_os_str().is_empty() {
        return Err(ExportError::InvalidOutputParent(request.output.clone()));
    }
    Ok(())
}

fn format_timestamp(time: RationalTime) -> Result<String, ExportError> {
    let nanoseconds = time.to_ticks(1_000_000_000, RoundingMode::NearestTiesAway)?;
    let seconds = nanoseconds / 1_000_000_000;
    let subsecond = nanoseconds % 1_000_000_000;
    Ok(format!("{seconds}.{subsecond:09}"))
}

fn temporary_output_path(output: &Path) -> Result<PathBuf, ExportError> {
    let parent = output
        .parent()
        .ok_or_else(|| ExportError::InvalidOutputParent(output.to_path_buf()))?;
    let name = output
        .file_name()
        .ok_or_else(|| ExportError::InvalidOutputParent(output.to_path_buf()))?
        .to_string_lossy();
    Ok(parent.join(format!(".{name}.{}.tmp.mp4", Uuid::new_v4())))
}

struct TemporaryExport {
    path: PathBuf,
    finalized: bool,
}

impl TemporaryExport {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            finalized: false,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn finalize(mut self, output: &Path) -> Result<(), std::io::Error> {
        fs::hard_link(&self.path, output)?;
        fs::remove_file(&self.path)?;
        self.finalized = true;
        Ok(())
    }
}

impl Drop for TemporaryExport {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BasicMp4ExportRequest, ExportError, format_timestamp, validate_request};
    use prism_timeline::RationalTime;
    use std::path::PathBuf;

    #[test]
    fn timestamp_format_uses_exact_named_rounding() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            format_timestamp(RationalTime::new(1_001, 30_000)?)?,
            "0.033366667"
        );
        Ok(())
    }

    #[test]
    fn invalid_source_is_rejected_before_encoder_launch() -> Result<(), Box<dyn std::error::Error>>
    {
        let request = BasicMp4ExportRequest {
            source: PathBuf::from("source-that-does-not-exist.mp4"),
            source_start: RationalTime::ZERO,
            duration: RationalTime::new(1, 1)?,
            output: PathBuf::from("output.mp4"),
        };
        assert!(matches!(
            validate_request(&request),
            Err(ExportError::InvalidSource(_))
        ));
        Ok(())
    }
}
