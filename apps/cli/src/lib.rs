//! Dependency-light command-line workflows backed by Prism's core APIs.

use prism_core::{SequenceId, TrackId};
use prism_export::{BasicMp4Exporter, ExportError, ExportReport, SingleTrackMp4ExportRequest};
use prism_persistence::{PersistenceError, load};
use prism_timeline::{RationalTime, TimeError};
use std::{ffi::OsString, path::PathBuf};
use thiserror::Error;
use uuid::Uuid;

/// Parsed `export-track` command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportTrackArgs {
    /// Validated project database to load.
    pub project: PathBuf,
    /// Sequence to render.
    pub sequence_id: SequenceId,
    /// Video track to render.
    pub track_id: TrackId,
    /// New MP4 destination.
    pub output: PathBuf,
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
    /// Exact output frame rate.
    pub frame_rate: RationalTime,
    /// Explicit encoder executable.
    pub ffmpeg: PathBuf,
    /// Explicit validation-probe executable.
    pub ffprobe: PathBuf,
}

/// CLI parse and workflow failures.
#[derive(Debug, Error)]
pub enum CliError {
    /// No supported subcommand was supplied.
    #[error(
        "usage: prism-cli export-track --project PATH --sequence UUID --track UUID --output PATH --width PIXELS --height PIXELS --fps NUM/DEN --ffmpeg PATH --ffprobe PATH"
    )]
    Usage,
    /// A flag was unknown or repeated.
    #[error("unknown or duplicate option: {0}")]
    InvalidOption(String),
    /// A required flag was absent.
    #[error("missing required option: {0}")]
    MissingOption(&'static str),
    /// A flag requiring textual parsing was not valid UTF-8.
    #[error("option value is not valid UTF-8: {0}")]
    NonUtf8Value(&'static str),
    /// A typed option value was malformed.
    #[error("invalid value for {option}: {value}")]
    InvalidValue {
        /// Flag name.
        option: &'static str,
        /// Rejected value.
        value: String,
    },
    /// Exact rate construction failed.
    #[error("invalid frame rate: {0}")]
    Time(#[from] TimeError),
    /// Project loading failed.
    #[error("could not load project: {0}")]
    Persistence(#[from] PersistenceError),
    /// Validated export failed.
    #[error("could not export track: {0}")]
    Export(#[from] ExportError),
}

/// Parses and runs one CLI invocation, excluding the executable name.
///
/// # Errors
///
/// Returns [`CliError`] for invalid arguments, project loading, or export.
pub fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<ExportReport, CliError> {
    let parsed = parse_export_track(arguments)?;
    let project = load(&parsed.project)?;
    Ok(
        BasicMp4Exporter::new(&parsed.ffmpeg, &parsed.ffprobe).export_single_video_track(
            &project,
            parsed.sequence_id,
            parsed.track_id,
            &SingleTrackMp4ExportRequest {
                width: parsed.width,
                height: parsed.height,
                frame_rate: parsed.frame_rate,
                output: parsed.output,
            },
        )?,
    )
}

/// Parses the supported non-interactive export command.
///
/// # Errors
///
/// Returns [`CliError`] for missing, duplicate, unknown, or malformed options.
pub fn parse_export_track(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<ExportTrackArgs, CliError> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("export-track")) {
        return Err(CliError::Usage);
    }
    let mut values = ParsedValues::default();
    while let Some(flag) = arguments.next() {
        let flag_text = flag.to_str().ok_or(CliError::NonUtf8Value("option"))?;
        let value = arguments
            .next()
            .ok_or(CliError::MissingOption("option value"))?;
        values.set(flag_text, value)?;
    }
    values.finish()
}

#[derive(Default)]
struct ParsedValues {
    project: Option<OsString>,
    sequence: Option<OsString>,
    track: Option<OsString>,
    output: Option<OsString>,
    width: Option<OsString>,
    height: Option<OsString>,
    fps: Option<OsString>,
    ffmpeg: Option<OsString>,
    ffprobe: Option<OsString>,
}

impl ParsedValues {
    fn set(&mut self, flag: &str, value: OsString) -> Result<(), CliError> {
        let destination = match flag {
            "--project" => &mut self.project,
            "--sequence" => &mut self.sequence,
            "--track" => &mut self.track,
            "--output" => &mut self.output,
            "--width" => &mut self.width,
            "--height" => &mut self.height,
            "--fps" => &mut self.fps,
            "--ffmpeg" => &mut self.ffmpeg,
            "--ffprobe" => &mut self.ffprobe,
            _ => return Err(CliError::InvalidOption(flag.to_owned())),
        };
        if destination.replace(value).is_some() {
            return Err(CliError::InvalidOption(flag.to_owned()));
        }
        Ok(())
    }

    fn finish(self) -> Result<ExportTrackArgs, CliError> {
        Ok(ExportTrackArgs {
            project: PathBuf::from(required(self.project, "--project")?),
            sequence_id: SequenceId::from_uuid(parse_uuid(
                required(self.sequence, "--sequence")?,
                "--sequence",
            )?),
            track_id: TrackId::from_uuid(parse_uuid(required(self.track, "--track")?, "--track")?),
            output: PathBuf::from(required(self.output, "--output")?),
            width: parse_u32(required(self.width, "--width")?, "--width")?,
            height: parse_u32(required(self.height, "--height")?, "--height")?,
            frame_rate: parse_rate(required(self.fps, "--fps")?)?,
            ffmpeg: PathBuf::from(required(self.ffmpeg, "--ffmpeg")?),
            ffprobe: PathBuf::from(required(self.ffprobe, "--ffprobe")?),
        })
    }
}

fn required(value: Option<OsString>, option: &'static str) -> Result<OsString, CliError> {
    value.ok_or(CliError::MissingOption(option))
}

fn parse_uuid(value: OsString, option: &'static str) -> Result<Uuid, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value(option))?;
    Uuid::parse_str(&value).map_err(|_| CliError::InvalidValue { option, value })
}

fn parse_u32(value: OsString, option: &'static str) -> Result<u32, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value(option))?;
    value
        .parse()
        .map_err(|_| CliError::InvalidValue { option, value })
}

fn parse_rate(value: OsString) -> Result<RationalTime, CliError> {
    let value = value
        .into_string()
        .map_err(|_| CliError::NonUtf8Value("--fps"))?;
    let Some((numerator, denominator)) = value.split_once('/') else {
        return Err(CliError::InvalidValue {
            option: "--fps",
            value,
        });
    };
    let numerator = numerator
        .parse::<i64>()
        .map_err(|_| CliError::InvalidValue {
            option: "--fps",
            value: value.clone(),
        })?;
    let denominator = denominator
        .parse::<u32>()
        .map_err(|_| CliError::InvalidValue {
            option: "--fps",
            value: value.clone(),
        })?;
    RationalTime::new(numerator, denominator).map_err(CliError::from)
}

#[cfg(test)]
mod tests {
    use super::{CliError, parse_export_track};
    use prism_timeline::RationalTime;
    use std::ffi::OsString;

    fn valid_arguments() -> Vec<OsString> {
        [
            "export-track",
            "--project",
            "edit.prism",
            "--sequence",
            "11111111-1111-4111-8111-111111111111",
            "--track",
            "22222222-2222-4222-8222-222222222222",
            "--output",
            "finish.mp4",
            "--width",
            "1920",
            "--height",
            "1080",
            "--fps",
            "30000/1001",
            "--ffmpeg",
            "/tools/ffmpeg",
            "--ffprobe",
            "/tools/ffprobe",
        ]
        .into_iter()
        .map(OsString::from)
        .collect()
    }

    #[test]
    fn parses_typed_export_track_command() -> Result<(), Box<dyn std::error::Error>> {
        let parsed = parse_export_track(valid_arguments())?;
        assert_eq!(parsed.width, 1920);
        assert_eq!(parsed.height, 1080);
        assert_eq!(parsed.frame_rate, RationalTime::new(30_000, 1_001)?);
        assert_eq!(
            parsed.sequence_id.as_uuid().to_string(),
            "11111111-1111-4111-8111-111111111111"
        );
        Ok(())
    }

    #[test]
    fn rejects_duplicate_and_missing_options() {
        let mut duplicate = valid_arguments();
        duplicate.extend([OsString::from("--width"), OsString::from("640")]);
        assert!(matches!(
            parse_export_track(duplicate),
            Err(CliError::InvalidOption(option)) if option == "--width"
        ));
        assert!(matches!(
            parse_export_track([OsString::from("export-track")]),
            Err(CliError::MissingOption("--project"))
        ));
    }

    #[test]
    fn rejects_malformed_frame_rate() {
        let mut arguments = valid_arguments();
        let fps = arguments
            .iter_mut()
            .skip_while(|argument| argument.as_os_str() != "--fps")
            .nth(1);
        if let Some(fps) = fps {
            *fps = OsString::from("29.97");
        }
        assert!(matches!(
            parse_export_track(arguments),
            Err(CliError::InvalidValue {
                option: "--fps",
                ..
            })
        ));
    }
}
