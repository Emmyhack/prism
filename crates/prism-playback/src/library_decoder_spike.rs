//! Feature-gated in-process `FFmpeg` decoder used only by the Task 1 playback spike.

use ffmpeg_next as ffmpeg;
use prism_media::DecodedVideoFrame;
use prism_timeline::RationalTime;
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Preview resolution selected before opening a decoder session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewMode {
    /// Decode at source dimensions.
    Full,
    /// Scale each dimension to one half.
    Half,
    /// Scale each dimension to one quarter.
    Quarter,
    /// Decode a caller-selected proxy source at its native dimensions.
    Proxy,
}

impl PreviewMode {
    const fn divisor(self) -> u32 {
        match self {
            Self::Full | Self::Proxy => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }
}

/// Measurements exposed by the spike decoder.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DecoderTelemetry {
    /// Frames returned without codec work.
    pub cache_hits: u64,
    /// Requests that performed a demuxer seek.
    pub cache_misses: u64,
    /// Frames decoded, including bounded prefetch.
    pub decoded_frames: u64,
    /// Frames currently retained in memory.
    pub cached_frames: usize,
    /// Approximate retained RGBA bytes.
    pub cached_bytes: usize,
}

/// Errors from the experimental library-backed boundary.
#[derive(Debug, Error)]
pub enum LibraryDecodeError {
    /// The requested source could not be opened or decoded.
    #[error("FFmpeg library decode failed: {0}")]
    Ffmpeg(String),
    /// No video stream was present.
    #[error("source has no video stream: {0}")]
    MissingVideo(PathBuf),
    /// Exact time conversion overflowed or was invalid.
    #[error("invalid decode timestamp")]
    InvalidTimestamp,
    /// Source or output dimensions were invalid.
    #[error("invalid video dimensions: {width}x{height}")]
    InvalidDimensions {
        /// Invalid width.
        width: u32,
        /// Invalid height.
        height: u32,
    },
    /// The decoder reached EOF before a timestamped frame satisfied the request.
    #[error("no decoded frame at or after the requested timestamp")]
    FrameUnavailable,
}

/// One reusable source session with bounded RGBA caching and forward prefetch.
pub struct LibraryVideoDecoder {
    path: PathBuf,
    input: ffmpeg::format::context::Input,
    stream_index: usize,
    stream_time_base: ffmpeg::Rational,
    decoder: ffmpeg::decoder::Video,
    scaler: ffmpeg::software::scaling::Context,
    output_width: u32,
    output_height: u32,
    cache: BTreeMap<i64, DecodedVideoFrame>,
    cache_order: VecDeque<i64>,
    cache_bytes: usize,
    cache_limit_bytes: usize,
    prefetch_frames: usize,
    telemetry: DecoderTelemetry,
}

