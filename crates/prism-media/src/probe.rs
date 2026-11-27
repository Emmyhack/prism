use super::{
    AtomicBool, AudioMetadata, BTreeSet, Command, Deserialize, Digest, Duration, File,
    FrameRateEvidence, MAX_REFERENCE_AUDIO_BYTES, MAX_REFERENCE_FRAME_BYTES, MediaError,
    MediaFingerprint, MediaMetadata, MediaProcessControl, Ordering, PacketTimingEvidence,
    PacketTimingReport, Path, PathBuf, ProxyProfile, RationalTime, Read, RoundingMode,
    RuntimeCapabilities, Sha256, StreamKind, StreamMetadata, VideoMetadata, supervised_process,
};

/// One tightly packed, top-to-bottom RGBA frame from the reference decoder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedVideoFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Four bytes per pixel in red, green, blue, alpha order.
    pub rgba: Vec<u8>,
}

impl DecodedVideoFrame {
    /// Returns a lowercase SHA-256 digest of the decoded pixel bytes.
    #[must_use]
    pub fn sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.rgba))
    }

    /// Returns one RGBA pixel when `(x, y)` is inside the frame.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidFrameDimensions`] or
    /// [`MediaError::InvalidFrameLength`] when the frame storage is invalid.
    pub fn pixel(&self, x: u32, y: u32) -> Result<Option<[u8; 4]>, MediaError> {
        let expected = checked_rgba_length(self.width, self.height)?;
        if self.rgba.len() != expected {
            return Err(MediaError::InvalidFrameLength {
                expected,
                actual: self.rgba.len(),
            });
        }
        if x >= self.width || y >= self.height {
            return Ok(None);
        }
        let index = usize::try_from((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4)
            .map_err(|_| MediaError::InvalidFrameDimensions {
                width: self.width,
                height: self.height,
            })?;
        Ok(Some(
            self.rgba[index..index + 4].try_into().unwrap_or([0; 4]),
        ))
    }

    /// Returns one packed RGBA row, or `None` when `y` is out of range.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame dimensions or packed RGBA buffer length
    /// are inconsistent.
    pub fn row_rgba(&self, y: u32) -> Result<Option<&[u8]>, MediaError> {
        let expected = checked_rgba_length(self.width, self.height)?;
        if self.rgba.len() != expected {
            return Err(MediaError::InvalidFrameLength {
                expected,
                actual: self.rgba.len(),
            });
        }
        if y >= self.height {
            return Ok(None);
        }
        let row_length = usize::try_from(self.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or(MediaError::InvalidFrameDimensions {
                width: self.width,
                height: self.height,
            })?;
        let start = usize::try_from(y)
            .ok()
            .and_then(|row| row.checked_mul(row_length))
            .ok_or(MediaError::InvalidFrameDimensions {
                width: self.width,
                height: self.height,
            })?;
        Ok(Some(&self.rgba[start..start + row_length]))
    }

    /// Creates a deterministic nearest-neighbor thumbnail in RGBA order.
    ///
    /// This CPU helper is intentionally simple and stable for cache fixtures;
    /// quality filters and GPU scaling belong to later render backends.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidFrameDimensions`] for invalid target or
    /// source dimensions, or [`MediaError::InvalidFrameLength`] when the
    /// source bytes do not match its declared dimensions.
    #[allow(clippy::cast_possible_truncation)]
    pub fn thumbnail(&self, width: u32, height: u32) -> Result<Self, MediaError> {
        let expected_source = checked_rgba_length(self.width, self.height)?;
        if self.rgba.len() != expected_source {
            return Err(MediaError::InvalidFrameLength {
                expected: expected_source,
                actual: self.rgba.len(),
            });
        }
        let target_length = checked_rgba_length(width, height)?;
        let mut rgba = vec![0_u8; target_length];
        let source_width = u64::from(self.width);
        let source_height = u64::from(self.height);
        let target_width = u64::from(width);
        let target_height = u64::from(height);
        for y in 0..target_height {
            let source_y = y * source_height / target_height;
            for x in 0..target_width {
                let source_x = x * source_width / target_width;
                let source_index = ((source_y * source_width + source_x) * 4) as usize;
                let target_index = ((y * target_width + x) * 4) as usize;
                rgba[target_index..target_index + 4]
                    .copy_from_slice(&self.rgba[source_index..source_index + 4]);
            }
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }
}

impl MediaFingerprint {
    /// Returns the exact byte length captured by the fingerprint.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Returns the lowercase hexadecimal full-file digest.
    #[must_use]
    pub fn sha256_hex(&self) -> &str {
        &self.sha256
    }

    /// Derives a content-addressed key for a media-derived artifact.
    ///
    /// The operation and parameters must describe every setting that affects
    /// the artifact (for example, thumbnail dimensions or waveform rate).
    #[must_use]
    pub fn cache_key(
        &self,
        namespace: &str,
        operation: &str,
        parameters: &[u8],
    ) -> prism_cache::CacheKey {
        prism_cache::CacheKey::from_parts(namespace, self.sha256.as_bytes(), operation, parameters)
    }

    /// Derives the stable cache key for a proxy profile.
    #[must_use]
    pub fn proxy_cache_key(&self, profile: &ProxyProfile) -> prism_cache::CacheKey {
        self.cache_key("proxy", "transcode", &profile.cache_parameters())
    }
}

/// Computes a deterministic full-file fingerprint in bounded memory.
///
/// # Errors
///
/// Returns [`MediaError::Process`] when the file cannot be opened or read.
pub fn fingerprint(path: &Path) -> Result<MediaFingerprint, MediaError> {
    fingerprint_impl(path, None)
}

/// Computes a fingerprint with cooperative deadline and cancellation checks.
///
/// Checks run before opening and between bounded file reads. OS-blocked file I/O
/// still requires platform qualification.
///
/// # Errors
/// Returns [`MediaError`] for file I/O failure, cancellation or deadline expiry.
pub fn fingerprint_with_control(
    path: &Path,
    control: MediaProcessControl<'_>,
) -> Result<MediaFingerprint, MediaError> {
    fingerprint_impl(path, Some(control))
}

fn fingerprint_impl(
    path: &Path,
    control: Option<MediaProcessControl<'_>>,
) -> Result<MediaFingerprint, MediaError> {
    let started = std::time::Instant::now();
    let check = || -> Result<(), MediaError> {
        if let Some(control) = control {
            if control.cancelled.load(Ordering::Acquire) {
                return Err(MediaError::OperationCancelled);
            }
            if started.elapsed() >= control.timeout {
                return Err(MediaError::OperationTimedOut);
            }
        }
        Ok(())
    };
    check()?;
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut byte_length = 0_u64;
    loop {
        check()?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        byte_length = byte_length
            .checked_add(
                u64::try_from(read).map_err(|error| {
                    MediaError::Process(std::io::Error::other(error.to_string()))
                })?,
            )
            .ok_or_else(|| MediaError::Process(std::io::Error::other("file length overflow")))?;
    }
    check()?;
    Ok(MediaFingerprint {
        sha256: format!("{:x}", digest.finalize()),
        byte_length,
    })
}

/// `ffprobe` process adapter with no ambient shell invocation.
#[derive(Clone, Debug)]
pub struct FfprobeInspector {
    executable: PathBuf,
}

/// `FFmpeg` command-line adapter used to establish decode and seek test vectors.
///
/// This is intentionally separate from the future isolated, library-backed
/// playback decoder and must not be used on the UI thread.
#[derive(Clone, Debug)]
pub struct FfmpegReferenceDecoder {
    executable: PathBuf,
}

impl FfmpegReferenceDecoder {
    /// Creates a decoder using an explicit executable path.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Decodes the first frame at or after an exact, nonnegative timestamp.
    ///
    /// `video_ordinal` is zero-based among video streams. The caller supplies
    /// dimensions obtained from inspected metadata; returned bytes are checked
    /// against those dimensions before a frame is constructed.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for invalid bounds, time conversion, process
    /// failure, or output that is not one tightly packed RGBA frame.
    pub fn decode_video_frame_rgba(
        &self,
        path: &Path,
        video_ordinal: u32,
        timestamp: RationalTime,
        width: u32,
        height: u32,
    ) -> Result<DecodedVideoFrame, MediaError> {
        self.decode_video_frame_rgba_with_control(
            path,
            video_ordinal,
            timestamp,
            width,
            height,
            MediaProcessControl {
                timeout: Duration::from_secs(30),
                cancelled: &AtomicBool::new(false),
            },
        )
    }

    /// Decodes one reference frame with bounded capture, a deadline and cancellation.
    ///
    /// # Errors
    /// Returns [`MediaError`] for invalid bounds, cancellation, timeout, excessive
    /// output, decoder failure or malformed pixels. The tool is killed and reaped.
    #[allow(clippy::too_many_arguments)]
    pub fn decode_video_frame_rgba_with_control(
        &self,
        path: &Path,
        video_ordinal: u32,
        timestamp: RationalTime,
        width: u32,
        height: u32,
        control: MediaProcessControl<'_>,
    ) -> Result<DecodedVideoFrame, MediaError> {
        let expected = checked_rgba_length(width, height)?;
        let seek = format_seek_timestamp(timestamp)?;
        let selection = format!("0:v:{video_ordinal}");
        let mut command = Command::new(&self.executable);
        command
            .args(["-v", "error", "-ss", &seek, "-i"])
            .arg(path)
            .args([
                "-map",
                &selection,
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ]);
        let output = supervised_process::capture(&mut command, expected, 65_536, control)?;
        if !output.status.success() {
            return Err(MediaError::Decoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if output.stdout.len() != expected {
            return Err(MediaError::InvalidFrameLength {
                expected,
                actual: output.stdout.len(),
            });
        }
        Ok(DecodedVideoFrame {
            width,
            height,
            rgba: output.stdout,
        })
    }

    /// Decodes a bounded mono `f32` sample window from the start of an audio stream.
    ///
    /// `audio_ordinal` is zero-based among audio streams. `FFmpeg` resamples to the
    /// requested rate and is given an exact duration ceiling derived from
    /// `max_samples`; returned bytes are independently bounded and validated.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for invalid bounds, process failure, excessive
    /// output, or a partial `f32` sample.
    pub fn decode_audio_mono_f32(
        &self,
        path: &Path,
        audio_ordinal: u32,
        sample_rate: u32,
        max_samples: usize,
    ) -> Result<Vec<f32>, MediaError> {
        self.decode_audio_mono_f32_with_control(
            path,
            audio_ordinal,
            sample_rate,
            max_samples,
            MediaProcessControl {
                timeout: Duration::from_secs(30),
                cancelled: &AtomicBool::new(false),
            },
        )
    }

    /// Decodes bounded mono samples with a deadline and caller cancellation.
    ///
    /// # Errors
    /// Returns [`MediaError`] for invalid bounds, cancellation, timeout, excessive
    /// output, decoder failure, or a partial `f32` sample.
    pub fn decode_audio_mono_f32_with_control(
        &self,
        path: &Path,
        audio_ordinal: u32,
        sample_rate: u32,
        max_samples: usize,
        control: MediaProcessControl<'_>,
    ) -> Result<Vec<f32>, MediaError> {
        let max_samples_u32 = u32::try_from(max_samples).ok();
        let maximum = max_samples.checked_mul(4);
        if sample_rate == 0
            || sample_rate > 384_000
            || max_samples_u32.is_none()
            || maximum.is_none_or(|bytes| bytes == 0 || bytes > MAX_REFERENCE_AUDIO_BYTES)
        {
            return Err(MediaError::InvalidAudioDecodeBounds {
                sample_rate,
                max_samples,
            });
        }
        let maximum = maximum.ok_or(MediaError::InvalidAudioDecodeBounds {
            sample_rate,
            max_samples,
        })?;
        let duration = format!(
            "{:.9}",
            f64::from(max_samples_u32.unwrap_or_default()) / f64::from(sample_rate)
        );
        let selection = format!("0:a:{audio_ordinal}");
        let sample_rate = sample_rate.to_string();
        let mut command = Command::new(&self.executable);
        command.args(["-v", "error", "-i"]).arg(path).args([
            "-map",
            &selection,
            "-t",
            &duration,
            "-ac",
            "1",
            "-ar",
            &sample_rate,
            "-f",
            "f32le",
            "pipe:1",
        ]);
        let output = supervised_process::capture(&mut command, maximum, 65_536, control)?;
        if !output.status.success() {
            return Err(MediaError::Decoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if output.stdout.len() > maximum || !output.stdout.len().is_multiple_of(4) {
            return Err(MediaError::InvalidAudioFrameLength {
                maximum,
                actual: output.stdout.len(),
            });
        }
        Ok(output
            .stdout
            .chunks_exact(4)
            .map(|bytes| {
                let mut sample = [0_u8; 4];
                sample.copy_from_slice(bytes);
                f32::from_le_bytes(sample)
            })
            .collect())
    }
}

pub(super) fn checked_rgba_length(width: u32, height: u32) -> Result<usize, MediaError> {
    let length = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4));
    match length {
        Some(length) if length > 0 && length <= MAX_REFERENCE_FRAME_BYTES => Ok(length),
        _ => Err(MediaError::InvalidFrameDimensions { width, height }),
    }
}

pub(super) fn format_seek_timestamp(timestamp: RationalTime) -> Result<String, MediaError> {
    let nanoseconds = timestamp.to_ticks(1_000_000_000, RoundingMode::NearestTiesAway)?;
    if nanoseconds < 0 {
        return Err(MediaError::NegativeDecodeTimestamp);
    }
    let seconds = nanoseconds / 1_000_000_000;
    let subsecond = nanoseconds % 1_000_000_000;
    Ok(format!("{seconds}.{subsecond:09}"))
}

impl FfprobeInspector {
    /// Creates an inspector using an explicit executable path.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Reports the installed tool version and build configuration.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when the process cannot run or reports failure.
    pub fn capabilities(&self) -> Result<RuntimeCapabilities, MediaError> {
        let output = Command::new(&self.executable).arg("-version").output()?;
        ensure_success(&output)?;
        let stdout =
            String::from_utf8(output.stdout).map_err(|_| MediaError::InvalidVersionOutput)?;
        let mut lines = stdout.lines();
        let version = lines.next().unwrap_or_default().to_owned();
        let configuration = lines
            .find_map(|line| line.strip_prefix("configuration: "))
            .map(str::to_owned);
        Ok(RuntimeCapabilities {
            ffprobe_version: version,
            build_configuration: configuration,
        })
    }

    /// Inspects container and stream metadata without decoding full media.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for process, input, JSON, or exact-time failures.
    pub fn inspect(&self, path: &Path) -> Result<MediaMetadata, MediaError> {
        self.inspect_with_control(
            path,
            MediaProcessControl {
                timeout: Duration::from_secs(30),
                cancelled: &AtomicBool::new(false),
            },
        )
    }

    /// Inspects metadata with bounded output, a deadline and caller cancellation.
    ///
    /// # Errors
    /// Returns [`MediaError`] for cancellation, timeout, excessive output, process
    /// failure or invalid metadata. The inspector is killed and reaped on failure.
    pub fn inspect_with_control(
        &self,
        path: &Path,
        control: MediaProcessControl<'_>,
    ) -> Result<MediaMetadata, MediaError> {
        let mut command = Command::new(&self.executable);
        command
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=format_name,duration:stream=index,codec_type,codec_name,width,height,pix_fmt,r_frame_rate,avg_frame_rate,time_base,sample_rate,channels,channel_layout,color_space,color_transfer,color_primaries:stream_tags=rotate,timecode",
                "-of",
                "json",
            ])
            .arg(path)
            ;
        let output = supervised_process::capture(&mut command, 4 * 1024 * 1024, 65_536, control)?;
        ensure_success(&output)?;
        let raw: ProbeOutput = serde_json::from_slice(&output.stdout)?;
        raw.try_into()
    }

    /// Inspects packet timestamps for one zero-based video-stream ordinal.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] for process, input, or JSON failures.
    pub fn inspect_video_packet_timing(
        &self,
        path: &Path,
        video_ordinal: u32,
    ) -> Result<PacketTimingReport, MediaError> {
        self.inspect_video_packet_timing_with_control(
            path,
            video_ordinal,
            MediaProcessControl {
                timeout: Duration::from_secs(30),
                cancelled: &AtomicBool::new(false),
            },
        )
    }

    /// Inspects packet timestamps with bounded output, a deadline and cancellation.
    ///
    /// # Errors
    /// Returns [`MediaError`] for cancellation, timeout, excessive output, process
    /// failure, invalid JSON, or invalid exact-time values.
    pub fn inspect_video_packet_timing_with_control(
        &self,
        path: &Path,
        video_ordinal: u32,
        control: MediaProcessControl<'_>,
    ) -> Result<PacketTimingReport, MediaError> {
        let selection = format!("v:{video_ordinal}");
        let mut command = Command::new(&self.executable);
        command
            .args([
                "-v",
                "error",
                "-select_streams",
                &selection,
                "-show_entries",
                "packet=pts,duration",
                "-of",
                "json",
            ])
            .arg(path);
        let output = supervised_process::capture(&mut command, 32 * 1024 * 1024, 65_536, control)?;
        ensure_success(&output)?;
        let raw: PacketOutput = serde_json::from_slice(&output.stdout)?;
        Ok(analyze_packet_timing(&raw.packets))
    }
}

fn ensure_success(output: &std::process::Output) -> Result<(), MediaError> {
    if output.status.success() {
        return Ok(());
    }
    Err(MediaError::Inspector {
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    format_name: Option<String>,
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    index: u32,
    codec_name: Option<String>,
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    time_base: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    channel_layout: Option<String>,
    color_space: Option<String>,
    color_transfer: Option<String>,
    color_primaries: Option<String>,
    tags: Option<ProbeTags>,
}

#[derive(Debug, Deserialize)]
struct ProbeTags {
    rotate: Option<String>,
    timecode: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PacketOutput {
    #[serde(default)]
    packets: Vec<ProbePacket>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub(super) struct ProbePacket {
    pub(super) pts: Option<i64>,
    pub(super) duration: Option<i64>,
}

pub(super) fn analyze_packet_timing(packets: &[ProbePacket]) -> PacketTimingReport {
    let durations = packets
        .iter()
        .filter_map(|packet| packet.duration)
        .filter(|duration| *duration > 0)
        .collect::<BTreeSet<_>>();
    let deltas = packets
        .windows(2)
        .filter_map(|window| window[1].pts?.checked_sub(window[0].pts?))
        .filter(|delta| *delta > 0)
        .collect::<BTreeSet<_>>();
    let usable_intervals = packets.len().saturating_sub(1);
    let evidence = if usable_intervals == 0 || (durations.is_empty() && deltas.is_empty()) {
        PacketTimingEvidence::Insufficient
    } else if durations.len() > 1 || deltas.len() > 1 {
        PacketTimingEvidence::ObservedVariable
    } else {
        PacketTimingEvidence::ObservedConstant
    };
    PacketTimingReport {
        packet_count: packets.len(),
        distinct_duration_ticks: durations.into_iter().collect(),
        evidence,
    }
}

impl TryFrom<ProbeOutput> for MediaMetadata {
    type Error = MediaError;

    fn try_from(raw: ProbeOutput) -> Result<Self, Self::Error> {
        let format_names = raw
            .format
            .as_ref()
            .and_then(|format| format.format_name.as_deref())
            .map(|names| names.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
        let duration = raw
            .format
            .and_then(|format| format.duration)
            .map(|value| parse_decimal_time("format.duration", &value))
            .transpose()?;
        let streams = raw
            .streams
            .into_iter()
            .map(StreamMetadata::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            format_names,
            duration,
            streams,
        })
    }
}

impl TryFrom<ProbeStream> for StreamMetadata {
    type Error = MediaError;

    fn try_from(raw: ProbeStream) -> Result<Self, Self::Error> {
        let time_base = parse_optional_fraction("stream.time_base", raw.time_base.as_deref())?;
        let kind = match raw.codec_type.as_deref() {
            Some("video") => {
                let nominal =
                    parse_optional_fraction("stream.r_frame_rate", raw.r_frame_rate.as_deref())?;
                let average = parse_optional_fraction(
                    "stream.avg_frame_rate",
                    raw.avg_frame_rate.as_deref(),
                )?;
                StreamKind::Video(VideoMetadata {
                    width: raw.width,
                    height: raw.height,
                    pixel_format: raw.pix_fmt,
                    nominal_frame_rate: nominal,
                    average_frame_rate: average,
                    frame_rate_evidence: classify_frame_rate(nominal, average),
                    color_primaries: raw.color_primaries,
                    color_transfer: raw.color_transfer,
                    color_space: raw.color_space,
                    rotation_degrees: raw
                        .tags
                        .as_ref()
                        .and_then(|tags| tags.rotate.as_deref())
                        .and_then(|value| value.parse().ok()),
                    timecode: raw.tags.and_then(|tags| tags.timecode),
                })
            }
            Some("audio") => StreamKind::Audio(AudioMetadata {
                sample_rate: raw
                    .sample_rate
                    .as_deref()
                    .and_then(|value| value.parse().ok()),
                channels: raw.channels,
                channel_layout: raw.channel_layout,
            }),
            _ => StreamKind::Other {
                codec_type: raw.codec_type,
            },
        };
        Ok(Self {
            index: raw.index,
            codec: raw.codec_name,
            time_base,
            kind,
        })
    }
}

fn classify_frame_rate(
    nominal: Option<RationalTime>,
    average: Option<RationalTime>,
) -> FrameRateEvidence {
    match (nominal, average) {
        (Some(left), Some(right)) if left == right => FrameRateEvidence::ConstantLikely,
        (Some(_), Some(_)) => FrameRateEvidence::VariableLikely,
        _ => FrameRateEvidence::Unknown,
    }
}

fn parse_optional_fraction(
    field: &'static str,
    value: Option<&str>,
) -> Result<Option<RationalTime>, MediaError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some((numerator, denominator)) = value.split_once('/') else {
        return Err(MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        });
    };
    let numerator = numerator
        .parse::<i64>()
        .map_err(|_| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    let denominator = denominator
        .parse::<u32>()
        .map_err(|_| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    if denominator == 0 {
        return Ok(None);
    }
    Ok(Some(RationalTime::new(numerator, denominator)?))
}

pub(super) fn parse_decimal_time(
    field: &'static str,
    value: &str,
) -> Result<RationalTime, MediaError> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 9 || whole.starts_with('-') {
        return Err(MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        });
    }
    let exponent = u32::try_from(fraction.len()).map_err(|_| MediaError::InvalidRational {
        field,
        value: value.to_owned(),
    })?;
    let scale = 10_u32
        .checked_pow(exponent)
        .ok_or_else(|| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    let whole = whole
        .parse::<i64>()
        .map_err(|_| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<i64>()
            .map_err(|_| MediaError::InvalidRational {
                field,
                value: value.to_owned(),
            })?
    };
    let ticks = whole
        .checked_mul(i64::from(scale))
        .and_then(|integer| integer.checked_add(fraction))
        .ok_or_else(|| MediaError::InvalidRational {
            field,
            value: value.to_owned(),
        })?;
    Ok(RationalTime::new(ticks, scale)?)
}