impl LibraryVideoDecoder {
    /// Opens one source and initializes demux, decode and RGBA scaling state.
    ///
    /// # Errors
    ///
    /// Returns an explicit error for missing streams, invalid dimensions or an
    /// `FFmpeg` initialization failure.
    pub fn open(
        path: impl AsRef<Path>,
        mode: PreviewMode,
        cache_limit_bytes: usize,
        prefetch_frames: usize,
    ) -> Result<Self, LibraryDecodeError> {
        ffmpeg::init().map_err(ffmpeg_error)?;
        let path = path.as_ref().to_path_buf();
        let input = ffmpeg::format::input(&path).map_err(ffmpeg_error)?;
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| LibraryDecodeError::MissingVideo(path.clone()))?;
        let stream_index = stream.index();
        let stream_time_base = stream.time_base();
        let parameters = stream.parameters();
        let context =
            ffmpeg::codec::context::Context::from_parameters(parameters).map_err(ffmpeg_error)?;
        let decoder = context.decoder().video().map_err(ffmpeg_error)?;
        let divisor = mode.divisor();
        let output_width = decoder.width().checked_div(divisor).unwrap_or(0).max(1);
        let output_height = decoder.height().checked_div(divisor).unwrap_or(0).max(1);
        if decoder.width() == 0 || decoder.height() == 0 {
            return Err(LibraryDecodeError::InvalidDimensions {
                width: decoder.width(),
                height: decoder.height(),
            });
        }
        let scaler = ffmpeg::software::scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGBA,
            output_width,
            output_height,
            ffmpeg::software::scaling::flag::Flags::BILINEAR,
        )
        .map_err(ffmpeg_error)?;
        Ok(Self {
            path,
            input,
            stream_index,
            stream_time_base,
            decoder,
            scaler,
            output_width,
            output_height,
            cache: BTreeMap::new(),
            cache_order: VecDeque::new(),
            cache_bytes: 0,
            cache_limit_bytes,
            prefetch_frames,
            telemetry: DecoderTelemetry::default(),
        })
    }

    /// Returns the opened source path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns current cache/decode counters.
    #[must_use]
    pub fn telemetry(&self) -> DecoderTelemetry {
        DecoderTelemetry {
            cached_frames: self.cache.len(),
            cached_bytes: self.cache_bytes,
            ..self.telemetry
        }
    }

    /// Seeks and decodes the first frame at or after `time`.
    ///
    /// A repeated exact request is served from the bounded cache. A miss seeks
    /// the reusable demuxer/decoder session and retains a bounded number of
    /// following frames for forward playback.
    ///
    /// # Errors
    ///
    /// Returns an explicit timestamp, seek, decode, scale or EOF error.
    pub fn decode_at(
        &mut self,
        time: RationalTime,
    ) -> Result<DecodedVideoFrame, LibraryDecodeError> {
        let target_tick = stream_tick(time, self.stream_time_base)?;
        if let Some(frame) = self.cache.get(&target_tick).cloned() {
            self.telemetry.cache_hits = self.telemetry.cache_hits.saturating_add(1);
            return Ok(frame);
        }
        self.telemetry.cache_misses = self.telemetry.cache_misses.saturating_add(1);
        let seek_timestamp = microseconds(time)?;
        self.input
            .seek(seek_timestamp, ..seek_timestamp)
            .map_err(ffmpeg_error)?;
        self.decoder.flush();

        let mut decoded_count = 0_u64;
        let mut decoded_frames = Vec::with_capacity(self.prefetch_frames.saturating_add(1));
        let mut prefetched = 0_usize;
        let input = &mut self.input;
        let decoder = &mut self.decoder;
        let scaler = &mut self.scaler;
        'packets: for (stream, packet) in input.packets() {
            if stream.index() != self.stream_index {
                continue;
            }
            decoder.send_packet(&packet).map_err(ffmpeg_error)?;
            loop {
                let mut source_frame = ffmpeg::util::frame::Video::empty();
                if decoder.receive_frame(&mut source_frame).is_err() {
                    break;
                }
                decoded_count = decoded_count.saturating_add(1);
                let Some(timestamp) = source_frame.timestamp().or_else(|| source_frame.pts())
                else {
                    continue;
                };
                if timestamp < target_tick {
                    continue;
                }
                let frame =
                    scale_frame(scaler, &source_frame, self.output_width, self.output_height)?;
                let key = if decoded_frames.is_empty() {
                    target_tick
                } else {
                    prefetched = prefetched.saturating_add(1);
                    timestamp
                };
                decoded_frames.push((key, frame));
                if self.prefetch_frames == 0 || prefetched >= self.prefetch_frames {
                    break 'packets;
                }
            }
        }
        self.telemetry.decoded_frames = self.telemetry.decoded_frames.saturating_add(decoded_count);
        let selected = decoded_frames.first().map(|(_, frame)| frame.clone());
        for (key, frame) in decoded_frames {
            self.insert_cache(key, frame);
        }
        selected.ok_or(LibraryDecodeError::FrameUnavailable)
    }

    fn insert_cache(&mut self, key: i64, frame: DecodedVideoFrame) {
        if self.cache_limit_bytes == 0 || frame.rgba.len() > self.cache_limit_bytes {
            return;
        }
        if let Some(previous) = self.cache.remove(&key) {
            self.cache_bytes = self.cache_bytes.saturating_sub(previous.rgba.len());
            self.cache_order.retain(|cached| *cached != key);
        }
        while self
            .cache_bytes
            .checked_add(frame.rgba.len())
            .is_none_or(|bytes| bytes > self.cache_limit_bytes)
        {
            let Some(oldest) = self.cache_order.pop_front() else {
                break;
            };
            if let Some(removed) = self.cache.remove(&oldest) {
                self.cache_bytes = self.cache_bytes.saturating_sub(removed.rgba.len());
            }
        }
        self.cache_bytes = self.cache_bytes.saturating_add(frame.rgba.len());
        self.cache_order.push_back(key);
        self.cache.insert(key, frame);
    }
}

fn scale_frame(
    scaler: &mut ffmpeg::software::scaling::Context,
    decoded: &ffmpeg::util::frame::Video,
    output_width: u32,
    output_height: u32,
) -> Result<DecodedVideoFrame, LibraryDecodeError> {
    let mut rgba = ffmpeg::util::frame::Video::empty();
    scaler.run(decoded, &mut rgba).map_err(ffmpeg_error)?;
    let row_bytes = usize::try_from(output_width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or(LibraryDecodeError::InvalidDimensions {
            width: output_width,
            height: output_height,
        })?;
    let byte_length = row_bytes
        .checked_mul(usize::try_from(output_height).unwrap_or(usize::MAX))
        .ok_or(LibraryDecodeError::InvalidDimensions {
            width: output_width,
            height: output_height,
        })?;
    let mut pixels = Vec::with_capacity(byte_length);
    let data = rgba.data(0);
    for row in 0..usize::try_from(output_height).unwrap_or(0) {
        let start =
            row.checked_mul(rgba.stride(0))
                .ok_or(LibraryDecodeError::InvalidDimensions {
                    width: output_width,
                    height: output_height,
                })?;
        let end = start
            .checked_add(row_bytes)
            .ok_or(LibraryDecodeError::InvalidDimensions {
                width: output_width,
                height: output_height,
            })?;
        let row = data
            .get(start..end)
            .ok_or(LibraryDecodeError::FrameUnavailable)?;
        pixels.extend_from_slice(row);
    }
    Ok(DecodedVideoFrame {
        width: output_width,
        height: output_height,
        rgba: pixels,
    })
}

fn ffmpeg_error(error: ffmpeg::Error) -> LibraryDecodeError {
    LibraryDecodeError::Ffmpeg(error.to_string())
}

fn microseconds(time: RationalTime) -> Result<i64, LibraryDecodeError> {
    scaled_ticks(time, 1, 1_000_000)
}

fn stream_tick(time: RationalTime, time_base: ffmpeg::Rational) -> Result<i64, LibraryDecodeError> {
    let numerator = i64::from(time_base.numerator());
    let denominator = i64::from(time_base.denominator());
    if numerator <= 0 || denominator <= 0 {
        return Err(LibraryDecodeError::InvalidTimestamp);
    }
    scaled_ticks(time, numerator, denominator)
}

fn scaled_ticks(
    time: RationalTime,
    unit_numerator: i64,
    unit_denominator: i64,
) -> Result<i64, LibraryDecodeError> {
    if !time.is_non_negative() || unit_numerator <= 0 || unit_denominator <= 0 {
        return Err(LibraryDecodeError::InvalidTimestamp);
    }
    let numerator = i128::from(time.value())
        .checked_mul(i128::from(unit_denominator))
        .ok_or(LibraryDecodeError::InvalidTimestamp)?;
    let denominator = i128::from(time.timescale())
        .checked_mul(i128::from(unit_numerator))
        .ok_or(LibraryDecodeError::InvalidTimestamp)?;
    i64::try_from(numerator / denominator).map_err(|_| LibraryDecodeError::InvalidTimestamp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn exact_time_conversion_uses_floor_without_floats() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(microseconds(RationalTime::new(1, 3)?)?, 333_333);
        assert_eq!(
            stream_tick(RationalTime::new(1, 2)?, ffmpeg::Rational(1, 30))?,
            15
        );
        assert!(microseconds(RationalTime::new(-1, 1)?).is_err());
        Ok(())
    }

    #[test]
    #[ignore = "requires PRISM_TASK1_MEDIA and linked FFmpeg libraries"]
    fn decodes_long_media_and_reuses_exact_cache_entry() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os("PRISM_TASK1_MEDIA") else {
            return Ok(());
        };
        let mut decoder = LibraryVideoDecoder::open(path, PreviewMode::Quarter, 32 << 20, 2)?;
        let first = decoder.decode_at(RationalTime::new(5, 1)?)?;
        let repeated = decoder.decode_at(RationalTime::new(5, 1)?)?;
        assert_eq!(first.sha256(), repeated.sha256());
        assert_eq!(decoder.telemetry().cache_hits, 1);
        assert!(decoder.telemetry().cached_frames >= 1);
        Ok(())
    }

    #[test]
    #[ignore = "requires PRISM_TASK1_MEDIA, PRISM_TASK1_PROXY and linked FFmpeg libraries"]
    fn benchmarks_preview_modes_on_long_media() -> Result<(), Box<dyn std::error::Error>> {
        let Some(source) = std::env::var_os("PRISM_TASK1_MEDIA") else {
            return Ok(());
        };
        let Some(proxy) = std::env::var_os("PRISM_TASK1_PROXY") else {
            return Ok(());
        };
        for (label, path, mode) in [
            ("full", PathBuf::from(&source), PreviewMode::Full),
            ("half", PathBuf::from(&source), PreviewMode::Half),
            ("quarter", PathBuf::from(&source), PreviewMode::Quarter),
            ("proxy", PathBuf::from(&proxy), PreviewMode::Proxy),
        ] {
            let mut decoder = LibraryVideoDecoder::open(path, mode, 96 << 20, 4)?;
            let mut seeks = Vec::new();
            for seconds in [0, 60, 120, 180, 240, 300, 360, 420, 480, 540] {
                let started = Instant::now();
                let _ = decoder.decode_at(RationalTime::new(seconds, 1)?)?;
                seeks.push(started.elapsed());
            }
            seeks.sort_unstable();
            let median = seeks[seeks.len() / 2];
            let maximum = seeks.last().copied().ok_or("no seek samples")?;

            let frame_budget = Duration::from_micros(33_333);
            let mut dropped = 0_usize;
            let mut scrub_samples = Vec::new();
            for frame in 0..60 {
                let started = Instant::now();
                let _ = decoder.decode_at(RationalTime::new(9_000 + frame, 30)?)?;
                let elapsed = started.elapsed();
                if elapsed > frame_budget {
                    dropped = dropped.saturating_add(1);
                }
                scrub_samples.push(elapsed);
            }
            scrub_samples.sort_unstable();
            let scrub_p95 = scrub_samples[56];
            let telemetry = decoder.telemetry();
            println!(
                "task1 mode={label} seek_median_ms={:.3} seek_max_ms={:.3} scrub_p95_ms={:.3} dropped_30fps={dropped}/60 cache_hits={} cache_misses={} decoded_frames={} cached_frames={} cached_bytes={}",
                median.as_secs_f64() * 1_000.0,
                maximum.as_secs_f64() * 1_000.0,
                scrub_p95.as_secs_f64() * 1_000.0,
                telemetry.cache_hits,
                telemetry.cache_misses,
                telemetry.decoded_frames,
                telemetry.cached_frames,
                telemetry.cached_bytes,
            );
        }
        Ok(())
    }
}
