//! Native Slint adapter for the Prism desktop workspace.

use crate::stream_preview::{StreamControl, StreamPreview};
use crate::view_preferences::ViewPreferences;
use prism_cache::{CacheError, CacheEvictionPolicy, CacheEvictionWorker, CacheLayout};
use prism_commands::{Command, CommandEngine, UndoRecord};
use prism_core::{ClipId, MarkerId, MediaAssetId, SequenceId, TrackId, TransitionId};
use prism_export::{ExportPreset, SequenceMp4ExportRequest};
use prism_media::{
    FfmpegProxyGenerator, FfmpegReferenceDecoder, FfprobeInspector, MediaArtifactKind,
    MediaArtifactWorker, MediaCatalogEntry, MediaFingerprint, MediaItemKind, ProxyProfile,
    ProxyRequest, ProxyStatus, StreamKind, decode_thumbnail_artifact, fingerprint,
};
use prism_project::{
    MediaAsset, Project, ProxySourceFingerprint, Sequence, TrackKind, TransitionKind,
};
use prism_studio::desktop::{
    AutosavePreferences, AutosavePreferencesStore, ExportController, ExportStatus, ExportWorker,
    ExportWorkerEvent,
};
use prism_studio::waveform::{WaveformRequest, WaveformWorker};
use prism_timeline::RationalTime;
use slint::Model;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Stdio},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};
use thiserror::Error;

slint::include_modules!();

const PROXY_AVAILABLE: &str = "AVAILABLE";
const PROXY_MISSING: &str = "MISSING";
const PROXY_STALE: &str = "STALE";
const BROWSER_THUMBNAIL_VARIANT: &str = "browser:max-edge=160";

fn media_tool(name: &str) -> PathBuf {
    let environment = match name {
        "ffmpeg" => "PRISM_FFMPEG",
        "ffprobe" => "PRISM_FFPROBE",
        "ffplay" => "PRISM_FFPLAY",
        _ => return PathBuf::from(name),
    };
    if let Some(path) = std::env::var_os(environment).filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(executable_directory) = executable.parent()
    {
        let sibling = executable_directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if sibling.is_file() {
            return sibling;
        }
        if let Some(contents) = executable_directory.parent() {
            let packaged = contents.join("Resources").join("bin").join(name);
            if packaged.is_file() {
                return packaged;
            }
        }
    }
    PathBuf::from(name)
}

fn media_runtime_status() -> Result<String, String> {
    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for name in ["ffmpeg", "ffprobe", "ffplay"] {
        let path = media_tool(name);
        match ProcessCommand::new(&path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => resolved.push(path.display().to_string()),
            _ => missing.push(name),
        }
    }
    if !missing.is_empty() {
        return Err(format!("Configure or package: {}", missing.join(", ")));
    }

    let ffmpeg = media_tool("ffmpeg");
    let encoders = ProcessCommand::new(&ffmpeg)
        .args(["-hide_banner", "-encoders"])
        .output()
        .map_err(|error| format!("Cannot inspect FFmpeg encoders: {error}"))?;
    let muxers = ProcessCommand::new(&ffmpeg)
        .args(["-hide_banner", "-muxers"])
        .output()
        .map_err(|error| format!("Cannot inspect FFmpeg muxers: {error}"))?;
    if !encoders.status.success() || !muxers.status.success() {
        return Err("FFmpeg capability inspection failed".to_owned());
    }
    let encoders = String::from_utf8_lossy(&encoders.stdout);
    let muxers = String::from_utf8_lossy(&muxers.stdout);
    let mut unavailable = Vec::new();
    if !encoders.contains("libx264") {
        unavailable.push("H.264 (libx264) encoder");
    }
    if !encoders.lines().any(|line| line.contains(" aac ")) {
        unavailable.push("AAC encoder");
    }
    if !muxers
        .lines()
        .any(|line| line.contains(" mp4 ") || line.contains(",mp4"))
    {
        unavailable.push("MP4 muxer");
    }
    if !unavailable.is_empty() {
        return Err(format!(
            "FFmpeg is missing required export support: {}",
            unavailable.join(", ")
        ));
    }
    Ok(format!(
        "H.264 High / AAC / MP4 ready · {}",
        resolved.join(" · ")
    ))
}

fn update_media_runtime_status(ui: &PrismStudio) {
    match media_runtime_status() {
        Ok(detail) => {
            ui.set_codec_status("READY".into());
            ui.set_codec_detail(detail.into());
        }
        Err(detail) => {
            ui.set_codec_status("MISSING".into());
            ui.set_codec_detail(detail.into());
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    const GIB: u64 = MIB * 1_024;
    let decimal = |unit: u64| ((bytes % unit).saturating_mul(10) / unit).min(9);
    if bytes >= GIB {
        format!("{}.{:01} GiB", bytes / GIB, decimal(GIB))
    } else if bytes >= MIB {
        format!("{}.{:01} MiB", bytes / MIB, decimal(MIB))
    } else if bytes >= KIB {
        format!("{}.{:01} KiB", bytes / KIB, decimal(KIB))
    } else {
        format!("{bytes} B")
    }
}

fn waveform_bars(points: &[prism_audio::WaveformPoint], bar_count: usize) -> Vec<f32> {
    if points.is_empty() || bar_count == 0 {
        return Vec::new();
    }
    let chunk_size = points.len().div_ceil(bar_count).max(1);
    points
        .chunks(chunk_size)
        .map(|chunk| {
            chunk
                .iter()
                .map(|point| point.min.abs().max(point.max.abs()))
                .fold(0.0_f32, f32::max)
                .clamp(0.0, 1.0)
        })
        .collect()
}

fn waveform_range(bars: &[f32], start_ratio: f32, width_ratio: f32) -> Vec<f32> {
    if bars.is_empty() || width_ratio <= 0.0 {
        return Vec::new();
    }
    let len = bars.len();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    let start = (start_ratio.clamp(0.0, 1.0) * len as f32).floor() as usize;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    let end = ((start_ratio + width_ratio).clamp(0.0, 1.0) * len as f32).ceil() as usize;
    bars[start.min(len)..end.clamp(start.saturating_add(1), len)].to_vec()
}

fn luma_scope(rgba: &[u8], bin_count: usize, sample_ceiling: usize) -> Vec<f32> {
    if bin_count == 0 || sample_ceiling == 0 || rgba.len() < 4 {
        return Vec::new();
    }
    let pixel_count = rgba.len() / 4;
    let stride = pixel_count.div_ceil(sample_ceiling).max(1);
    let mut bins = vec![0_u32; bin_count];
    for pixel in rgba.chunks_exact(4).step_by(stride) {
        let luma =
            (u32::from(pixel[0]) * 54 + u32::from(pixel[1]) * 183 + u32::from(pixel[2]) * 19) >> 8;
        let index = (usize::try_from(luma).unwrap_or(255) * bin_count / 256).min(bin_count - 1);
        bins[index] = bins[index].saturating_add(1);
    }
    let peak = bins.iter().copied().max().unwrap_or(0);
    if peak == 0 {
        return vec![0.0; bin_count];
    }
    #[allow(clippy::cast_precision_loss)]
    bins.into_iter()
        .map(|count| count as f32 / peak as f32)
        .collect()
}

fn pixel_luma(pixel: &[u8]) -> f32 {
    let value =
        (u32::from(pixel[0]) * 54 + u32::from(pixel[1]) * 183 + u32::from(pixel[2]) * 19) >> 8;
    #[allow(clippy::cast_precision_loss)]
    let normalized = value as f32 / 255.0;
    normalized
}

fn luma_waveform(
    rgba: &[u8],
    width: u32,
    height: u32,
    column_count: usize,
    sample_ceiling: usize,
) -> Vec<LumaScopeColumn> {
    let Ok(width) = usize::try_from(width) else {
        return Vec::new();
    };
    let Ok(height) = usize::try_from(height) else {
        return Vec::new();
    };
    if width == 0
        || height == 0
        || column_count == 0
        || sample_ceiling == 0
        || rgba.len() < width.saturating_mul(height).saturating_mul(4)
    {
        return Vec::new();
    }
    let samples_per_column = sample_ceiling.div_ceil(column_count).max(1);
    let row_stride = height.div_ceil(samples_per_column).max(1);
    (0..column_count)
        .map(|column| {
            let x = column.saturating_mul(width) / column_count;
            let mut low = 1.0_f32;
            let mut high = 0.0_f32;
            for y in (0..height).step_by(row_stride) {
                let offset = (y.saturating_mul(width).saturating_add(x)).saturating_mul(4);
                let value = pixel_luma(&rgba[offset..offset + 4]);
                low = low.min(value);
                high = high.max(value);
            }
            LumaScopeColumn { low, high }
        })
        .collect()
}

fn vectorscope(rgba: &[u8], axis_bins: usize, sample_ceiling: usize) -> Vec<f32> {
    if axis_bins == 0 || sample_ceiling == 0 || rgba.len() < 4 {
        return Vec::new();
    }
    let pixel_count = rgba.len() / 4;
    let stride = pixel_count.div_ceil(sample_ceiling).max(1);
    let mut bins = vec![0_u32; axis_bins.saturating_mul(axis_bins)];
    for pixel in rgba.chunks_exact(4).step_by(stride) {
        let red = f32::from(pixel[0]) / 255.0;
        let green = f32::from(pixel[1]) / 255.0;
        let blue = f32::from(pixel[2]) / 255.0;
        let chroma_u = (-0.1146 * red - 0.3854 * green + 0.5 * blue + 0.5).clamp(0.0, 1.0);
        let chroma_v = (0.5 * red - 0.4542 * green - 0.0458 * blue + 0.5).clamp(0.0, 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            clippy::cast_sign_loss
        )]
        let x = (chroma_u * axis_bins as f32).floor() as usize;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            clippy::cast_sign_loss
        )]
        let y = ((1.0 - chroma_v) * axis_bins as f32).floor() as usize;
        let index = y.min(axis_bins - 1).saturating_mul(axis_bins) + x.min(axis_bins - 1);
        bins[index] = bins[index].saturating_add(1);
    }
    let peak = bins.iter().copied().max().unwrap_or(0);
    if peak == 0 {
        return vec![0.0; bins.len()];
    }
    #[allow(clippy::cast_precision_loss)]
    bins.into_iter()
        .map(|count| count as f32 / peak as f32)
        .collect()
}

fn proxy_status(
    source: &Path,
    proxy: Option<&Path>,
    recorded: Option<&ProxySourceFingerprint>,
) -> &'static str {
    let recorded = recorded.map(|fingerprint| MediaFingerprint {
        sha256: fingerprint.sha256.clone(),
        byte_length: fingerprint.byte_length,
    });
    match prism_media::proxy_status(source, proxy, recorded.as_ref()) {
        Ok(ProxyStatus::Available) => PROXY_AVAILABLE,
        Ok(ProxyStatus::Missing) => PROXY_MISSING,
        Ok(ProxyStatus::Stale) | Err(_) => PROXY_STALE,
    }
}

fn next_proxy_version(base: &Path) -> Result<PathBuf, String> {
    let parent = base
        .parent()
        .ok_or_else(|| "Proxy destination has no parent directory".to_owned())?;
    let stem = base
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Prism.proxy");
    let (stem, first_version) = stem
        .rsplit_once(".v")
        .and_then(|(root, version)| {
            version
                .parse::<u32>()
                .ok()
                .map(|version| (root, version + 1))
        })
        .unwrap_or((stem, 2));
    for version in first_version..=9_999 {
        let candidate = parent.join(format!("{stem}.v{version}.mp4"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("No available versioned proxy filename was found".to_owned())
}

#[derive(Debug, Eq, PartialEq)]
struct NativeExportStatus {
    label: &'static str,
    output: String,
    progress: i32,
    error: String,
    can_cancel: bool,
    can_retry: bool,
    can_remove: bool,
}

impl From<&ExportStatus> for NativeExportStatus {
    fn from(status: &ExportStatus) -> Self {
        Self {
            label: status.label,
            output: status.output.clone(),
            progress: i32::from(status.progress_percent.unwrap_or(0)),
            error: status.error.clone().unwrap_or_default(),
            can_cancel: status.can_cancel,
            can_retry: status.can_retry,
            can_remove: status.can_remove,
        }
    }
}

fn apply_export_status(ui: &PrismStudio, status: Option<&ExportStatus>) {
    if let Some(status) = status {
        let status = NativeExportStatus::from(status);
        ui.set_export_status(status.label.into());
        ui.set_export_output(status.output.into());
        ui.set_export_progress(status.progress);
        ui.set_export_error(status.error.into());
        ui.set_export_can_cancel(status.can_cancel);
        ui.set_export_can_retry(status.can_retry);
        ui.set_export_can_remove(status.can_remove);
    } else {
        ui.set_export_status("READY".into());
        ui.set_export_progress(-1);
        ui.set_export_error("".into());
        ui.set_export_can_cancel(false);
        ui.set_export_can_retry(false);
        ui.set_export_can_remove(false);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AudioPreviewSegment {
    clip_id: ClipId,
    path: PathBuf,
    source_time: RationalTime,
    remaining: RationalTime,
}

// Current preview supports one audible clip at a time; mixing is a separate engine task.
fn audio_preview_segment(project: &Project, playhead: RationalTime) -> Option<AudioPreviewSegment> {
    project
        .sequences
        .values()
        .next()?
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio && !track.muted)
        .flat_map(|track| &track.clips)
        .find_map(|clip| {
            let end = clip.timeline_start.checked_add(clip.source_duration).ok()?;
            if playhead < clip.timeline_start || playhead >= end {
                return None;
            }
            let asset = project.media.get(&clip.media_id)?;
            Some(AudioPreviewSegment {
                clip_id: clip.id,
                path: PathBuf::from(&asset.source_path),
                source_time: clip
                    .source_start
                    .checked_add(playhead.checked_sub(clip.timeline_start).ok()?)
                    .ok()?,
                remaining: end.checked_sub(playhead).ok()?,
            })
        })
}

fn program_clip_index(project: &Project, position: RationalTime) -> Option<i32> {
    let sequence = project.sequences.values().next()?;
    sequence
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
        .enumerate()
        .find_map(|(index, (track, clip))| {
            (track.kind == TrackKind::Video
                && track.visible
                && position >= clip.timeline_start
                && position < clip.timeline_start.checked_add(clip.source_duration).ok()?)
            .then(|| i32::try_from(index).ok())
            .flatten()
        })
}

fn split_all_commands(project: &Project, position: RationalTime) -> Result<Vec<Command>, String> {
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("Open a sequence first")?;
    let mut commands = Vec::new();
    for track in sequence.tracks.iter().filter(|track| !track.locked) {
        for clip in &track.clips {
            let end = clip
                .timeline_start
                .checked_add(clip.source_duration)
                .map_err(|e| e.to_string())?;
            if position > clip.timeline_start && position < end {
                commands.push(Command::SplitClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    right_clip_id: ClipId::new(),
                    timeline_position: position,
                });
            }
        }
    }
    if commands.is_empty() {
        return Err("No unlocked clip crosses the playhead".into());
    }
    Ok(commands)
}

fn aligned_av_commands(
    project: &Project,
    index: i32,
    action: &str,
    position: RationalTime,
) -> Result<Vec<Command>, String> {
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("Open a sequence first")?;
    let (track, clip) = sequence
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
        .nth(usize::try_from(index).map_err(|_| "Select a timeline clip first")?)
        .ok_or("Selected clip is no longer available")?;
    let matches = sequence
        .tracks
        .iter()
        .filter(|other| other.kind != track.kind)
        .flat_map(|other| other.clips.iter().map(move |candidate| (other, candidate)))
        .filter(|(_, candidate)| {
            candidate.media_id == clip.media_id
                && candidate.timeline_start == clip.timeline_start
                && candidate.source_start == clip.source_start
                && candidate.source_duration == clip.source_duration
        })
        .collect::<Vec<_>>();
    let [(other_track, other_clip)] = matches.as_slice() else {
        return Err(if matches.is_empty() {
            "No audio/video companion has matching source and timing"
        } else {
            "Multiple aligned companions found; edit clips individually"
        }
        .to_owned());
    };
    if track.locked || other_track.locked {
        return Err("Unlock both tracks before editing aligned A/V".into());
    }
    [(track, clip), (*other_track, *other_clip)]
        .into_iter()
        .map(|(track, clip)| match action {
            "timeline.split_av" => Ok(Command::SplitClip {
                sequence_id: sequence.id,
                track_id: track.id,
                clip_id: clip.id,
                right_clip_id: ClipId::new(),
                timeline_position: position,
            }),
            "timeline.move_av" => Ok(Command::MoveClip {
                sequence_id: sequence.id,
                track_id: track.id,
                clip_id: clip.id,
                timeline_start: position,
            }),
            "timeline.delete_av" => Ok(Command::DeleteClip {
                sequence_id: sequence.id,
                track_id: track.id,
                clip_id: clip.id,
            }),
            "timeline.ripple_delete_av" => Ok(Command::RippleDelete {
                sequence_id: sequence.id,
                track_id: track.id,
                clip_id: clip.id,
            }),
            _ => Err("Unsupported aligned A/V edit".into()),
        })
        .collect()
}

fn aligned_av_companion_index(project: &Project, index: i32) -> Option<usize> {
    let sequence = project.sequences.values().next()?;
    let selected = usize::try_from(index).ok()?;
    let (track, clip) = sequence
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
        .nth(selected)?;
    let matches = sequence
        .tracks
        .iter()
        .flat_map(|other| other.clips.iter().map(move |candidate| (other, candidate)))
        .enumerate()
        .filter(|(_, (other, candidate))| {
            other.kind != track.kind
                && candidate.media_id == clip.media_id
                && candidate.timeline_start == clip.timeline_start
                && candidate.source_start == clip.source_start
                && candidate.source_duration == clip.source_duration
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    (matches.len() == 1).then(|| matches[0])
}

fn move_selected_commands(
    project: &Project,
    index: i32,
    timeline_start: RationalTime,
) -> Result<Vec<Command>, String> {
    if aligned_av_companion_index(project, index).is_some() {
        return aligned_av_commands(project, index, "timeline.move_av", timeline_start);
    }
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or_else(|| "No sequence available".to_owned())?;
    let (track, clip) = sequence
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
        .nth(usize::try_from(index).map_err(|_| "Select a clip first".to_owned())?)
        .ok_or_else(|| "Selected clip unavailable".to_owned())?;
    Ok(vec![Command::MoveClip {
        sequence_id: sequence.id,
        track_id: track.id,
        clip_id: clip.id,
        timeline_start,
    }])
}

fn duplicate_selected_commands(project: &Project, index: i32) -> Result<Vec<Command>, String> {
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or_else(|| "No sequence available".to_owned())?;
    let selected = usize::try_from(index).map_err(|_| "Select a clip first".to_owned())?;
    let clips = sequence
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
        .collect::<Vec<_>>();
    let (track, clip) = clips
        .get(selected)
        .copied()
        .ok_or_else(|| "Selected clip is unavailable".to_owned())?;
    let companion = aligned_av_companion_index(project, index);
    let timeline_start = if companion.is_some() {
        sequence.duration().map_err(|error| error.to_string())?
    } else {
        track
            .clips
            .iter()
            .filter_map(|clip| clip.timeline_start.checked_add(clip.source_duration).ok())
            .max()
            .unwrap_or(RationalTime::ZERO)
    };
    let mut targets = vec![(track, clip)];
    if let Some(companion) = companion {
        targets.push(clips[companion]);
    }
    Ok(targets
        .into_iter()
        .map(|(track, clip)| Command::DuplicateClip {
            sequence_id: sequence.id,
            track_id: track.id,
            clip_id: clip.id,
            new_clip_id: ClipId::new(),
            timeline_start,
        })
        .collect())
}

fn slip_selected_commands(
    project: &Project,
    index: i32,
    source_delta: RationalTime,
) -> Result<Vec<Command>, String> {
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or_else(|| "No sequence available".to_owned())?;
    let selected = usize::try_from(index).map_err(|_| "Select a clip first".to_owned())?;
    let clips = sequence
        .tracks
        .iter()
        .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
        .collect::<Vec<_>>();
    let (track, clip) = clips
        .get(selected)
        .copied()
        .ok_or_else(|| "Selected clip unavailable".to_owned())?;
    let mut targets = vec![(track.id, clip.id)];
    if let Some(companion) = aligned_av_companion_index(project, index) {
        let (track, clip) = clips[companion];
        targets.push((track.id, clip.id));
    }
    Ok(targets
        .into_iter()
        .map(|(track_id, clip_id)| Command::SlipClip {
            sequence_id: sequence.id,
            track_id,
            clip_id,
            source_delta,
        })
        .collect())
}

fn append_source_commands(
    project: &Project,
    media_id: MediaAssetId,
    source_start: RationalTime,
    source_duration: RationalTime,
) -> Result<Vec<Command>, String> {
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or_else(|| "Import media to create a sequence first".to_owned())?;
    let (has_video, has_audio, _) = media_stream_summary(project, media_id);
    if !has_video && !has_audio {
        return Err("Selected media has no timeline stream classification".to_owned());
    }
    let timeline_start = sequence.duration().map_err(|error| error.to_string())?;
    let mut commands = Vec::new();
    for kind in [TrackKind::Video, TrackKind::Audio]
        .into_iter()
        .filter(|kind| {
            (*kind == TrackKind::Video && has_video) || (*kind == TrackKind::Audio && has_audio)
        })
    {
        let track_id = sequence
            .tracks
            .iter()
            .find(|track| track.kind == kind && !track.locked)
            .map_or_else(
                || {
                    let id = TrackId::new();
                    commands.push(Command::CreateTrack {
                        sequence_id: sequence.id,
                        id,
                        kind,
                    });
                    id
                },
                |track| track.id,
            );
        commands.push(Command::InsertClip {
            sequence_id: sequence.id,
            track_id,
            id: ClipId::new(),
            media_id,
            timeline_start,
            source_start,
            source_duration,
        });
    }
    Ok(commands)
}

/// Native source edits use explicit fixed first-video/first-audio destinations until
/// source patching is available. Plan everything before committing a single transaction.
fn source_edit_commands(
    project: &Project,
    media_id: MediaAssetId,
    source_start: RationalTime,
    source_duration: RationalTime,
    position: RationalTime,
    overwrite: bool,
) -> Result<Vec<Command>, String> {
    let sequence = project
        .sequences
        .values()
        .next()
        .ok_or("Open a sequence first")?;
    if !source_duration.is_positive()
        || source_start < RationalTime::ZERO
        || position < RationalTime::ZERO
    {
        return Err("Use a positive source range and a nonnegative playhead position".into());
    }
    source_start
        .checked_add(source_duration)
        .map_err(|e| e.to_string())?;
    let end = position
        .checked_add(source_duration)
        .map_err(|e| e.to_string())?;
    if !sequence.transitions.is_empty() {
        return Err(
            "Insert/Overwrite with transitions is not supported yet; remove the transitions first"
                .into(),
        );
    }
    if !project.media.contains_key(&media_id) {
        return Err("Selected source is unavailable".into());
    }
    let (video, audio, _) = media_stream_summary(project, media_id);
    if !video && !audio {
        return Err("Selected source has no classified streams".into());
    }
    let mut commands = Vec::new();
    let destinations = source_edit_destinations(sequence, video, audio, &mut commands)?;
    if overwrite {
        plan_source_overwrite(sequence, &destinations, position, end, &mut commands)?;
    } else {
        plan_source_insert(sequence, position, end, source_duration, &mut commands)?;
    }
    for track_id in destinations {
        commands.push(Command::InsertClip {
            sequence_id: sequence.id,
            track_id,
            id: ClipId::new(),
            media_id,
            timeline_start: position,
            source_start,
            source_duration,
        });
    }
    Ok(commands)
}

fn source_edit_destinations(
    sequence: &Sequence,
    video: bool,
    audio: bool,
    commands: &mut Vec<Command>,
) -> Result<Vec<TrackId>, String> {
    [TrackKind::Video, TrackKind::Audio]
        .into_iter()
        .filter(|kind| (*kind == TrackKind::Video && video) || (*kind == TrackKind::Audio && audio))
        .map(|kind| {
            if let Some(track) = sequence.tracks.iter().find(|track| track.kind == kind) {
                if track.locked {
                    return Err(
                        "Unlock the first destination video/audio track before editing".into(),
                    );
                }
                return Ok(track.id);
            }
            let id = TrackId::new();
            commands.push(Command::CreateTrack {
                sequence_id: sequence.id,
                id,
                kind,
            });
            Ok(id)
        })
        .collect()
}

fn plan_source_overwrite(
    sequence: &Sequence,
    destinations: &[TrackId],
    position: RationalTime,
    end: RationalTime,
    commands: &mut Vec<Command>,
) -> Result<(), String> {
    for track in sequence
        .tracks
        .iter()
        .filter(|track| destinations.contains(&track.id))
    {
        for clip in &track.clips {
            let clip_end = clip
                .timeline_start
                .checked_add(clip.source_duration)
                .map_err(|error| error.to_string())?;
            if clip.timeline_start >= end || clip_end <= position {
                continue;
            }
            commands.push(Command::DeleteClip {
                sequence_id: sequence.id,
                track_id: track.id,
                clip_id: clip.id,
            });
            if clip.timeline_start < position {
                commands.push(Command::InsertClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    id: clip.id,
                    media_id: clip.media_id,
                    timeline_start: clip.timeline_start,
                    source_start: clip.source_start,
                    source_duration: position
                        .checked_sub(clip.timeline_start)
                        .map_err(|error| error.to_string())?,
                });
            }
            if clip_end > end {
                commands.push(Command::InsertClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    id: if clip.timeline_start < position {
                        ClipId::new()
                    } else {
                        clip.id
                    },
                    media_id: clip.media_id,
                    timeline_start: end,
                    source_start: clip
                        .source_start
                        .checked_add(
                            end.checked_sub(clip.timeline_start)
                                .map_err(|error| error.to_string())?,
                        )
                        .map_err(|error| error.to_string())?,
                    source_duration: clip_end
                        .checked_sub(end)
                        .map_err(|error| error.to_string())?,
                });
            }
        }
    }
    Ok(())
}

fn plan_source_insert(
    sequence: &Sequence,
    position: RationalTime,
    end: RationalTime,
    source_duration: RationalTime,
    commands: &mut Vec<Command>,
) -> Result<(), String> {
    // Move from the end backwards so each intermediate command stays overlap-free.
    for track in sequence.tracks.iter().filter(|track| !track.locked) {
        for clip in track.clips.iter().rev() {
            let clip_end = clip
                .timeline_start
                .checked_add(clip.source_duration)
                .map_err(|error| error.to_string())?;
            if clip.timeline_start >= position {
                commands.push(Command::MoveClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    timeline_start: clip
                        .timeline_start
                        .checked_add(source_duration)
                        .map_err(|error| error.to_string())?,
                });
            } else if clip_end > position {
                let right = ClipId::new();
                commands.push(Command::SplitClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    right_clip_id: right,
                    timeline_position: position,
                });
                commands.push(Command::MoveClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: right,
                    timeline_start: end,
                });
            }
        }
    }
    Ok(())
}

fn media_usage(project: &Project) -> BTreeMap<MediaAssetId, usize> {
    let mut counts = BTreeMap::new();
    if let Some(sequence) = project.sequences.values().next() {
        for clip in sequence.tracks.iter().flat_map(|track| &track.clips) {
            *counts.entry(clip.media_id).or_default() += 1;
        }
    }
    counts
}

fn media_stream_summary(
    project: &Project,
    media_id: MediaAssetId,
) -> (bool, bool, Option<RationalTime>) {
    let mut has_video = false;
    let mut has_audio = false;
    let mut source_end = None;
    for (kind, clip) in project.sequences.values().flat_map(|sequence| {
        sequence
            .tracks
            .iter()
            .flat_map(|track| track.clips.iter().map(move |clip| (track.kind, clip)))
    }) {
        if clip.media_id != media_id {
            continue;
        }
        has_video |= kind == TrackKind::Video;
        has_audio |= kind == TrackKind::Audio;
        if let Ok(end) = clip.source_start.checked_add(clip.source_duration) {
            source_end = Some(source_end.map_or(end, |current: RationalTime| current.max(end)));
        }
    }
    (has_video, has_audio, source_end)
}

fn aligned_av_range_commands(
    project: &Project,
    index: i32,
    timeline_start: RationalTime,
    source_start: RationalTime,
    duration: RationalTime,
    available: RationalTime,
) -> Result<Vec<Command>, String> {
    if source_start.is_negative() || !duration.is_positive() {
        return Err("Source In must be nonnegative and duration must be greater than zero".into());
    }
    if source_start
        .checked_add(duration)
        .map_err(|error| error.to_string())?
        > available
    {
        return Err("The requested A/V range exceeds the original source duration".into());
    }
    aligned_av_commands(project, index, "timeline.move_av", timeline_start)?
        .into_iter()
        .map(|command| {
            if let Command::MoveClip {
                sequence_id,
                track_id,
                clip_id,
                timeline_start,
            } = command
            {
                Ok(Command::TrimClip {
                    sequence_id,
                    track_id,
                    clip_id,
                    timeline_start,
                    source_start,
                    source_duration: duration,
                })
            } else {
                Err("Unsupported paired range operation".into())
            }
        })
        .collect()
}

// A selected clip outside the playhead matches its source In point.
fn matched_source(
    project: &Project,
    index: i32,
    playhead: RationalTime,
) -> Option<(usize, RationalTime)> {
    let sequence = project.sequences.values().next()?;
    let clip = sequence
        .tracks
        .iter()
        .flat_map(|track| &track.clips)
        .nth(usize::try_from(index).ok()?)?;
    let media_index = project.media.keys().position(|id| *id == clip.media_id)?;
    let end = clip.timeline_start.checked_add(clip.source_duration).ok()?;
    let offset = if playhead >= clip.timeline_start && playhead < end {
        playhead.checked_sub(clip.timeline_start).ok()?
    } else {
        RationalTime::ZERO
    };
    Some((media_index, clip.source_start.checked_add(offset).ok()?))
}

fn navigation_target(
    project: &Project,
    position: RationalTime,
    command: &str,
) -> Option<RationalTime> {
    let sequence = project.sequences.values().next()?;
    let duration = sequence.duration().ok()?;
    match command {
        "playback.start" => Some(RationalTime::ZERO),
        "playback.end" => Some(duration),
        "playback.previous_edit" | "playback.next_edit" => {
            let mut boundaries = vec![RationalTime::ZERO, duration];
            for track in &sequence.tracks {
                if (track.kind == TrackKind::Video && !track.visible)
                    || (track.kind == TrackKind::Audio && track.muted)
                {
                    continue;
                }
                for clip in &track.clips {
                    boundaries.push(clip.timeline_start);
                    if let Ok(end) = clip.timeline_start.checked_add(clip.source_duration) {
                        boundaries.push(end);
                    }
                }
            }
            if command == "playback.next_edit" {
                Some(
                    boundaries
                        .into_iter()
                        .filter(|time| *time > position)
                        .min()
                        .unwrap_or(duration),
                )
            } else {
                Some(
                    boundaries
                        .into_iter()
                        .filter(|time| *time < position)
                        .max()
                        .unwrap_or(RationalTime::ZERO),
                )
            }
        }
        _ => None,
    }
}

fn export_preset(index: i32) -> Result<ExportPreset, String> {
    match index {
        0 => Ok(ExportPreset::Youtube1080p),
        1 => Ok(ExportPreset::Youtube4k),
        2 => Ok(ExportPreset::Vertical1080p),
        3 => Ok(ExportPreset::Square1080p),
        _ => Err("Choose a supported export preset".to_owned()),
    }
}

fn update_export_summary(ui: &PrismStudio, project: &Project) {
    let target = project.sequences.values().find_map(|sequence| {
        sequence
            .tracks
            .iter()
            .find(|track| {
                track.kind == TrackKind::Video && track.visible && !track.clips.is_empty()
            })
            .map(|_| sequence)
    });
    ui.set_can_export(false);
    let Some(sequence) = target else {
        ui.set_export_video_summary("Video / no visible video track".into());
        ui.set_export_audio_summary("Audio / not included".into());
        ui.set_export_preflight("Add visible video clips before exporting.".into());
        return;
    };
    let video_tracks = sequence
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Video && track.visible && !track.clips.is_empty())
        .count();
    let audio_tracks = sequence
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio && !track.muted && !track.clips.is_empty())
        .count();
    let video_duration = sequence.duration().ok();
    ui.set_export_video_summary(
        format!(
            "Video / {video_tracks} active tracks · {} clips · {}",
            sequence
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Video && track.visible)
                .map(|track| track.clips.len())
                .sum::<usize>(),
            video_duration.map(display_timecode).unwrap_or_default()
        )
        .into(),
    );
    if audio_tracks > 0 {
        ui.set_export_audio_summary(format!("Audio / AAC · {audio_tracks} active tracks").into());
    } else {
        ui.set_export_audio_summary("Audio / video-only export".into());
    }
    ui.set_can_export(video_duration.is_some_and(RationalTime::is_positive));
    ui.set_export_preflight(
        "Ready. Export creates a new file without replacing existing media.".into(),
    );
}

// A curated list of executable UI actions. Eligibility is recomputed at execution,
// so an open launcher never bypasses clip selection, lock state, or job state.
fn capture_view(ui: &PrismStudio) -> ViewPreferences {
    ViewPreferences {
        layout: ui.get_layout_preset().to_string(),
        monitor_height: ui.get_monitor_height(),
        inspector_open: ui.get_inspector_open(),
        inspector_tab: ui.get_inspector_tab().to_string(),
        inspector_section: ui.get_inspector_section().to_string(),
        browser_tab: ui.get_browser_tab().to_string(),
        snapping: ui.get_snap_enabled(),
        guides: ui.get_show_safe_guides(),
        media_sort: ui.get_media_sort(),
        media_filter: ui.get_media_filter(),
        scope_mode: ui.get_program_scope_mode(),
        monitor_scope: ui.get_show_monitor_scope(),
        keyboard_preset: ui.get_keyboard_preset().to_string(),
        ..Default::default()
    }
}

fn apply_view(ui: &PrismStudio, view: &ViewPreferences) {
    let height = ui
        .window()
        .size()
        .to_logical(ui.window().scale_factor())
        .height;
    ui.set_layout_preset(view.layout.clone().into());
    ui.set_monitor_height(
        view.monitor_height
            .clamp(220.0, (height - 360.0).max(220.0)),
    );
    ui.set_inspector_open(view.inspector_open);
    ui.set_inspector_tab(view.inspector_tab.clone().into());
    ui.set_inspector_section(view.inspector_section.clone().into());
    ui.set_browser_tab(view.browser_tab.clone().into());
    ui.set_snap_enabled(view.snapping);
    ui.set_show_safe_guides(view.guides);
    ui.set_media_sort(view.media_sort);
    ui.set_media_filter(view.media_filter);
    ui.set_program_scope_mode(view.scope_mode);
    ui.set_show_monitor_scope(view.monitor_scope);
    ui.set_keyboard_preset(view.keyboard_preset.clone().into());
    ui.set_precision_open(false);
    ui.set_media_tools_open(false);
    ui.set_timeline_zoom(1.0);
    ui.set_timeline_offset(0.0);
}

#[allow(clippy::too_many_lines)]
fn launcher_actions(ui: &PrismStudio) -> Vec<CommandItem> {
    let project = ui.get_has_project();
    let sequence = ui.get_clip_count() > 0;
    let clip = ui.get_selected_clip_index() >= 0 && !ui.get_inspector_locked();
    let entries = [
        (
            "project.new",
            "New project…",
            "Start a local Prism project",
            "",
            true,
        ),
        (
            "project.open",
            "Open project…",
            "Open an existing .prism file",
            "Ctrl/⌘ O",
            true,
        ),
        (
            "project.import_media",
            "Import footage…",
            "Browse files and add media",
            "Ctrl/⌘ I",
            true,
        ),
        (
            "project.save_as",
            "Save project as…",
            "Create a new project file and continue editing the copy",
            "Ctrl/⌘ Shift S",
            project,
        ),
        (
            "project.save",
            "Save project",
            "Save the current project",
            "Ctrl/⌘ S",
            project,
        ),
        (
            "edit.undo",
            "Undo last edit",
            "Restore the preceding saved state",
            "Ctrl/⌘ Z",
            ui.get_can_undo(),
        ),
        (
            "edit.redo",
            "Redo edit",
            "Reapply the last undone transaction",
            "Ctrl/⌘ Shift Z",
            ui.get_can_redo(),
        ),
        (
            "playback.play_pause",
            "Play / pause sequence",
            "Control sequence playback",
            "Space",
            sequence,
        ),
        (
            "timeline.split",
            "Split selected clip",
            "Cut the selected clip at the playhead",
            "Ctrl/⌘ K",
            clip,
        ),
        (
            "timeline.split_all",
            "Add edit to all unlocked tracks",
            "Cut crossing clips together, including muted and hidden tracks",
            "Ctrl/⌘ Shift K",
            sequence,
        ),
        (
            "source.match_program_frame",
            "Match Program frame in Source",
            "Locate the original frame beneath the playhead",
            if ui.get_keyboard_preset() == "Premiere" {
                "F"
            } else {
                ""
            },
            sequence,
        ),
        (
            "timeline.trim_av",
            "Change aligned A/V source range",
            "Apply Source In and Duration to both matching clips",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.move_av",
            "Move aligned audio and video",
            "Move both matching clips to the time entered in aligned A/V controls",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.split_av",
            "Split aligned audio and video",
            "Cut both matching clips at the playhead in one undo step",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.delete_av",
            "Delete aligned audio and video",
            "Remove both matching clips and leave their gaps",
            "",
            ui.get_can_edit_av(),
        ),
        (
            "timeline.duplicate",
            "Duplicate clip to track end",
            "Append a copy of the selected source range",
            "",
            clip,
        ),
        (
            "timeline.delete",
            "Delete selected clip",
            "Remove the clip and leave its gap",
            "Delete",
            clip,
        ),
        (
            "timeline.ripple_delete",
            "Ripple delete selected clip",
            "Remove the clip and close time on its track",
            if ui.get_keyboard_preset() == "Premiere" {
                "Shift Delete"
            } else {
                "Backspace"
            },
            clip,
        ),
        (
            "timeline.add_marker",
            "Add timeline marker",
            "Annotate the current playhead position",
            "M",
            sequence,
        ),
        (
            "view.markers",
            "Show markers",
            "Browse, rename, and navigate timeline markers",
            "",
            true,
        ),
        (
            "playback.previous_edit",
            "Go to previous edit",
            "Jump to the preceding enabled track boundary",
            "↑",
            sequence,
        ),
        (
            "playback.next_edit",
            "Go to next edit",
            "Jump to the following enabled track boundary",
            "↓",
            sequence,
        ),
        (
            "playback.start",
            "Go to sequence start",
            "Move the playhead to the beginning",
            "Home",
            sequence,
        ),
        (
            "playback.end",
            "Go to sequence end",
            "Move the playhead to the end",
            "End",
            sequence,
        ),
        (
            "timeline.fit",
            "Fit sequence in timeline",
            "Show the entire sequence",
            if ui.get_keyboard_preset() == "Premiere" {
                "Backslash"
            } else {
                "F"
            },
            project,
        ),
        (
            "timeline.toggle_snap",
            "Toggle timeline snapping",
            "Switch frame-grid snapping on or off",
            "S",
            true,
        ),
        (
            "source.match_frame",
            "Match selected clip in Source",
            "Show its frame at the playhead, or its source In when outside the clip",
            if ui.get_keyboard_preset() == "Premiere" {
                ""
            } else {
                "Shift F"
            },
            ui.get_selected_clip_index() >= 0,
        ),
        (
            "source.clear_marks",
            "Use full source clip",
            "Clear source In/Out marks without changing the timeline",
            "",
            ui.get_source_ready(),
        ),
        (
            "source.insert",
            "Insert marked source at playhead",
            "Use first video/audio tracks and shift unlocked tracks",
            if ui.get_keyboard_preset() == "Premiere" {
                ","
            } else {
                ""
            },
            ui.get_source_ready(),
        ),
        (
            "source.overwrite",
            "Overwrite with marked source at playhead",
            "Replace first video/audio tracks while preserving later timing",
            if ui.get_keyboard_preset() == "Premiere" {
                "."
            } else {
                ""
            },
            ui.get_source_ready(),
        ),
        (
            "source.append",
            "Append marked source range",
            "Add the source In/Out range to the sequence",
            if ui.get_keyboard_preset() == "Premiere" {
                ""
            } else {
                ","
            },
            ui.get_source_ready(),
        ),
        (
            "media.find_timeline",
            "Find source in timeline",
            "Cycle through clips that use the selected source",
            "",
            ui.get_selected_media_uses() > 0,
        ),
        (
            "media.relink_source",
            "Relink selected source…",
            "Repair the location of source media",
            "",
            ui.get_selected_media_index() >= 0,
        ),
        (
            "view.source",
            "Show source controls",
            "Inspect source range, relinking, and proxies",
            "",
            true,
        ),
        (
            "view.properties",
            "Show clip properties",
            "Edit selected clip timing",
            "",
            true,
        ),
        (
            "view.shortcuts",
            "Show keyboard shortcuts",
            "Choose a preset and inspect available or reserved mappings",
            "",
            true,
        ),
        (
            "view.history",
            "Show edit history",
            "Inspect undoable session actions",
            "",
            true,
        ),
        (
            "view.scopes",
            "Show color scopes",
            "Inspect the decoded Program frame histogram, waveform and chroma",
            "",
            true,
        ),
        (
            "view.jobs",
            "Show background jobs",
            "Autosave, thumbnails, proxies, cache, and export",
            "",
            true,
        ),
        (
            "view.export",
            "Open export settings",
            "Choose 1080p or 4K output and a destination",
            "",
            true,
        ),
        (
            "view.save_layout",
            "Save workspace layout",
            "Remember this project's panels, sizes and media sorting",
            "",
            project,
        ),
        (
            "view.reset",
            "Reset workspace layout",
            "Restore Editing, the inspector, and timeline fit",
            "",
            true,
        ),
        (
            "tool.V",
            "Selection / move tool",
            "Select clips, drag to move, or scrub the ruler",
            "V",
            true,
        ),
        (
            "tool.C",
            "Razor tool",
            "Split a clip at the clicked frame",
            "C",
            true,
        ),
        (
            "tool.T",
            "Trim tool",
            "Drag the nearest clip edge",
            if ui.get_keyboard_preset() == "Premiere" {
                ""
            } else {
                "T"
            },
            true,
        ),
        (
            "tool.Y",
            "Slip tool",
            "Change source frames without moving the clip",
            "Y",
            true,
        ),
        (
            "tool.U",
            "Slide tool",
            "Reposition a clip and adjust both neighbors",
            "U",
            true,
        ),
        (
            "tool.R",
            "Rolling edit tool",
            "Move a boundary between adjacent clips",
            if ui.get_keyboard_preset() == "Premiere" {
                "N"
            } else {
                "R"
            },
            true,
        ),
    ];
    entries
        .into_iter()
        .map(|(id, label, detail, shortcut, enabled)| CommandItem {
            id: id.into(),
            label: label.into(),
            detail: if enabled {
                detail.into()
            } else {
                match id {
                    "edit.undo" => "Nothing to undo in this session".into(),
                    "edit.redo" => "Nothing to redo in this session".into(),
                    "project.save" => "Open or create a project first".into(),
                    "source.append" => "Select an available source and mark its range".into(),
                    "playback.play_pause" | "timeline.add_marker" => {
                        "Add clips to a sequence first".into()
                    }
                    _ => "Select an unlocked timeline clip first".into(),
                }
            },
            shortcut: shortcut.into(),
            enabled,
        })
        .collect()
}

fn filter_launcher(ui: &PrismStudio) {
    let terms = ui
        .get_palette_query()
        .to_lowercase()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let items = launcher_actions(ui)
        .into_iter()
        .filter(|item| {
            let searchable = format!("{} {}", item.label, item.detail).to_lowercase();
            terms.iter().all(|term| searchable.contains(term))
        })
        .collect::<Vec<_>>();
    ui.set_command_items(slint::ModelRc::new(slint::VecModel::from(items)));
}

fn launch_default_export(
    controller: &mut ExportController,
    worker: &mut Option<ExportWorker>,
    project: &Project,
    output: PathBuf,
    preset: ExportPreset,
) -> Result<(), String> {
    controller.start().map_err(|error| error.to_string())?;
    let target = project
        .sequences
        .iter()
        .find_map(|(sequence_id, sequence)| {
            sequence
                .tracks
                .iter()
                .find(|track| {
                    track.kind == TrackKind::Video && track.visible && !track.clips.is_empty()
                })
                .map(|_| *sequence_id)
        });
    let Some(sequence_id) = target else {
        controller
            .fail("project has no visible non-empty video track")
            .map_err(|error| error.to_string())?;
        return Ok(());
    };
    let (width, height, frame_rate, video_bitrate_kbps) = preset.settings();
    *worker = Some(ExportWorker::spawn_sequence(
        media_tool("ffmpeg"),
        media_tool("ffprobe"),
        project.clone(),
        sequence_id,
        SequenceMp4ExportRequest {
            width,
            height,
            frame_rate,
            video_bitrate_kbps,
            audio_bitrate_kbps: preset.audio_bitrate_kbps(),
            output,
        },
    ));
    Ok(())
}

/// Native shell startup failures.
#[derive(Debug, Error)]
pub enum NativeUiError {
    /// Native window or event-loop initialization failed.
    #[error("native platform error: {0}")]
    Platform(#[from] slint::PlatformError),
    /// The requested project could not be loaded safely.
    #[error("could not open project: {0}")]
    Persistence(#[from] prism_persistence::PersistenceError),
}

// Decode away from the UI thread. Each monitor holds at most one queued request;
// a newer scrub position supersedes pending work and stale replies are discarded.
#[derive(Debug)]
struct PreviewRequest {
    generation: u64,
    path: PathBuf,
    time: RationalTime,
    cache_root: Option<PathBuf>,
    playback: bool,
    frame_rate: RationalTime,
    cancelled: Arc<AtomicBool>,
}

type PreviewPayload = (
    prism_media::DecodedVideoFrame,
    RationalTime,
    Option<MediaFingerprint>,
    bool,
);
type PreviewReply = Result<PreviewPayload, String>;

#[derive(Debug)]
struct PreviewWorker {
    sender: SyncSender<PreviewRequest>,
    receiver: Receiver<(u64, PreviewReply)>,
    pending: RefCell<Option<PreviewRequest>>,
    generation: Cell<u64>,
    busy: Cell<bool>,
    requested_at: Cell<Option<Instant>>,
    stream_control: StreamControl,
}

#[derive(Debug)]
struct ProxyGeneration {
    receiver: Receiver<Result<PathBuf, String>>,
    cancelled: Arc<AtomicBool>,
    media_id: MediaAssetId,
    source_fingerprint: ProxySourceFingerprint,
}

impl Drop for PreviewWorker {
    fn drop(&mut self) {
        self.stream_control.stop();
    }
}

impl PreviewWorker {
    fn new() -> Self {
        Self::with_thumbnails(false)
    }

    #[allow(clippy::too_many_lines)]
    fn with_thumbnails(thumbnails: bool) -> Self {
        let (sender, requests) = mpsc::sync_channel::<PreviewRequest>(1);
        let (replies, receiver) = mpsc::sync_channel(1);
        let stream_control = StreamControl::default();
        let decode_control = stream_control.clone();
        std::thread::spawn(move || {
            let mut stream: Option<StreamPreview> = None;
            while let Ok(mut request) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                if request.cancelled.load(Ordering::Acquire) {
                    continue;
                }
                if !request.playback {
                    stream.take();
                }
                let source_fingerprint = thumbnails
                    .then(|| fingerprint(&request.path).ok())
                    .flatten();
                let cached = if thumbnails {
                    request.cache_root.as_ref().and_then(|root| {
                        let entry = MediaCatalogEntry {
                            path: request.path.clone(),
                            kind: MediaItemKind::Video,
                            metadata: None,
                            fingerprint: source_fingerprint.clone(),
                            offline: false,
                            diagnostic: None,
                        };
                        let key = MediaArtifactWorker::thumbnail_variant_key(
                            &entry,
                            BROWSER_THUMBNAIL_VARIANT,
                        );
                        let layout = CacheLayout::new(root);
                        match layout.read(key, MediaArtifactKind::Thumbnail.extension()) {
                            Ok(Some(bytes)) => {
                                if let Ok(frame) = decode_thumbnail_artifact(&bytes) {
                                    Some(frame)
                                } else {
                                    let _ = layout
                                        .remove(key, MediaArtifactKind::Thumbnail.extension());
                                    None
                                }
                            }
                            Ok(None) | Err(_) => None,
                        }
                    })
                } else {
                    None
                };
                let result = if let Some(frame) = cached {
                    Ok((frame, RationalTime::ZERO, source_fingerprint, true))
                } else if request.playback {
                    (|| {
                        if !decode_control.active() {
                            return Err("Video playback stopped".to_owned());
                        }
                        if !stream.as_ref().is_some_and(|stream| {
                            stream.reusable(&request.path, request.time, request.frame_rate)
                        }) {
                            stream.take();
                            stream = Some(StreamPreview::open(
                                &request.path,
                                request.time,
                                request.frame_rate,
                                &media_tool("ffmpeg"),
                                &media_tool("ffprobe"),
                                decode_control.clone(),
                                &request.cancelled,
                            )?);
                        }
                        let (frame, duration) = stream
                            .as_mut()
                            .ok_or("Video decoder unavailable")?
                            .frame(request.time)?;
                        Ok((frame, duration, None, false))
                    })()
                } else {
                    decode_preview(&request.path, request.time, &request.cancelled).and_then(
                        |(frame, duration)| {
                            if thumbnails {
                                let longest = u64::from(frame.width.max(frame.height)).max(1);
                                let width = u32::try_from(u64::from(frame.width) * 160 / longest)
                                    .unwrap_or(1)
                                    .max(1);
                                let height = u32::try_from(u64::from(frame.height) * 160 / longest)
                                    .unwrap_or(1)
                                    .max(1);
                                frame
                                    .thumbnail(width, height)
                                    .map(|frame| (frame, duration, source_fingerprint, false))
                                    .map_err(|e| e.to_string())
                            } else {
                                Ok((frame, duration, None, false))
                            }
                        },
                    )
                };
                if request.playback && result.is_err() {
                    stream.take();
                }
                if replies.send((request.generation, result)).is_err() {
                    break;
                }
            }
        });
        Self {
            sender,
            receiver,
            pending: RefCell::new(None),
            generation: Cell::new(0),
            busy: Cell::new(false),
            requested_at: Cell::new(None),
            stream_control,
        }
    }

    fn invalidate(&self) {
        self.stream_control.stop();
        self.busy.set(false);
        self.requested_at.set(None);
        self.generation.set(self.generation.get().wrapping_add(1));
        *self.pending.borrow_mut() = None;
    }

    fn request(&self, path: PathBuf, time: RationalTime) {
        self.invalidate();
        self.busy.set(true);
        self.requested_at.set(Some(Instant::now()));
        *self.pending.borrow_mut() = Some(PreviewRequest {
            generation: self.generation.get(),
            path,
            time,
            cache_root: None,
            playback: false,
            frame_rate: RationalTime::from_integer(30),
            cancelled: self.stream_control.request_token(),
        });
        self.flush();
    }

    // Playback must let the active decode finish. Scrubbing still supersedes it.
    fn request_playback(&self, path: PathBuf, time: RationalTime, frame_rate: RationalTime) {
        if !self.busy.get() {
            self.stream_control.activate();
            self.generation.set(self.generation.get().wrapping_add(1));
            self.busy.set(true);
            self.requested_at.set(Some(Instant::now()));
            *self.pending.borrow_mut() = Some(PreviewRequest {
                generation: self.generation.get(),
                path,
                time,
                cache_root: None,
                playback: true,
                frame_rate,
                cancelled: self.stream_control.request_token(),
            });
            self.flush();
        }
    }

    fn request_thumbnail(&self, path: PathBuf, cache_root: Option<PathBuf>) {
        self.invalidate();
        self.busy.set(true);
        self.requested_at.set(Some(Instant::now()));
        *self.pending.borrow_mut() = Some(PreviewRequest {
            generation: self.generation.get(),
            path,
            time: RationalTime::ZERO,
            cache_root,
            playback: false,
            frame_rate: RationalTime::from_integer(30),
            cancelled: self.stream_control.request_token(),
        });
        self.flush();
    }

    fn flush(&self) {
        let pending = self.pending.borrow_mut().take();
        if let Some(request) = pending
            && let Err(TrySendError::Full(request)) = self.sender.try_send(request)
        {
            *self.pending.borrow_mut() = Some(request);
        }
    }

    fn timed_out(&self, deadline: Duration) -> bool {
        self.busy.get()
            && self
                .requested_at
                .get()
                .is_some_and(|started| started.elapsed() > deadline)
    }

    fn poll(&self) -> Option<Result<(slint::Image, RationalTime), String>> {
        self.poll_decoded().map(|result| {
            result.map(|(frame, duration, _, _)| {
                let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &frame.rgba,
                    frame.width,
                    frame.height,
                );
                (slint::Image::from_rgba8(buffer), duration)
            })
        })
    }

    fn poll_decoded(&self) -> Option<PreviewReply> {
        self.flush();
        let mut result = None;
        while let Ok((generation, frame)) = self.receiver.try_recv() {
            if generation == self.generation.get() {
                self.busy.set(false);
                self.requested_at.set(None);
                result = Some(frame);
            }
        }
        result
    }
}

fn decode_preview(
    path: &Path,
    time: RationalTime,
    cancelled: &AtomicBool,
) -> Result<(prism_media::DecodedVideoFrame, RationalTime), String> {
    let metadata = FfprobeInspector::new(media_tool("ffprobe"))
        .inspect_with_control(
            path,
            prism_media::MediaProcessControl {
                timeout: Duration::from_secs(5),
                cancelled,
            },
        )
        .map_err(|e| e.to_string())?;
    let video = metadata
        .streams
        .iter()
        .find_map(|stream| match &stream.kind {
            StreamKind::Video(video) => Some(video),
            _ => None,
        })
        .ok_or_else(|| "No video stream available".to_owned())?;
    let (Some(width), Some(height)) = (video.width, video.height) else {
        return Err("Video dimensions unavailable".to_owned());
    };
    let frame = FfmpegReferenceDecoder::new(media_tool("ffmpeg"))
        .decode_video_frame_rgba_with_control(
            path,
            0,
            time,
            width,
            height,
            prism_media::MediaProcessControl {
                timeout: Duration::from_secs(5),
                cancelled,
            },
        )
        .map_err(|e| e.to_string())?;
    Ok((frame, metadata.duration.unwrap_or(RationalTime::ZERO)))
}

fn display_timecode(time: RationalTime) -> String {
    display_timecode_at_rate(time, RationalTime::from_integer(30))
}

fn display_timecode_at_rate(time: RationalTime, frame_rate: RationalTime) -> String {
    let scale = u32::try_from(frame_rate.value()).unwrap_or(30);
    let frames = time
        .to_ticks(scale, prism_timeline::RoundingMode::Floor)
        .unwrap_or(0)
        .checked_div(i64::from(frame_rate.timescale()))
        .unwrap_or(0)
        .max(0);
    #[allow(clippy::cast_possible_truncation)]
    let nominal = frame_rate.as_seconds_f64().round().max(1.0) as i64;
    let minute = nominal.saturating_mul(60);
    let hour = minute.saturating_mul(60);
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        frames / hour,
        frames / minute % 60,
        frames / nominal % 60,
        frames % nominal
    )
}

fn timing_text(time: RationalTime) -> slint::SharedString {
    if time.timescale() == 1 {
        time.value().to_string().into()
    } else {
        format!("{}/{}", time.value(), time.timescale()).into()
    }
}

fn parse_timing(text: &str) -> Result<RationalTime, String> {
    let text = text.trim();
    let invalid = || "Enter non-negative seconds (1.5) or a fraction (3/2)".to_owned();
    let time = if let Some((numerator, denominator)) = text.split_once('/') {
        RationalTime::new(
            numerator.trim().parse().map_err(|_| invalid())?,
            denominator.trim().parse().map_err(|_| invalid())?,
        )
    } else if let Some((whole, fraction)) = text.split_once('.') {
        if fraction.is_empty()
            || fraction.len() > 6
            || !fraction.bytes().all(|c| c.is_ascii_digit())
            || !whole.bytes().all(|c| c.is_ascii_digit())
        {
            return Err(invalid());
        }
        let scale = 10_u32
            .checked_pow(u32::try_from(fraction.len()).map_err(|_| invalid())?)
            .ok_or_else(invalid)?;
        let whole: i64 = if whole.is_empty() {
            0
        } else {
            whole.parse().map_err(|_| invalid())?
        };
        let fraction: i64 = fraction.parse().map_err(|_| invalid())?;
        let value = whole
            .checked_mul(i64::from(scale))
            .and_then(|v| v.checked_add(fraction))
            .ok_or_else(invalid)?;
        RationalTime::new(value, scale)
    } else {
        RationalTime::new(text.parse().map_err(|_| invalid())?, 1)
    }
    .map_err(|e| e.to_string())?;
    if time.is_negative() {
        return Err(invalid());
    }
    Ok(time)
}

struct NativeExportRuntime {
    project: RefCell<Option<Project>>,
    project_path: RefCell<Option<PathBuf>>,
    controller: RefCell<ExportController>,
    worker: RefCell<Option<ExportWorker>>,
    queued_export: RefCell<Option<(Project, ExportPreset)>>,
    program_preview: PreviewWorker,
    source_preview: PreviewWorker,
    playhead: Cell<RationalTime>,
    playing: Cell<bool>,
    playback_started: Cell<Option<Instant>>,
    playback_origin: Cell<RationalTime>,
    audio_process: RefCell<Option<Child>>,
    audio_decoder: RefCell<Option<Child>>,
    audio_end: Cell<RationalTime>,
    audio_clock: RefCell<Option<crate::audio_clock::AudioClock>>,
    audio_clip: Cell<Option<ClipId>>,
    source_position: Cell<RationalTime>,
    source_duration: Cell<RationalTime>,
    source_in: Cell<RationalTime>,
    source_out: Cell<RationalTime>,
    undo: RefCell<Vec<UndoRecord>>,
    redo: RefCell<Vec<UndoRecord>>,
    thumbnail_preview: PreviewWorker,
    thumbnail_queue: RefCell<VecDeque<(usize, PathBuf)>>,
    thumbnail_current: Cell<Option<usize>>,
    thumbnail_total: Cell<usize>,
    thumbnail_completed: Cell<usize>,
    thumbnail_failed: Cell<usize>,
    waveform_worker: RefCell<WaveformWorker>,
    waveform_total: Cell<usize>,
    waveform_completed: Cell<usize>,
    waveform_failed: Cell<usize>,
    waveforms: RefCell<BTreeMap<String, Vec<f32>>>,
    media_artifacts: RefCell<Option<MediaArtifactWorker>>,
    cards: RefCell<Vec<MediaCard>>,
    use_proxy: Cell<bool>,
    proxy_generation: RefCell<Option<ProxyGeneration>>,
    proxy_availability: RefCell<BTreeMap<MediaAssetId, bool>>,
    recovery_candidates: RefCell<Vec<prism_persistence::AutosaveCandidate>>,
    autosave_scheduler: RefCell<Option<prism_persistence::AutosaveScheduler>>,
    pending_autosave: RefCell<Option<Project>>,
    autosave_preferences: Cell<AutosavePreferences>,
    cache_eviction: RefCell<Option<CacheEvictionWorker>>,
}

impl Drop for NativeExportRuntime {
    fn drop(&mut self) {
        self.waveform_worker.borrow().cancel();
        if let Some(worker) = self.worker.get_mut().as_ref() {
            worker.cancel();
        }
        if let Some(generation) = self.proxy_generation.get_mut().as_ref() {
            generation.cancelled.store(true, Ordering::Release);
        }
        if let Some(mut child) = self.audio_process.get_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.audio_decoder.get_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl NativeExportRuntime {
    fn new(project: Option<Project>, project_path: Option<PathBuf>) -> Self {
        Self {
            project: RefCell::new(project),
            project_path: RefCell::new(project_path),
            controller: RefCell::new(ExportController::new()),
            worker: RefCell::new(None),
            queued_export: RefCell::new(None),
            program_preview: PreviewWorker::new(),
            source_preview: PreviewWorker::new(),
            playhead: Cell::new(RationalTime::ZERO),
            playing: Cell::new(false),
            playback_started: Cell::new(None),
            playback_origin: Cell::new(RationalTime::ZERO),
            audio_process: RefCell::new(None),
            audio_decoder: RefCell::new(None),
            audio_end: Cell::new(RationalTime::ZERO),
            audio_clock: RefCell::new(None),
            audio_clip: Cell::new(None),
            source_position: Cell::new(RationalTime::ZERO),
            source_duration: Cell::new(RationalTime::ZERO),
            source_in: Cell::new(RationalTime::ZERO),
            source_out: Cell::new(RationalTime::ZERO),
            undo: RefCell::new(Vec::new()),
            redo: RefCell::new(Vec::new()),
            thumbnail_preview: PreviewWorker::with_thumbnails(true),
            thumbnail_queue: RefCell::new(VecDeque::new()),
            thumbnail_current: Cell::new(None),
            thumbnail_total: Cell::new(0),
            thumbnail_completed: Cell::new(0),
            thumbnail_failed: Cell::new(0),
            waveform_worker: RefCell::new(WaveformWorker::spawn(media_tool("ffmpeg"), 64)),
            waveform_total: Cell::new(0),
            waveform_completed: Cell::new(0),
            waveform_failed: Cell::new(0),
            waveforms: RefCell::new(BTreeMap::new()),
            media_artifacts: RefCell::new(None),
            cards: RefCell::new(Vec::new()),
            use_proxy: Cell::new(false),
            proxy_generation: RefCell::new(None),
            proxy_availability: RefCell::new(BTreeMap::new()),
            recovery_candidates: RefCell::new(Vec::new()),
            autosave_scheduler: RefCell::new(None),
            pending_autosave: RefCell::new(None),
            autosave_preferences: Cell::new(AutosavePreferences::default()),
            cache_eviction: RefCell::new(None),
        }
    }

    fn frame_duration(&self) -> RationalTime {
        self.project
            .borrow()
            .as_ref()
            .and_then(|project| {
                let sequence_id = *project.sequences.keys().next()?;
                project.sequence_frame_duration(sequence_id)
            })
            .unwrap_or_else(|| RationalTime::new(1, 30).unwrap_or(RationalTime::from_integer(1)))
    }

    fn frame_rate(&self) -> RationalTime {
        self.project
            .borrow()
            .as_ref()
            .and_then(|project| {
                let sequence_id = *project.sequences.keys().next()?;
                project.sequence_frame_rate(sequence_id)
            })
            .unwrap_or(RationalTime::from_integer(30))
    }

    fn quantize_to_frame(&self, seconds: f64) -> RationalTime {
        let frame = self.frame_duration();
        #[allow(clippy::cast_possible_truncation)]
        let count = (seconds.max(0.0) / frame.as_seconds_f64()).floor() as i64;
        frame.checked_mul_i64(count).unwrap_or(RationalTime::ZERO)
    }

    fn display_timecode(&self, time: RationalTime) -> String {
        display_timecode_at_rate(time, self.frame_rate())
    }

    #[allow(clippy::too_many_lines)]
    fn handle(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        match command {
            "view.markers" => {
                ui.set_active_workspace("EDIT".into());
                ui.set_browser_tab("Markers".into());
                Ok(())
            }
            "view.properties" | "view.source" | "view.history" | "view.jobs" | "view.scopes"
            | "view.shortcuts" => {
                ui.set_active_workspace("EDIT".into());
                ui.set_inspector_open(true);
                ui.set_inspector_tab(
                    match command {
                        "view.history" => "History",
                        "view.jobs" => "Jobs",
                        "view.scopes" => "Scopes",
                        "view.shortcuts" => "Shortcuts",
                        _ => "Properties",
                    }
                    .into(),
                );
                if matches!(command, "view.properties" | "view.source") {
                    ui.set_inspector_section(
                        if command == "view.source" {
                            "Source"
                        } else {
                            "Clip"
                        }
                        .into(),
                    );
                }
                Ok(())
            }
            "view.export" => {
                ui.set_active_workspace("EXPORT".into());
                Ok(())
            }
            "view.save_layout" => self.save_view(ui),
            "view.reset" => {
                apply_view(ui, &ViewPreferences::default());
                ui.set_workspace_status("Default view · not saved".into());
                self.filter_cards(ui);
                self.update_ruler(ui);
                Ok(())
            }
            "tool.V" | "tool.C" | "tool.T" | "tool.Y" | "tool.U" | "tool.R" => {
                ui.set_active_workspace("EDIT".into());
                ui.set_active_tool(command.trim_start_matches("tool.").into());
                Ok(())
            }
            "timeline.rename_marker" => self.rename_marker(ui),
            "project.new" => self.new_project(ui),
            "media.find_timeline" => self.find_media_in_timeline(ui),
            "project.save" => self.save(ui),
            "project.save_as" => self.save_as(ui),
            "edit.undo" | "edit.redo" => self.apply_history(ui, command == "edit.redo"),
            "timeline.apply_timing" => self.apply_timing(ui),
            "timeline.move_left"
            | "timeline.move_right"
            | "timeline.trim_in_left"
            | "timeline.trim_in_right"
            | "timeline.trim_out_left"
            | "timeline.trim_out_right" => self.adjust_clip_frame(ui, command),
            "timeline.slip_left"
            | "timeline.slip_right"
            | "timeline.roll_in_left"
            | "timeline.roll_in_right"
            | "timeline.roll_out_left"
            | "timeline.roll_out_right"
            | "timeline.slide_left"
            | "timeline.slide_right" => self.precision_edit_frame(ui, command),
            "source.append" => self.append_source(ui),
            "source.insert" | "source.overwrite" => {
                self.edit_source(ui, command == "source.overwrite")
            }
            "media.assign_proxy" => self.assign_proxy(ui),
            "media.relink_source" => self.relink_source(ui),
            "media.clear_proxy" => self.clear_proxy(ui),
            "media.generate_proxy" => self.generate_proxy(ui),
            "media.regenerate_proxy" => self.regenerate_proxy(ui),
            "media.cancel_proxy" => {
                if let Some(generation) = self.proxy_generation.borrow().as_ref() {
                    generation.cancelled.store(true, Ordering::Release);
                    ui.set_feedback("Cancelling proxy generation…".into());
                }
                Ok(())
            }
            "cache.clean" => self.clean_cache(ui),
            "autosave.toggle" => self.update_autosave_preferences(ui, true),
            "autosave.retention" => self.update_autosave_preferences(ui, false),
            "media.toggle_proxy" => {
                self.use_proxy.set(!self.use_proxy.get());
                ui.set_proxy_active(self.use_proxy.get());
                self.select_media(ui, ui.get_selected_media_index());
                self.seek_time(ui, self.playhead.get());
                Ok(())
            }
            "source.match_program_frame" => {
                let index = self
                    .project
                    .borrow()
                    .as_ref()
                    .and_then(|project| program_clip_index(project, self.playhead.get()))
                    .ok_or("No visible video clip beneath the playhead")?;
                ui.set_selected_clip_index(index);
                self.update_inspector(ui);
                self.match_frame(ui)
            }
            "source.match_frame" => self.match_frame(ui),
            "source.clear_marks" => {
                if !ui.get_source_ready() {
                    return Err("Select an available source first".to_owned());
                }
                self.source_in.set(RationalTime::ZERO);
                self.source_out.set(self.source_duration.get());
                self.update_source_labels(ui);
                ui.set_feedback("Source range reset to the full clip".into());
                Ok(())
            }
            "source.mark_in" | "source.mark_out" => {
                if !ui.get_source_ready() {
                    return Err("Select an available source first".to_owned());
                }
                let position = self.source_position.get();
                if command == "source.mark_in" {
                    if position >= self.source_out.get() {
                        return Err("In point must be before Out".to_owned());
                    }
                    self.source_in.set(position);
                } else {
                    let end = position
                        .checked_add(self.frame_duration())
                        .map_err(|e| e.to_string())?
                        .min(self.source_duration.get());
                    if end <= self.source_in.get() {
                        return Err("Out point must be after In".to_owned());
                    }
                    self.source_out.set(end);
                }
                self.update_source_labels(ui);
                Ok(())
            }
            "timeline.split_all" => {
                let commands = split_all_commands(
                    self.project
                        .borrow()
                        .as_ref()
                        .ok_or("Open a project first")?,
                    self.playhead.get(),
                )?;
                self.commit_commands(ui, commands, "Add edit to all unlocked tracks")?;
                ui.set_selected_clip_index(-1);
                self.update_inspector(ui);
                Ok(())
            }
            "timeline.split_av"
            | "timeline.delete_av"
            | "timeline.ripple_delete_av"
            | "timeline.move_av"
            | "timeline.trim_av" => self.edit_aligned_av(command, ui),
            "timeline.split" | "timeline.delete" | "timeline.ripple_delete" => {
                self.edit_clip_or_aligned(command, ui)
            }
            "timeline.add_marker" => self.add_marker(ui),
            "timeline.delete_marker" => self.delete_marker(ui),
            "timeline.toggle_dissolve" => self.toggle_dissolve(ui),
            "timeline.duplicate" => self.duplicate_clip(ui),
            "timeline.track_up" => self.change_track(ui, ui.get_selected_track_index(), "up"),
            "timeline.track_down" => self.change_track(ui, ui.get_selected_track_index(), "down"),
            "timeline.toggle_snap" => {
                ui.set_snap_enabled(!ui.get_snap_enabled());
                ui.set_feedback(
                    if ui.get_snap_enabled() {
                        "Timeline snapping enabled"
                    } else {
                        "Timeline snapping disabled"
                    }
                    .into(),
                );
                Ok(())
            }
            "playback.stop" => {
                let retain_picture = ui.get_has_program_frame();
                self.stop_playback(ui);
                self.seek_time(ui, self.playhead.get());
                ui.set_has_program_frame(retain_picture);
                Ok(())
            }
            "view.zoom_in" | "view.zoom_out" => {
                let zoom = if command == "view.zoom_in" {
                    ui.get_timeline_zoom() * 2.0
                } else {
                    ui.get_timeline_zoom() / 2.0
                }
                .clamp(1.0, 8.0);
                ui.set_timeline_zoom(zoom);
                ui.set_timeline_offset(ui.get_timeline_offset().min(1.0 - 1.0 / zoom));
                self.update_ruler(ui);
                Ok(())
            }
            "playback.play_pause" => {
                let playing = !self.playing.get();
                self.playing.set(playing);
                ui.set_playing(playing);
                if playing {
                    self.playback_origin.set(self.playhead.get());
                    self.playback_started.set(Some(Instant::now()));
                    if let Err(error) = self.start_audio_playback() {
                        self.stop_playback(ui);
                        ui.set_feedback(error.into());
                    }
                    ui.set_playback_buffering(
                        self.playing.get() && self.audio_clock.borrow().is_some(),
                    );
                } else {
                    let retain_picture = ui.get_has_program_frame();
                    self.stop_playback(ui);
                    self.seek_time(ui, self.playhead.get());
                    ui.set_has_program_frame(retain_picture);
                }
                Ok(())
            }
            "timeline.fit" => {
                ui.set_timeline_zoom(1.0);
                ui.set_timeline_offset(0.0);
                self.update_ruler(ui);
                Ok(())
            }
            "playback.start" | "playback.end" | "playback.previous_edit" | "playback.next_edit" => {
                let target = self
                    .project
                    .borrow()
                    .as_ref()
                    .and_then(|project| navigation_target(project, self.playhead.get(), command))
                    .ok_or_else(|| "Open a sequence to navigate its edits".to_owned())?;
                self.stop_playback(ui);
                self.seek_time(ui, target);
                Ok(())
            }
            "playback.step_backward" | "playback.step_forward" => {
                self.stop_playback(ui);
                let frame = self.frame_duration();
                let delta = if command == "playback.step_forward" {
                    frame
                } else {
                    RationalTime::ZERO
                        .checked_sub(frame)
                        .map_err(|error| error.to_string())?
                };
                let time = self
                    .playhead
                    .get()
                    .checked_add(delta)
                    .map_err(|e| e.to_string())?;
                self.seek_time(ui, time.max(RationalTime::ZERO));
                Ok(())
            }
            "export.destination" => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Export video")
                    .set_file_name("Prism export.mp4")
                    .add_filter("MP4 video", &["mp4"])
                    .save_file()
                {
                    ui.set_export_output(path.display().to_string().into());
                }
                Ok(())
            }
            "project.open" => self.open_project(ui),
            "recovery.recover_latest" => self.recover_latest(ui),
            "project.import_media" => self.import_media(ui),
            "export.queue" => self.queue(ui),
            "export.cancel" => {
                if let Some(worker) = self.worker.borrow().as_ref() {
                    worker.cancel();
                }
                self.controller
                    .borrow_mut()
                    .cancel()
                    .map_err(|error| error.to_string())
            }
            "export.retry" => self.retry(),
            "export.remove" => self
                .controller
                .borrow_mut()
                .remove()
                .map_err(|error| error.to_string()),
            _ => Err("This command is not available".to_owned()),
        }
    }

    fn new_project(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Create Prism Project")
            .set_file_name("Untitled.prism")
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Untitled");
        let project = Project::new(name);
        prism_persistence::save_atomic(&project, &path).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &path);
        ui.set_export_output(
            path.with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(path);
        self.reset_view(ui);
        Ok(())
    }

    fn open_project(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open Prism Project")
            .add_filter("Prism Project", &["prism", "json"])
            .pick_file()
        else {
            return Ok(());
        };
        let project = prism_persistence::load(&path).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &path);
        ui.set_export_output(
            path.with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(path);
        self.reset_view(ui);
        Ok(())
    }

    fn queue(&self, ui: &PrismStudio) -> Result<(), String> {
        media_runtime_status()?;
        let project = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "open a project before exporting".to_owned())?;
        if self.worker.borrow().is_some() {
            return Err("An export is already running".to_owned());
        }
        if ui.get_export_output().trim().is_empty() {
            return Err("Choose an output destination first".to_owned());
        }
        let output = PathBuf::from(ui.get_export_output().as_str());
        if output.exists() {
            return Err(
                "Choose a new output filename; Prism does not replace existing media".to_owned(),
            );
        }
        let preset = export_preset(ui.get_export_preset_index())?;
        update_export_summary(ui, &project);
        if !ui.get_can_export() {
            return Err(ui.get_export_preflight().to_string());
        }
        *self.queued_export.borrow_mut() = Some((project.clone(), preset));
        let mut controller = self.controller.borrow_mut();
        controller.queue(output.clone());
        launch_default_export(
            &mut controller,
            &mut self.worker.borrow_mut(),
            &project,
            output,
            preset,
        )
    }

    fn retry(&self) -> Result<(), String> {
        if self.worker.borrow().is_some() {
            return Err("Wait for the current export worker to finish".to_owned());
        }
        let (project, preset) = self
            .queued_export
            .borrow()
            .clone()
            .ok_or_else(|| "Export job is unavailable".to_owned())?;
        let mut controller = self.controller.borrow_mut();
        controller.retry().map_err(|error| error.to_string())?;
        let output = controller
            .status()
            .map(|status| PathBuf::from(status.output))
            .ok_or_else(|| "export job is unavailable".to_owned())?;
        launch_default_export(
            &mut controller,
            &mut self.worker.borrow_mut(),
            &project,
            output,
            preset,
        )
    }

    fn import_media(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("Import Media")
            .add_filter(
                "Video and audio",
                &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpeg", "mpg"],
            )
            .add_filter(
                "Audio",
                &["wav", "aif", "aiff", "mp3", "m4a", "aac", "flac", "ogg"],
            )
            .pick_files()
        else {
            return Ok(());
        };
        let mut project = self.project.borrow().clone().unwrap_or_else(|| {
            let name = paths
                .first()
                .and_then(|path| path.file_stem())
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled");
            Project::new(name)
        });
        let mut import_history = Vec::new();
        for path in paths {
            import_history.push(import_media_into_project(&mut project, &path)?);
        }
        let project_path = if let Some(path) = self.project_path.borrow().clone() {
            path
        } else {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Save Prism Project")
                .set_file_name(format!("{}.prism", project.name))
                .add_filter("Prism Project", &["prism"])
                .save_file()
            else {
                return Ok(());
            };
            path
        };
        prism_persistence::save_atomic(&project, &project_path)
            .map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &project_path);
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(project_path);
        self.reset_view(ui);
        self.undo.borrow_mut().extend(import_history);
        self.update_history(ui);
        Ok(())
    }

    fn save(&self, ui: &PrismStudio) -> Result<(), String> {
        let project = self.project.borrow();
        let project = project
            .as_ref()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let path = self.project_path.borrow();
        let path = path
            .as_ref()
            .ok_or_else(|| "Project has no save destination".to_owned())?;
        prism_persistence::save_atomic(project, path).map_err(|e| e.to_string())?;
        ui.set_feedback("Project saved".into());
        Ok(())
    }

    fn save_as(&self, ui: &PrismStudio) -> Result<(), String> {
        let default_name = self
            .project_path
            .borrow()
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|name| name.to_str())
            .map_or_else(
                || "Prism copy.prism".to_owned(),
                |name| format!("{name} copy.prism"),
            );
        let Some(destination) = rfd::FileDialog::new()
            .set_title("Save Prism Project As")
            .set_file_name(default_name)
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        self.save_as_to(ui, &destination)
    }

    fn save_as_to(&self, ui: &PrismStudio, destination: &Path) -> Result<(), String> {
        let project = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        // Existing projects must never be replaced by Save As.
        prism_persistence::save_backup(&project, destination).map_err(|error| error.to_string())?;
        let view = capture_view(ui);
        let undo = self.undo.borrow_mut().drain(..).collect::<Vec<_>>();
        let redo = self.redo.borrow_mut().drain(..).collect::<Vec<_>>();
        *self.project_path.borrow_mut() = Some(destination.to_path_buf());
        apply_project_summary(ui, &project, destination);
        ui.set_export_output(
            destination
                .with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        self.reset_view(ui);
        *self.undo.borrow_mut() = undo;
        *self.redo.borrow_mut() = redo;
        self.update_history(ui);
        apply_view(ui, &view);
        self.filter_cards(ui);
        self.update_ruler(ui);
        ui.set_workspace_status("Current view · not saved for this copy".into());
        ui.set_feedback("Saved a new project copy · further edits use this copy".into());
        Ok(())
    }

    fn refresh_recovery(&self, ui: &PrismStudio) {
        let candidates = self
            .project_path
            .borrow()
            .as_deref()
            .map(prism_persistence::list_autosaves)
            .transpose();
        match candidates {
            Ok(Some(candidates)) => {
                ui.set_recovery_count(i32::try_from(candidates.len()).unwrap_or(i32::MAX));
                if let Some(candidate) = candidates.first() {
                    ui.set_recovery_revision(candidate.revision.to_string().into());
                    ui.set_recovery_path(candidate.path.display().to_string().into());
                } else {
                    ui.set_recovery_revision("".into());
                    ui.set_recovery_path("".into());
                }
                *self.recovery_candidates.borrow_mut() = candidates;
            }
            Ok(None) => {
                self.recovery_candidates.borrow_mut().clear();
                ui.set_recovery_count(0);
                ui.set_recovery_revision("".into());
                ui.set_recovery_path("".into());
            }
            Err(error) => {
                self.recovery_candidates.borrow_mut().clear();
                ui.set_recovery_count(0);
                ui.set_recovery_revision("".into());
                ui.set_recovery_path("".into());
                ui.set_feedback(format!("Could not inspect recovery files: {error}").into());
            }
        }
    }

    fn reset_autosave_scheduler(&self, ui: &PrismStudio) {
        self.autosave_scheduler.borrow_mut().take();
        self.pending_autosave.borrow_mut().take();
        let Some(path) = self.project_path.borrow().clone() else {
            self.autosave_preferences
                .set(AutosavePreferences::default());
            ui.set_autosave_enabled(false);
            ui.set_autosave_retention(5);
            ui.set_autosave_status("UNAVAILABLE".into());
            return;
        };
        let store = AutosavePreferencesStore::new(path.with_extension("autosave.json"));
        let preferences = match store.load() {
            Ok(preferences) => preferences,
            Err(error) => {
                ui.set_feedback(format!("Could not load autosave preferences: {error}").into());
                AutosavePreferences::default()
            }
        };
        self.autosave_preferences.set(preferences);
        ui.set_autosave_enabled(preferences.enabled);
        ui.set_autosave_retention(
            i32::try_from(preferences.retained_revisions).unwrap_or(i32::MAX),
        );
        if !preferences.enabled {
            ui.set_autosave_status("DISABLED".into());
            return;
        }
        match prism_persistence::AutosaveScheduler::spawn(path, 2) {
            Ok(scheduler) => {
                *self.autosave_scheduler.borrow_mut() = Some(scheduler);
                ui.set_autosave_status("READY".into());
            }
            Err(error) => {
                ui.set_autosave_status("FAILED".into());
                ui.set_feedback(format!("Could not start autosave: {error}").into());
            }
        }
    }

    fn update_autosave_preferences(
        &self,
        ui: &PrismStudio,
        toggle_enabled: bool,
    ) -> Result<(), String> {
        let project_path =
            self.project_path.borrow().clone().ok_or_else(|| {
                "Save the project before changing autosave preferences".to_owned()
            })?;
        let mut preferences = self.autosave_preferences.get();
        if toggle_enabled {
            preferences.enabled = !preferences.enabled;
        } else {
            preferences.retained_revisions = match preferences.retained_revisions {
                1 => 5,
                5 => 10,
                10 => 20,
                _ => 1,
            };
        }
        AutosavePreferencesStore::new(project_path.with_extension("autosave.json"))
            .save(preferences)
            .map_err(|error| error.to_string())?;
        self.autosave_preferences.set(preferences);
        self.reset_autosave_scheduler(ui);
        ui.set_feedback(
            format!(
                "Autosave {} · retaining {} revisions",
                if preferences.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                preferences.retained_revisions
            )
            .into(),
        );
        Ok(())
    }

    fn queue_autosave(&self, ui: &PrismStudio, project: &Project) {
        if !self.autosave_preferences.get().enabled {
            ui.set_autosave_status("DISABLED".into());
            return;
        }
        if self.pending_autosave.borrow().is_some() {
            *self.pending_autosave.borrow_mut() = Some(project.clone());
            ui.set_autosave_status("BUSY".into());
            return;
        }
        let scheduler_ref = self.autosave_scheduler.borrow();
        let Some(scheduler) = scheduler_ref.as_ref() else {
            ui.set_autosave_status("UNAVAILABLE".into());
            return;
        };
        match scheduler.try_submit(project) {
            prism_persistence::AutosaveSubmit::Queued => {
                ui.set_autosave_status("QUEUED".into());
            }
            prism_persistence::AutosaveSubmit::QueueFull => {
                *self.pending_autosave.borrow_mut() = Some(project.clone());
                ui.set_autosave_status("BUSY".into());
            }
            prism_persistence::AutosaveSubmit::WorkerStopped => {
                ui.set_autosave_status("FAILED".into());
            }
        }
    }

    fn retry_pending_autosave(&self, ui: &PrismStudio) {
        let Some(project) = self.pending_autosave.borrow().clone() else {
            return;
        };
        let scheduler_ref = self.autosave_scheduler.borrow();
        let Some(scheduler) = scheduler_ref.as_ref() else {
            return;
        };
        match scheduler.try_submit(&project) {
            prism_persistence::AutosaveSubmit::Queued => {
                self.pending_autosave.borrow_mut().take();
                ui.set_autosave_status("QUEUED".into());
            }
            prism_persistence::AutosaveSubmit::QueueFull => {
                ui.set_autosave_status("BUSY".into());
            }
            prism_persistence::AutosaveSubmit::WorkerStopped => {
                ui.set_autosave_status("FAILED".into());
            }
        }
    }

    fn recover_latest(&self, ui: &PrismStudio) -> Result<(), String> {
        let source = self
            .recovery_candidates
            .borrow()
            .first()
            .map(|candidate| candidate.path.clone())
            .ok_or_else(|| "No recovery candidate is available".to_owned())?;
        let default_name = self
            .project_path
            .borrow()
            .as_deref()
            .and_then(Path::file_stem)
            .and_then(|name| name.to_str())
            .map_or_else(
                || "Recovered.prism".to_owned(),
                |name| format!("{name}.recovered.prism"),
            );
        let Some(destination) = rfd::FileDialog::new()
            .set_title("Recover Prism Autosave As")
            .set_file_name(default_name)
            .add_filter("Prism Project", &["prism"])
            .save_file()
        else {
            return Ok(());
        };
        let revision = prism_persistence::recover_autosave(&source, &destination)
            .map_err(|error| error.to_string())?;
        let project = prism_persistence::load(&destination).map_err(|error| error.to_string())?;
        apply_project_summary(ui, &project, &destination);
        ui.set_export_output(
            destination
                .with_extension("export.mp4")
                .display()
                .to_string()
                .into(),
        );
        *self.project.borrow_mut() = Some(project);
        *self.project_path.borrow_mut() = Some(destination);
        self.reset_view(ui);
        ui.set_feedback(
            format!("Recovered autosave revision {revision} into a new project").into(),
        );
        Ok(())
    }

    fn reset_view(&self, ui: &PrismStudio) {
        self.stop_playback(ui);
        if let Some(generation) = self.proxy_generation.borrow().as_ref() {
            generation.cancelled.store(true, Ordering::Release);
        }
        self.proxy_generation.borrow_mut().take();
        self.proxy_availability.borrow_mut().clear();
        ui.set_proxy_generating(false);
        self.undo.borrow_mut().clear();
        self.redo.borrow_mut().clear();
        self.update_history(ui);
        ui.set_timeline_zoom(1.0);
        ui.set_timeline_offset(0.0);
        ui.set_snap_enabled(true);
        self.refresh_recovery(ui);
        self.reset_autosave_scheduler(ui);
        self.cache_eviction.borrow_mut().take();
        self.media_artifacts.borrow_mut().take();
        if let Some(root) = self.cache_root() {
            *self.media_artifacts.borrow_mut() = Some(MediaArtifactWorker::spawn(root, 8));
        }
        self.update_cache_state(ui);
        self.source_duration.set(RationalTime::ZERO);
        self.source_position.set(RationalTime::ZERO);
        self.source_in.set(RationalTime::ZERO);
        self.source_out.set(RationalTime::ZERO);
        ui.set_source_ready(false);
        self.update_source_labels(ui);
        self.reset_thumbnails(ui);
        self.reset_waveforms(ui);
        ui.set_inspector_name("".into());
        ui.set_inspector_path("".into());
        ui.set_selected_proxy("".into());
        ui.set_proxy_status(PROXY_MISSING.into());
        self.use_proxy.set(false);
        ui.set_proxy_active(false);
        ui.set_inspector_locked(false);
        ui.set_selected_clip_index(-1);
        ui.set_selected_media_index(-1);
        ui.set_selected_media("".into());
        ui.set_selected_source_path("".into());
        ui.set_source_preview_size("".into());
        ui.set_source_file_status("Select source footage to view its details".into());
        ui.set_has_source_frame(false);
        ui.set_source_detail("Select footage in the media browser".into());
        self.source_preview.invalidate();
        ui.set_active_workspace("EDIT".into());
        self.restore_view(ui);
        self.seek_time(ui, RationalTime::ZERO);
    }

    fn save_view(&self, ui: &PrismStudio) -> Result<(), String> {
        let path = self
            .project_path
            .borrow()
            .as_ref()
            .map(|path| path.with_extension("workspace.json"))
            .ok_or_else(|| "Save a project before saving its workspace".to_owned())?;
        capture_view(ui)
            .save(&path)
            .map_err(|error| error.to_string())?;
        ui.set_workspace_status("Saved for this project".into());
        ui.set_feedback(
            "Workspace saved · this project will reopen with these panels and preferences".into(),
        );
        Ok(())
    }

    fn restore_view(&self, ui: &PrismStudio) {
        let path = self
            .project_path
            .borrow()
            .as_ref()
            .map(|path| path.with_extension("workspace.json"));
        let saved = path
            .as_ref()
            .map_or(Ok(None), |path| ViewPreferences::load(path));
        let view = match saved {
            Ok(Some(view)) => {
                ui.set_workspace_status("Restored saved view".into());
                view
            }
            Ok(None) => {
                ui.set_workspace_status("Default view".into());
                ViewPreferences::default()
            }
            Err(error) => {
                ui.set_workspace_status("Default view".into());
                ui.set_feedback(format!("Could not restore the saved workspace: {error}").into());
                ViewPreferences::default()
            }
        };
        apply_view(ui, &view);
        self.filter_cards(ui);
        self.update_ruler(ui);
    }

    fn cache_root(&self) -> Option<PathBuf> {
        self.project_path
            .borrow()
            .as_ref()
            .map(|path| path.with_extension("prism-cache"))
    }

    fn update_cache_state(&self, ui: &PrismStudio) {
        let pending = self
            .cache_eviction
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.pending_evictions() > 0);
        ui.set_cache_cleaning(pending);
        let Some(root) = self.cache_root() else {
            ui.set_cache_status("UNAVAILABLE".into());
            ui.set_cache_detail("Save the project to enable its derived cache".into());
            return;
        };
        match CacheLayout::new(root).inventory() {
            Ok(inventory) => {
                ui.set_cache_status(if pending { "CLEANING" } else { "READY" }.into());
                ui.set_cache_detail(
                    format!(
                        "{} artifacts · {}",
                        inventory.artifact_count,
                        format_bytes(inventory.total_bytes)
                    )
                    .into(),
                );
            }
            Err(error) => {
                ui.set_cache_status("FAILED".into());
                ui.set_cache_detail(error.to_string().into());
            }
        }
    }

    fn clean_cache(&self, ui: &PrismStudio) -> Result<(), String> {
        if self
            .cache_eviction
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.pending_evictions() > 0)
        {
            return Err("Cache cleanup is already running".to_owned());
        }
        let root = self
            .cache_root()
            .ok_or_else(|| "Save the project before cleaning its cache".to_owned())?;
        let worker = CacheEvictionWorker::spawn(CacheLayout::new(root), 1);
        worker
            .try_submit(CacheEvictionPolicy {
                max_bytes: Some(0),
                max_age: None,
            })
            .map_err(|error| error.to_string())?;
        *self.cache_eviction.borrow_mut() = Some(worker);
        ui.set_cache_cleaning(true);
        ui.set_cache_status("CLEANING".into());
        ui.set_feedback("Cleaning project derived cache in the background…".into());
        Ok(())
    }

    fn find_media_in_timeline(&self, ui: &PrismStudio) -> Result<(), String> {
        let target = {
            let project = self.project.borrow();
            let project = project.as_ref().ok_or("Open a project first")?;
            let media_id = project
                .media
                .keys()
                .nth(
                    usize::try_from(ui.get_selected_media_index())
                        .map_err(|_| "Select source footage first")?,
                )
                .ok_or("Selected media is no longer available")?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or("No sequence available")?;
            let uses = sequence
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .enumerate()
                .filter(|(_, clip)| clip.media_id == *media_id)
                .map(|(index, clip)| (index, clip.timeline_start))
                .collect::<Vec<_>>();
            uses.iter()
                .find(|(index, _)| {
                    i32::try_from(*index).is_ok_and(|index| index > ui.get_selected_clip_index())
                })
                .or_else(|| uses.first())
                .copied()
                .ok_or("This source is not used in the active sequence")?
        };
        self.stop_playback(ui);
        self.select_clip(
            ui,
            i32::try_from(target.0).map_err(|error| error.to_string())?,
        );
        self.seek_time(ui, target.1);
        ui.set_active_workspace("EDIT".into());
        ui.set_inspector_open(true);
        ui.set_inspector_tab("Properties".into());
        ui.set_inspector_section("Clip".into());
        ui.set_feedback("Located source usage · choose Find again for the next occurrence".into());
        Ok(())
    }

    fn select_media(&self, ui: &PrismStudio, index: i32) {
        let project = self.project.borrow();
        let Some(project) = project.as_ref() else {
            return;
        };
        let Ok(media_index) = usize::try_from(index) else {
            return;
        };
        let Some(asset) = project.media.values().nth(media_index) else {
            return;
        };
        self.proxy_availability.borrow_mut().remove(&asset.id);
        ui.set_selected_media_index(index);
        ui.set_selected_media_uses(
            i32::try_from(*media_usage(project).get(&asset.id).unwrap_or(&0)).unwrap_or(i32::MAX),
        );
        ui.set_selected_media(
            Path::new(&asset.source_path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&asset.source_path)
                .into(),
        );
        self.source_position.set(RationalTime::ZERO);
        self.source_duration.set(RationalTime::ZERO);
        self.source_in.set(RationalTime::ZERO);
        self.source_out.set(RationalTime::ZERO);
        ui.set_source_ready(false);
        ui.set_source_preview_size("".into());
        ui.set_source_file_status(
            match std::fs::metadata(&asset.source_path) {
                Ok(metadata) if metadata.is_file() => {
                    format!("Source online · {}", format_bytes(metadata.len()))
                }
                _ => "Source offline · locate the original file".to_owned(),
            }
            .into(),
        );
        self.update_source_labels(ui);
        ui.set_has_source_frame(false);
        ui.set_selected_source_path(asset.source_path.clone().into());
        ui.set_source_detail(asset.source_path.clone().into());
        ui.set_selected_proxy(asset.proxy_path.clone().unwrap_or_default().into());
        let status = proxy_status(
            Path::new(&asset.source_path),
            asset.proxy_path.as_deref().map(Path::new),
            asset.proxy_source_fingerprint.as_ref(),
        );
        self.proxy_availability
            .borrow_mut()
            .insert(asset.id, status == PROXY_AVAILABLE);
        ui.set_proxy_status(status.into());
        if status != PROXY_AVAILABLE {
            self.use_proxy.set(false);
            ui.set_proxy_active(false);
        }
        let (has_video, has_audio, source_duration) = media_stream_summary(project, asset.id);
        if !has_video && has_audio {
            self.source_preview.invalidate();
            let duration = source_duration.unwrap_or(RationalTime::ZERO);
            self.source_duration.set(duration);
            self.source_out.set(duration);
            let available = Path::new(&asset.source_path).is_file() && duration.is_positive();
            ui.set_source_ready(available);
            ui.set_source_preview_size(
                format!("Audio source · {}", self.display_timecode(duration)).into(),
            );
            ui.set_source_detail("Audio source · waveform available in timeline".into());
            self.update_source_labels(ui);
            return;
        }
        let preview_path = if self.use_proxy.get() && status == PROXY_AVAILABLE {
            asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
        } else {
            &asset.source_path
        };
        self.source_preview
            .request(PathBuf::from(preview_path), RationalTime::ZERO);
    }

    fn proxy_is_available(&self, asset: &MediaAsset) -> bool {
        if let Some(available) = self.proxy_availability.borrow().get(&asset.id) {
            return *available;
        }
        let available = proxy_status(
            Path::new(&asset.source_path),
            asset.proxy_path.as_deref().map(Path::new),
            asset.proxy_source_fingerprint.as_ref(),
        ) == PROXY_AVAILABLE;
        self.proxy_availability
            .borrow_mut()
            .insert(asset.id, available);
        available
    }

    fn match_frame(&self, ui: &PrismStudio) -> Result<(), String> {
        let (media_index, time) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| {
                matched_source(project, ui.get_selected_clip_index(), self.playhead.get())
            })
            .ok_or_else(|| "Select a timeline clip to match its source frame".to_owned())?;
        let index = i32::try_from(media_index).map_err(|error| error.to_string())?;
        self.stop_playback(ui);
        self.select_media(ui, index);
        let source = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| {
                let asset = project.media.values().nth(media_index).cloned()?;
                let (has_video, _, _) = media_stream_summary(project, asset.id);
                Some((asset, has_video))
            })
            .ok_or_else(|| "Selected source is no longer available".to_owned())?;
        let (asset, has_video) = source;
        self.source_position.set(time);
        self.update_source_labels(ui);
        if has_video {
            let path = if self.use_proxy.get() && self.proxy_is_available(&asset) {
                asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
            } else {
                &asset.source_path
            };
            self.source_preview.request(PathBuf::from(path), time);
        }
        ui.set_inspector_open(true);
        ui.set_inspector_tab("Properties".into());
        ui.set_inspector_section("Source".into());
        ui.set_feedback(
            format!(
                "Matched {} at {}",
                if has_video {
                    "source frame"
                } else {
                    "audio source"
                },
                self.display_timecode(time)
            )
            .into(),
        );
        Ok(())
    }

    fn select_clip(&self, ui: &PrismStudio, index: i32) {
        ui.set_selected_clip_index(index);
        self.update_inspector(ui);
        let selected = self.project.borrow().as_ref().and_then(|p| {
            let sequence = p.sequences.values().next()?;
            let clip = sequence
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .nth(usize::try_from(index).ok()?)?;
            let media_index = p.media.keys().position(|id| *id == clip.media_id)?;
            Some((clip.timeline_start, media_index))
        });
        if let Some((time, media_index)) = selected {
            if let Ok(index) = i32::try_from(media_index) {
                ui.set_selected_media_index(index);
                if let Some(name) = slint::Model::row_data(&ui.get_media_items(), media_index) {
                    ui.set_selected_media(name);
                }
                self.select_media(ui, index);
            }
            let _ = time;
        }
    }

    fn seek_ratio(&self, ui: &PrismStudio, ratio: f32) {
        self.stop_playback(ui);
        if !ratio.is_finite() {
            return;
        }
        let duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next()?.duration().ok());
        if let Some(duration) = duration {
            // Pointer coordinates are display values; quantize once at the UI boundary.
            let time = self
                .quantize_to_frame(duration.as_seconds_f64() * f64::from(ratio.clamp(0.0, 1.0)));
            self.seek_time(ui, time);
        }
    }

    fn seek_time(&self, ui: &PrismStudio, time: RationalTime) {
        let project = self.project.borrow();
        let sequence = project.as_ref().and_then(|p| p.sequences.values().next());
        let duration = sequence
            .and_then(|s| s.duration().ok())
            .unwrap_or(RationalTime::ZERO);
        let time = time.min(duration).max(RationalTime::ZERO);
        self.playhead.set(time);
        ui.set_timecode(self.display_timecode(time).into());
        #[allow(clippy::cast_possible_truncation)]
        let ratio = if duration.is_positive() {
            (time.as_seconds_f64() / duration.as_seconds_f64()) as f32
        } else {
            0.0
        };
        ui.set_playhead_ratio(ratio);
        let span = 1.0 / ui.get_timeline_zoom().clamp(1.0, 8.0);
        let offset = ui.get_timeline_offset();
        if ratio < offset || ratio > offset + span {
            ui.set_timeline_offset((ratio - span * 0.15).clamp(0.0, 1.0 - span));
            self.update_ruler(ui);
        }
        let target = sequence.and_then(|s| {
            s.tracks
                .iter()
                .filter(|t| t.visible && t.kind == TrackKind::Video)
                .flat_map(|t| &t.clips)
                .find_map(|clip| {
                    let end = clip.timeline_start.checked_add(clip.source_duration).ok()?;
                    if time < clip.timeline_start || time >= end {
                        return None;
                    }
                    let asset = project.as_ref()?.media.get(&clip.media_id)?;
                    let source_time = time
                        .checked_sub(clip.timeline_start)
                        .ok()?
                        .checked_add(clip.source_start)
                        .ok()?;
                    let proxy_is_available = self.proxy_is_available(asset);
                    let path = if self.use_proxy.get() && proxy_is_available {
                        asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
                    } else {
                        &asset.source_path
                    };
                    Some((PathBuf::from(path), source_time))
                })
        });
        if let Some((path, time)) = target {
            if self.playing.get() {
                // Keep the last completed frame visible while the next one decodes.
                self.program_preview
                    .request_playback(path, time, self.frame_rate());
            } else {
                ui.set_has_program_frame(false);
                self.program_preview.request(path, time);
            }
            ui.set_preview_status("Decoding frame…".into());
        } else {
            ui.set_has_program_frame(false);
            ui.set_program_luma_scope(slint::ModelRc::new(
                slint::VecModel::from(Vec::<f32>::new()),
            ));
            ui.set_program_luma_waveform(slint::ModelRc::new(slint::VecModel::from(Vec::<
                LumaScopeColumn,
            >::new(
            ))));
            ui.set_program_vectorscope(slint::ModelRc::new(slint::VecModel::from(
                Vec::<f32>::new(),
            )));
            self.program_preview.invalidate();
            ui.set_preview_status(
                if duration.is_positive() {
                    "Gap / no visible video at playhead"
                } else {
                    "Import footage to create a sequence"
                }
                .into(),
            );
        }
    }

    fn stop_audio_preview(&self) {
        self.audio_clip.set(None);
        if let Some(mut child) = self.audio_process.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut child) = self.audio_decoder.borrow_mut().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.audio_end.set(RationalTime::ZERO);
        self.audio_clock.borrow_mut().take();
    }

    fn stop_playback(&self, ui: &PrismStudio) {
        self.program_preview.invalidate();
        self.playing.set(false);
        self.playback_started.set(None);
        self.stop_audio_preview();
        ui.set_playing(false);
        ui.set_playback_buffering(false);
    }

    fn start_audio_playback(&self) -> Result<(), String> {
        self.stop_audio_preview();
        let plan = self
            .project
            .borrow()
            .as_ref()
            .map(|project| crate::audio_preview::plan(project, self.playhead.get()))
            .transpose()?
            .flatten();
        let Some(plan) = plan else {
            return Ok(());
        };
        let end = self
            .playhead
            .get()
            .checked_add(plan.duration)
            .map_err(|error| error.to_string())?;
        let (decoder, player, clock) =
            crate::audio_preview::start(&plan, &media_tool("ffmpeg"), &media_tool("ffplay"))?;
        self.audio_end.set(end);
        *self.audio_clock.borrow_mut() = Some(clock);
        *self.audio_decoder.borrow_mut() = Some(decoder);
        *self.audio_process.borrow_mut() = Some(player);
        Ok(())
    }

    fn follow_audio_timeline(&self, ui: &PrismStudio) {
        // The decoder/player pair stays alive across cuts and silence gaps.
        self.audio_clip.set(
            self.project
                .borrow()
                .as_ref()
                .and_then(|project| audio_preview_segment(project, self.playhead.get()))
                .map(|segment| segment.clip_id),
        );
        let exited = self
            .audio_process
            .borrow_mut()
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        let remaining = self
            .audio_end
            .get()
            .checked_sub(self.playhead.get())
            .unwrap_or(RationalTime::ZERO);
        if let Some(status) = exited {
            let completed = status.success() && remaining.as_seconds_f64() <= 0.25;
            let end = self.audio_end.get();
            self.stop_playback(ui);
            if completed {
                self.seek_time(ui, end);
            } else {
                ui.set_feedback(
                    "Audio preview stopped early · check source audio and playback device".into(),
                );
            }
        }
    }

    fn commit(&self, ui: &PrismStudio, command: Command, label: &str) -> Result<(), String> {
        self.commit_commands(ui, vec![command], label)
    }

    fn selected_media_id(&self, ui: &PrismStudio) -> Result<MediaAssetId, String> {
        self.project
            .borrow()
            .as_ref()
            .and_then(|project| {
                project
                    .media
                    .keys()
                    .nth(usize::try_from(ui.get_selected_media_index()).ok()?)
                    .copied()
            })
            .ok_or_else(|| "Select media first".to_owned())
    }

    fn assign_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let source = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| PathBuf::from(&asset.source_path))
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let Some(path) = rfd::FileDialog::new()
            .set_title("Assign Proxy Media")
            .add_filter("Video", &["mp4", "mov", "m4v", "mkv", "webm"])
            .pick_file()
        else {
            return Ok(());
        };
        let index = ui.get_selected_media_index();
        let source_fingerprint = fingerprint(&source).map_err(|error| error.to_string())?;
        self.commit(
            ui,
            Command::SetMediaProxy {
                media_id,
                proxy_path: Some(path.display().to_string()),
                source_fingerprint: Some(ProxySourceFingerprint {
                    sha256: source_fingerprint.sha256,
                    byte_length: source_fingerprint.byte_length,
                }),
            },
            "Assign media proxy",
        )?;
        self.use_proxy.set(true);
        ui.set_proxy_active(true);
        self.select_media(ui, index);
        Ok(())
    }

    fn relink_source(&self, ui: &PrismStudio) -> Result<(), String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Relink Source Media")
            .add_filter(
                "Video",
                &["mp4", "mov", "m4v", "mkv", "avi", "webm", "mpeg", "mpg"],
            )
            .pick_file()
        else {
            return Ok(());
        };
        self.relink_source_to(ui, &path)
    }

    fn relink_source_to(&self, ui: &PrismStudio, path: &Path) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let metadata = FfprobeInspector::new(media_tool("ffprobe"))
            .inspect(path)
            .map_err(|error| error.to_string())?;
        if !metadata
            .streams
            .iter()
            .any(|stream| matches!(stream.kind, StreamKind::Video(_)))
        {
            return Err("The replacement file has no video stream".to_owned());
        }
        let available = metadata
            .duration
            .filter(|duration| duration.is_positive())
            .ok_or_else(|| "The replacement file has no valid duration".to_owned())?;
        let required = self
            .project
            .borrow()
            .as_ref()
            .into_iter()
            .flat_map(|project| project.sequences.values())
            .flat_map(|sequence| &sequence.tracks)
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.media_id == media_id)
            .filter_map(|clip| clip.source_start.checked_add(clip.source_duration).ok())
            .max()
            .unwrap_or(RationalTime::ZERO);
        if available < required {
            return Err(format!(
                "Replacement is too short: clips require {}, file provides {}",
                timing_text(required),
                timing_text(available)
            ));
        }
        let index = ui.get_selected_media_index();
        self.commit(
            ui,
            Command::RelinkMedia {
                media_id,
                source_path: path.display().to_string(),
            },
            "Relink source media",
        )?;
        self.reset_thumbnails(ui);
        self.select_media(ui, index);
        self.update_inspector(ui);
        Ok(())
    }

    fn clear_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let index = ui.get_selected_media_index();
        self.commit(
            ui,
            Command::SetMediaProxy {
                media_id,
                proxy_path: None,
                source_fingerprint: None,
            },
            "Clear media proxy",
        )?;
        self.use_proxy.set(false);
        ui.set_proxy_active(false);
        self.select_media(ui, index);
        Ok(())
    }

    fn generate_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let source = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| PathBuf::from(&asset.source_path))
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let default_name = source
            .file_stem()
            .and_then(|name| name.to_str())
            .map_or_else(
                || "Prism.proxy.mp4".to_owned(),
                |name| format!("{name}.proxy.mp4"),
            );
        let Some(output) = rfd::FileDialog::new()
            .set_title("Generate Prism Proxy")
            .set_file_name(default_name)
            .add_filter("MP4 proxy", &["mp4"])
            .save_file()
        else {
            return Ok(());
        };
        self.start_proxy_generation(ui, media_id, source, output)
    }

    fn regenerate_proxy(&self, ui: &PrismStudio) -> Result<(), String> {
        let media_id = self.selected_media_id(ui)?;
        let (source, base) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.get(&media_id))
            .map(|asset| {
                let source = PathBuf::from(&asset.source_path);
                let base = asset.proxy_path.as_ref().map_or_else(
                    || {
                        let stem = source
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Prism");
                        source.with_file_name(format!("{stem}.proxy.mp4"))
                    },
                    PathBuf::from,
                );
                (source, base)
            })
            .ok_or_else(|| "Selected media is unavailable".to_owned())?;
        let output = next_proxy_version(&base)?;
        self.start_proxy_generation(ui, media_id, source, output)
    }

    fn start_proxy_generation(
        &self,
        ui: &PrismStudio,
        media_id: MediaAssetId,
        source: PathBuf,
        output: PathBuf,
    ) -> Result<(), String> {
        if self.proxy_generation.borrow().is_some() {
            return Err("A proxy is already being generated".to_owned());
        }
        let source_fingerprint = fingerprint(&source).map_err(|error| error.to_string())?;
        let profile = ProxyProfile::new(960, 540, "libx264", Some("aac".to_owned()), "mp4")
            .map_err(|error| error.to_string())?;
        let request = ProxyRequest {
            source,
            output,
            profile,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = FfmpegProxyGenerator::new(media_tool("ffmpeg"))
                .generate(&request, Some(&worker_cancelled))
                .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        *self.proxy_generation.borrow_mut() = Some(ProxyGeneration {
            receiver,
            cancelled,
            media_id,
            source_fingerprint: ProxySourceFingerprint {
                sha256: source_fingerprint.sha256,
                byte_length: source_fingerprint.byte_length,
            },
        });
        ui.set_proxy_generating(true);
        ui.set_feedback("Generating 540p proxy in the background…".into());
        Ok(())
    }

    fn commit_commands(
        &self,
        ui: &PrismStudio,
        commands: Vec<Command>,
        label: &str,
    ) -> Result<(), String> {
        let mut updated = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let outcome = CommandEngine::execute_transaction(&mut updated, label, commands)
            .map_err(|e| e.to_string())?;
        let path = self
            .project_path
            .borrow()
            .clone()
            .ok_or_else(|| "Save a project first".to_owned())?;
        // Publish to the UI only after the validated edit has been saved successfully.
        prism_persistence::save_atomic(&updated, &path).map_err(|e| e.to_string())?;
        let audio_changed = self.project.borrow().as_ref().and_then(|project| {
            crate::audio_preview::plan(project, self.playhead.get())
                .ok()
                .flatten()
        }) != crate::audio_preview::plan(&updated, self.playhead.get())
            .ok()
            .flatten();
        if self.playing.get() && audio_changed {
            self.stop_playback(ui);
        }
        apply_project_summary(ui, &updated, &path);
        self.refresh_timeline_waveforms(ui);
        *self.project.borrow_mut() = Some(updated);
        self.filter_cards(ui);
        if let Some(project) = self.project.borrow().as_ref() {
            self.queue_autosave(ui, project);
        }
        let mut undo = self.undo.borrow_mut();
        undo.push(outcome.undo);
        if undo.len() > 50 {
            undo.remove(0);
        }
        drop(undo);
        self.redo.borrow_mut().clear();
        self.update_history(ui);
        self.update_inspector(ui);
        self.update_ruler(ui);
        ui.set_feedback(format!("{label} · saved").into());
        self.seek_time(ui, self.playhead.get());
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn edit_aligned_av(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        let moving = command == "timeline.move_av";
        let trimming = command == "timeline.trim_av";
        let position = if moving {
            parse_timing(ui.get_paired_start().as_str())?
        } else {
            self.playhead.get()
        };
        let (commands, selected_id) = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let commands = if trimming {
                let index = ui.get_selected_clip_index();
                // Check pair eligibility before starting media inspection.
                aligned_av_commands(project, index, "timeline.move_av", RationalTime::ZERO)?;
                let clip = project
                    .sequences
                    .values()
                    .next()
                    .and_then(|sequence| {
                        sequence
                            .tracks
                            .iter()
                            .flat_map(|track| &track.clips)
                            .nth(usize::try_from(index).ok()?)
                    })
                    .ok_or("Selected clip unavailable")?;
                let asset = project
                    .media
                    .get(&clip.media_id)
                    .ok_or("Media unavailable")?;
                let metadata = FfprobeInspector::new(media_tool("ffprobe"))
                    .inspect(Path::new(&asset.source_path))
                    .map_err(|error| error.to_string())?;
                let available = metadata
                    .duration
                    .ok_or("Original source duration unavailable")?;
                aligned_av_range_commands(
                    project,
                    index,
                    clip.timeline_start,
                    parse_timing(ui.get_paired_source_in().as_str())?,
                    parse_timing(ui.get_paired_duration().as_str())?,
                    available,
                )?
            } else {
                aligned_av_commands(project, ui.get_selected_clip_index(), command, position)?
            };
            let id = project
                .sequences
                .values()
                .next()
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .nth(usize::try_from(ui.get_selected_clip_index()).ok()?)
                })
                .map(|clip| clip.id);
            (commands, id)
        };
        let label = match command {
            "timeline.split_av" => "Split aligned A/V",
            "timeline.move_av" => "Move aligned A/V",
            "timeline.trim_av" => "Change aligned A/V source range",
            "timeline.ripple_delete_av" => "Ripple delete aligned A/V",
            _ => "Delete aligned A/V",
        };
        self.commit_commands(ui, commands, label)?;
        let selection = if moving || trimming {
            self.project
                .borrow()
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .position(|clip| Some(clip.id) == selected_id)
                })
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1)
        } else {
            -1
        };
        ui.set_selected_clip_index(selection);
        self.update_inspector(ui);
        if moving || trimming {
            self.stop_playback(ui);
            self.seek_time(
                ui,
                if moving {
                    position
                } else {
                    self.playhead.get()
                },
            );
        }
        Ok(())
    }

    fn edit_clip(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        let index = usize::try_from(ui.get_selected_clip_index())
            .map_err(|_| "Select a clip first".to_owned())?;
        let operation = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip) = sequence
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                .nth(index)
                .ok_or_else(|| "Selected clip is no longer available".to_owned())?;
            match command {
                "timeline.split" => Command::SplitClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    right_clip_id: ClipId::new(),
                    timeline_position: self.playhead.get(),
                },
                "timeline.ripple_delete" => Command::RippleDelete {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                },
                _ => Command::DeleteClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                },
            }
        };
        // End the immutable project borrow before commit replaces the snapshot.
        let label = match command {
            "timeline.split" => "Split clip",
            "timeline.ripple_delete" => "Ripple delete clip",
            _ => "Delete clip",
        };
        self.commit(ui, operation, label)?;
        ui.set_selected_clip_index(-1);
        self.update_inspector(ui);
        Ok(())
    }

    fn edit_clip_or_aligned(&self, command: &str, ui: &PrismStudio) -> Result<(), String> {
        let paired = self.project.borrow().as_ref().is_some_and(|project| {
            aligned_av_companion_index(project, ui.get_selected_clip_index()).is_some()
        });
        if paired {
            let aligned = match command {
                "timeline.split" => "timeline.split_av",
                "timeline.ripple_delete" => "timeline.ripple_delete_av",
                _ => "timeline.delete_av",
            };
            self.edit_aligned_av(aligned, ui)
        } else {
            self.edit_clip(command, ui)
        }
    }

    fn add_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let (sequence_id, marker_number) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .map(|sequence| (sequence.id, sequence.markers.len() + 1))
            .ok_or_else(|| "No sequence available".to_owned())?;
        self.commit(
            ui,
            Command::AddMarker {
                sequence_id,
                id: MarkerId::new(),
                time: self.playhead.get(),
                label: format!("Marker {marker_number}"),
            },
            "Add timeline marker",
        )
    }

    fn rename_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let label = ui.get_marker_label().trim().to_owned();
        if label.is_empty() {
            return Err("Give the marker a name before saving".to_owned());
        }
        let index = usize::try_from(ui.get_selected_marker_index())
            .map_err(|_| "Select a marker first".to_owned())?;
        let (sequence_id, marker) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next())
            .and_then(|s| s.markers.get(index).map(|m| (s.id, m.clone())))
            .ok_or_else(|| "Marker unavailable".to_owned())?;
        self.commit_commands(
            ui,
            vec![
                Command::DeleteMarker {
                    sequence_id,
                    marker_id: marker.id,
                },
                Command::AddMarker {
                    sequence_id,
                    id: marker.id,
                    time: marker.time,
                    label: label.clone(),
                },
            ],
            "Rename timeline marker",
        )?;
        let new_index = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next())
            .and_then(|s| s.markers.iter().position(|m| m.id == marker.id));
        ui.set_selected_marker_index(new_index.and_then(|i| i32::try_from(i).ok()).unwrap_or(-1));
        ui.set_marker_label(label.into());
        Ok(())
    }

    fn delete_marker(&self, ui: &PrismStudio) -> Result<(), String> {
        let index = usize::try_from(ui.get_selected_marker_index())
            .map_err(|_| "Select a marker first".to_owned())?;
        let (sequence_id, marker_id) = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .and_then(|sequence| {
                sequence
                    .markers
                    .get(index)
                    .map(|marker| (sequence.id, marker.id))
            })
            .ok_or_else(|| "Selected marker is unavailable".to_owned())?;
        self.commit(
            ui,
            Command::DeleteMarker {
                sequence_id,
                marker_id,
            },
            "Delete timeline marker",
        )?;
        ui.set_selected_marker_index(-1);
        Ok(())
    }

    fn toggle_dissolve(&self, ui: &PrismStudio) -> Result<(), String> {
        let selected = usize::try_from(ui.get_selected_clip_index())
            .map_err(|_| "Select a video clip first".to_owned())?;
        let operation = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip_index) = sequence
                .tracks
                .iter()
                .flat_map(|track| (0..track.clips.len()).map(move |index| (track, index)))
                .nth(selected)
                .ok_or_else(|| "Selected clip is unavailable".to_owned())?;
            if track.kind != TrackKind::Video {
                return Err("Dissolves require a video track".to_owned());
            }
            let outgoing = &track.clips[clip_index];
            let incoming = track
                .clips
                .get(clip_index + 1)
                .ok_or_else(|| "Select a clip with an adjacent video clip after it".to_owned())?;
            if let Some(transition) = sequence.transitions.iter().find(|transition| {
                transition.outgoing_clip == outgoing.id && transition.incoming_clip == incoming.id
            }) {
                Command::DeleteTransition {
                    sequence_id: sequence.id,
                    transition_id: transition.id,
                }
            } else {
                let duration = self
                    .frame_duration()
                    .checked_mul_i64(15)
                    .map_err(|error| error.to_string())?
                    .min(outgoing.source_duration)
                    .min(incoming.source_duration);
                Command::AddTransition {
                    sequence_id: sequence.id,
                    id: TransitionId::new(),
                    outgoing_clip: outgoing.id,
                    incoming_clip: incoming.id,
                    duration,
                    alignment: prism_project::TransitionAlignment::Centered,
                    kind: TransitionKind::Dissolve,
                }
            }
        };
        let label = if ui.get_selected_has_transition() {
            "Remove dissolve"
        } else {
            "Add dissolve"
        };
        self.commit(ui, operation, label)
    }

    fn duplicate_clip(&self, ui: &PrismStudio) -> Result<(), String> {
        let operations = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            duplicate_selected_commands(project, ui.get_selected_clip_index())?
        };
        self.commit_commands(ui, operations, "Duplicate clip to track end")
    }

    fn change_track(&self, ui: &PrismStudio, index: i32, action: &str) -> Result<(), String> {
        let (operation, selected_clip_id) = {
            let project = self.project.borrow();
            let sequence = project
                .as_ref()
                .and_then(|p| p.sequences.values().next())
                .ok_or_else(|| "No sequence available".to_owned())?;
            let track = sequence
                .tracks
                .get(usize::try_from(index).map_err(|e| e.to_string())?)
                .ok_or_else(|| "Track unavailable".to_owned())?;
            let selected_clip_id = sequence
                .tracks
                .iter()
                .flat_map(|track| &track.clips)
                .nth(usize::try_from(ui.get_selected_clip_index()).unwrap_or(usize::MAX))
                .map(|clip| clip.id);
            let operation = match action {
                "lock" => Command::SetTrackLocked {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    locked: !track.locked,
                },
                "mute" => Command::SetTrackMuted {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    muted: !track.muted,
                },
                "visible" => Command::SetTrackVisible {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    visible: !track.visible,
                },
                "up" | "down" => {
                    let current = usize::try_from(index).map_err(|e| e.to_string())?;
                    let destination_index = if action == "up" {
                        current
                            .checked_sub(1)
                            .ok_or_else(|| "Track is already first".to_owned())?
                    } else {
                        let next = current + 1;
                        if next >= sequence.tracks.len() {
                            return Err("Track is already last".to_owned());
                        }
                        next
                    };
                    Command::ReorderTrack {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        destination_index,
                    }
                }
                _ => return Err("Unknown track action".to_owned()),
            };
            (operation, selected_clip_id)
        };
        self.commit(ui, operation, "Track updated")?;
        if let Some(selected_clip_id) = selected_clip_id {
            let new_index = self
                .project
                .borrow()
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .position(|clip| clip.id == selected_clip_id)
                })
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1);
            ui.set_selected_clip_index(new_index);
            self.update_inspector(ui);
        }
        Ok(())
    }

    fn reset_thumbnails(&self, ui: &PrismStudio) {
        self.thumbnail_preview.invalidate();
        self.thumbnail_current.set(None);
        let project = self.project.borrow();
        let media = project
            .as_ref()
            .map(|p| p.media.values().enumerate().collect::<Vec<_>>())
            .unwrap_or_default();
        let video_media = project
            .as_ref()
            .map(|project| {
                project
                    .sequences
                    .values()
                    .flat_map(|sequence| &sequence.tracks)
                    .filter(|track| track.kind == TrackKind::Video)
                    .flat_map(|track| &track.clips)
                    .map(|clip| clip.media_id)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        *self.cards.borrow_mut() = media
            .iter()
            .map(|(index, asset)| {
                let metadata = std::fs::metadata(&asset.source_path)
                    .ok()
                    .filter(std::fs::Metadata::is_file);
                MediaCard {
                    uses: 0,
                    offline: metadata.is_none(),
                    file_size: metadata
                        .as_ref()
                        .map_or_else(
                            || "Unavailable".to_owned(),
                            |metadata| format_bytes(metadata.len()),
                        )
                        .into(),
                    index: i32::try_from(*index).unwrap_or(i32::MAX),
                    name: Path::new(&asset.source_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&asset.source_path)
                        .into(),
                    frame: slint::Image::default(),
                    has_frame: false,
                    status: if metadata.is_none() {
                        "Source offline"
                    } else if video_media.contains(&asset.id) {
                        "Preparing preview…"
                    } else {
                        "Audio source · waveform in timeline"
                    }
                    .into(),
                }
            })
            .collect();
        *self.thumbnail_queue.borrow_mut() = media
            .iter()
            .filter(|(_, asset)| video_media.contains(&asset.id))
            .map(|(i, a)| (*i, PathBuf::from(&a.source_path)))
            .collect();
        self.thumbnail_total
            .set(self.thumbnail_queue.borrow().len());
        self.thumbnail_completed.set(0);
        self.thumbnail_failed.set(0);
        self.filter_cards(ui);
        self.next_thumbnail(ui);
    }

    fn filter_cards(&self, ui: &PrismStudio) {
        let query = ui.get_search().to_lowercase();
        let usage = self
            .project
            .borrow()
            .as_ref()
            .map(|project| {
                let counts = media_usage(project);
                project
                    .media
                    .keys()
                    .enumerate()
                    .map(|(index, id)| {
                        (
                            i32::try_from(index).unwrap_or(i32::MAX),
                            i32::try_from(*counts.get(id).unwrap_or(&0)).unwrap_or(i32::MAX),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let paths = self
            .project
            .borrow()
            .as_ref()
            .map(|project| {
                project
                    .media
                    .values()
                    .enumerate()
                    .map(|(index, asset)| {
                        (
                            i32::try_from(index).unwrap_or(i32::MAX),
                            asset.source_path.clone(),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        ui.set_selected_media_uses(*usage.get(&ui.get_selected_media_index()).unwrap_or(&0));
        let mut cards = self
            .cards
            .borrow()
            .iter()
            .filter(|item| item.name.to_lowercase().contains(&query))
            .cloned()
            .map(|mut item| {
                item.uses = *usage.get(&item.index).unwrap_or(&0);
                if let Some(path) = paths.get(&item.index) {
                    item.offline = !Path::new(path).is_file();
                }
                item
            })
            .filter(|item| match ui.get_media_filter() {
                1 => item.uses > 0,
                2 => item.uses == 0,
                3 => item.offline,
                _ => true,
            })
            .collect::<Vec<_>>();
        if ui.get_media_sort() != 0 {
            cards.sort_by_cached_key(|item| (item.name.to_lowercase(), item.index));
            if ui.get_media_sort() == 2 {
                cards.reverse();
            }
        }
        ui.set_media_cards(slint::ModelRc::new(slint::VecModel::from(cards)));
    }

    fn update_media_task_state(&self, ui: &PrismStudio) {
        let total = self.thumbnail_total.get();
        let completed = self.thumbnail_completed.get();
        let failed = self.thumbnail_failed.get();
        let pending_decode = total.saturating_sub(completed);
        let pending_writes = self
            .media_artifacts
            .borrow()
            .as_ref()
            .map_or(0, MediaArtifactWorker::pending_writes);
        let pending_waveforms = self.waveform_worker.borrow().pending();
        let pending = pending_decode
            .saturating_add(pending_writes)
            .saturating_add(pending_waveforms);
        ui.set_media_task_pending(i32::try_from(pending).unwrap_or(i32::MAX));
        ui.set_media_task_status(
            if total == 0 {
                "IDLE"
            } else if pending_decode > 0 {
                "DECODING"
            } else if pending_writes > 0 {
                "CACHING"
            } else if pending_waveforms > 0 {
                "WAVEFORMS"
            } else if failed > 0 || self.waveform_failed.get() > 0 {
                "COMPLETE WITH ERRORS"
            } else {
                "COMPLETE"
            }
            .into(),
        );
        ui.set_media_task_detail(
            format!(
                "{completed}/{total} previews · {}/{} waveforms{}{}{}",
                self.waveform_completed.get(),
                self.waveform_total.get(),
                if failed > 0 {
                    format!(" · {failed} unavailable")
                } else {
                    String::new()
                },
                if pending_writes > 0 {
                    format!(" · {pending_writes} cache writes")
                } else {
                    String::new()
                },
                if self.waveform_failed.get() > 0 {
                    format!(" · {} waveform errors", self.waveform_failed.get())
                } else {
                    String::new()
                }
            )
            .into(),
        );
    }

    fn next_thumbnail(&self, ui: &PrismStudio) {
        if let Some((index, path)) = self.thumbnail_queue.borrow_mut().pop_front() {
            self.thumbnail_current.set(Some(index));
            self.thumbnail_preview
                .request_thumbnail(path, self.cache_root());
        } else {
            self.thumbnail_current.set(None);
        }
        self.update_media_task_state(ui);
    }

    fn reset_waveforms(&self, ui: &PrismStudio) {
        self.waveform_worker
            .replace(WaveformWorker::spawn(media_tool("ffmpeg"), 64));
        self.waveform_total.set(0);
        self.waveform_completed.set(0);
        self.waveform_failed.set(0);
        self.waveforms.borrow_mut().clear();
        let Some(cache_root) = self.cache_root() else {
            self.update_media_task_state(ui);
            return;
        };
        let requests = self
            .project
            .borrow()
            .as_ref()
            .map(|project| {
                let mut audio_media = BTreeMap::<MediaAssetId, RationalTime>::new();
                for clip in project
                    .sequences
                    .values()
                    .flat_map(|sequence| &sequence.tracks)
                    .filter(|track| track.kind == TrackKind::Audio)
                    .flat_map(|track| &track.clips)
                {
                    let Ok(source_end) = clip.source_start.checked_add(clip.source_duration) else {
                        continue;
                    };
                    audio_media
                        .entry(clip.media_id)
                        .and_modify(|current| *current = (*current).max(source_end))
                        .or_insert(source_end);
                }
                audio_media
                    .into_iter()
                    .filter_map(|(id, source_end)| {
                        let asset = project.media.get(&id)?;
                        let samples = source_end
                            .to_ticks(48_000, prism_timeline::RoundingMode::Ceil)
                            .ok()
                            .and_then(|samples| usize::try_from(samples).ok())?
                            .max(1);
                        Some(WaveformRequest {
                            path: PathBuf::from(&asset.source_path),
                            cache_root: cache_root.clone(),
                            sample_rate: 48_000,
                            max_samples: samples,
                            samples_per_point: 1_024,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.waveform_total.set(requests.len());
        for request in requests {
            if let Err(error) = self.waveform_worker.borrow().try_submit(request) {
                self.waveform_failed
                    .set(self.waveform_failed.get().saturating_add(1));
                ui.set_feedback(format!("Could not queue audio waveform: {error}").into());
            }
        }
        self.update_media_task_state(ui);
    }

    fn refresh_timeline_waveforms(&self, ui: &PrismStudio) {
        let clips = ui.get_timeline_clips();
        let waveforms = self.waveforms.borrow();
        for index in 0..clips.row_count() {
            let Some(mut clip) = clips.row_data(index) else {
                continue;
            };
            if !clip.audio {
                continue;
            }
            let peaks = waveforms
                .get(clip.source_path.as_str())
                .map(|bars| {
                    waveform_range(bars, clip.waveform_start_ratio, clip.waveform_width_ratio)
                })
                .unwrap_or_default();
            clip.waveform = slint::ModelRc::new(slint::VecModel::from(peaks));
            clips.set_row_data(index, clip);
        }
    }

    fn update_history(&self, ui: &PrismStudio) {
        let undo = self.undo.borrow();
        let redo = self.redo.borrow();
        ui.set_can_undo(!undo.is_empty());
        ui.set_can_redo(!redo.is_empty());
        let items = undo
            .iter()
            .map(|record| HistoryItem {
                label: record.label().into(),
                applied: true,
            })
            .chain(redo.iter().rev().map(|record| HistoryItem {
                label: record.label().into(),
                applied: false,
            }))
            .collect::<Vec<_>>();
        ui.set_history_items(slint::ModelRc::new(slint::VecModel::from(items)));
    }

    fn apply_history(&self, ui: &PrismStudio, redo_action: bool) -> Result<(), String> {
        let mut updated = self
            .project
            .borrow()
            .clone()
            .ok_or_else(|| "Open a project first".to_owned())?;
        let path = self
            .project_path
            .borrow()
            .clone()
            .ok_or_else(|| "Project destination unavailable".to_owned())?;
        let (from, to) = if redo_action {
            (&self.redo, &self.undo)
        } else {
            (&self.undo, &self.redo)
        };
        let record = from
            .borrow_mut()
            .pop()
            .ok_or_else(|| "No edit available".to_owned())?;
        let label = record.label().to_owned();
        let inverse = record.apply(&mut updated).map_err(|e| e.to_string())?;
        if let Err(error) = prism_persistence::save_atomic(&updated, &path) {
            // Restore the history entry if disk persistence failed.
            let restored = inverse.apply(&mut updated).map_err(|e| e.to_string())?;
            from.borrow_mut().push(restored);
            return Err(error.to_string());
        }
        to.borrow_mut().push(inverse);
        let audio_changed = self.project.borrow().as_ref().and_then(|project| {
            crate::audio_preview::plan(project, self.playhead.get())
                .ok()
                .flatten()
        }) != crate::audio_preview::plan(&updated, self.playhead.get())
            .ok()
            .flatten();
        if self.playing.get() && audio_changed {
            self.stop_playback(ui);
        }
        apply_project_summary(ui, &updated, &path);
        self.refresh_timeline_waveforms(ui);
        *self.project.borrow_mut() = Some(updated);
        self.filter_cards(ui);
        if let Some(project) = self.project.borrow().as_ref() {
            self.queue_autosave(ui, project);
        }
        ui.set_selected_clip_index(-1);
        self.update_history(ui);
        self.update_inspector(ui);
        self.update_ruler(ui);
        self.seek_time(ui, self.playhead.get());
        if label == "Relink source media" {
            let media_index = ui.get_selected_media_index();
            self.reset_thumbnails(ui);
            self.select_media(ui, media_index);
            self.update_inspector(ui);
        }
        ui.set_feedback(
            format!(
                "{} {label} · saved",
                if redo_action { "Redid" } else { "Undid" }
            )
            .into(),
        );
        Ok(())
    }

    fn update_companion_selection(ui: &PrismStudio, project: Option<&Project>) {
        ui.set_selected_companion_index(
            project
                .and_then(|project| {
                    aligned_av_companion_index(project, ui.get_selected_clip_index())
                })
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1),
        );
    }

    fn update_inspector(&self, ui: &PrismStudio) {
        let project = self.project.borrow();
        Self::update_companion_selection(ui, project.as_ref());
        let pair = project
            .as_ref()
            .ok_or_else(|| "Select a timeline clip".to_owned())
            .and_then(|project| {
                aligned_av_commands(
                    project,
                    ui.get_selected_clip_index(),
                    "timeline.delete_av",
                    self.playhead.get(),
                )
            });
        ui.set_can_edit_av(pair.is_ok());
        ui.set_aligned_av_detail(
            match pair {
                Ok(_) => {
                    "Matching source and timing · both clips will be edited together".to_owned()
                }
                Err(reason) => reason,
            }
            .into(),
        );
        let selected = project.as_ref().and_then(|p| {
            let sequence = p.sequences.values().next()?;
            let (row, track, clip) = sequence
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(row, track)| track.clips.iter().map(move |clip| (row, track, clip)))
                .nth(usize::try_from(ui.get_selected_clip_index()).ok()?)?;
            Some((p, sequence, row, track, clip))
        });
        if let Some((project, sequence, row, track, clip)) = selected {
            let path = project
                .media
                .get(&clip.media_id)
                .map_or("Offline media", |m| m.source_path.as_str());
            ui.set_inspector_name(
                Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path)
                    .into(),
            );
            ui.set_inspector_path(path.into());
            ui.set_inspector_track(
                format!(
                    "{}{}",
                    if track.kind == TrackKind::Audio {
                        "A"
                    } else {
                        "V"
                    },
                    row + 1
                )
                .into(),
            );
            ui.set_inspector_locked(track.locked);
            ui.set_selected_is_audio(track.kind == TrackKind::Audio);
            let waveform = self
                .waveforms
                .borrow()
                .get(path)
                .cloned()
                .unwrap_or_default();
            ui.set_selected_waveform(slint::ModelRc::new(slint::VecModel::from(waveform)));
            ui.set_selected_track_index(i32::try_from(row).unwrap_or(-1));
            ui.set_clip_start(timing_text(clip.timeline_start));
            ui.set_paired_start(timing_text(clip.timeline_start));
            ui.set_paired_source_in(timing_text(clip.source_start));
            ui.set_paired_duration(timing_text(clip.source_duration));
            ui.set_clip_source_in(timing_text(clip.source_start));
            ui.set_clip_duration(timing_text(clip.source_duration));
            let next = track
                .clips
                .iter()
                .position(|item| item.id == clip.id)
                .and_then(|index| track.clips.get(index + 1));
            ui.set_selected_has_next(track.kind == TrackKind::Video && next.is_some());
            ui.set_selected_has_transition(next.is_some_and(|next| {
                sequence.transitions.iter().any(|transition| {
                    transition.outgoing_clip == clip.id && transition.incoming_clip == next.id
                })
            }));
        } else {
            ui.set_inspector_name("".into());
            ui.set_inspector_path("".into());
            ui.set_inspector_locked(false);
            ui.set_selected_is_audio(false);
            ui.set_selected_waveform(slint::ModelRc::new(
                slint::VecModel::from(Vec::<f32>::new()),
            ));
            ui.set_selected_track_index(-1);
            ui.set_selected_has_next(false);
            ui.set_selected_has_transition(false);
        }
    }

    fn apply_timing(&self, ui: &PrismStudio) -> Result<(), String> {
        let start = parse_timing(ui.get_clip_start().as_str())?;
        let source_start = parse_timing(ui.get_clip_source_in().as_str())?;
        let duration = parse_timing(ui.get_clip_duration().as_str())?;
        if !duration.is_positive() {
            return Err("Duration must be greater than zero".to_owned());
        }
        let commands = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "No project available".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let (track, clip) = sequence
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
                .nth(
                    usize::try_from(ui.get_selected_clip_index())
                        .map_err(|_| "Select a clip first".to_owned())?,
                )
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let asset = project
                .media
                .get(&clip.media_id)
                .ok_or_else(|| "Media unavailable".to_owned())?;
            let metadata = FfprobeInspector::new(media_tool("ffprobe"))
                .inspect(Path::new(&asset.source_path))
                .map_err(|e| e.to_string())?;
            let available = metadata
                .duration
                .ok_or_else(|| "Source duration unavailable".to_owned())?;
            if source_start
                .checked_add(duration)
                .map_err(|e| e.to_string())?
                > available
            {
                return Err("The requested trim exceeds the source duration".to_owned());
            }
            if aligned_av_companion_index(project, ui.get_selected_clip_index()).is_some() {
                aligned_av_range_commands(
                    project,
                    ui.get_selected_clip_index(),
                    start,
                    source_start,
                    duration,
                    available,
                )?
            } else {
                vec![Command::TrimClip {
                    sequence_id: sequence.id,
                    track_id: track.id,
                    clip_id: clip.id,
                    timeline_start: start,
                    source_start,
                    source_duration: duration,
                }]
            }
        };
        self.commit_commands(ui, commands, "Change clip timing")
    }

    fn adjust_clip_frame(&self, ui: &PrismStudio, operation: &str) -> Result<(), String> {
        let frame = self.frame_duration();
        if matches!(operation, "timeline.move_left" | "timeline.move_right") {
            let current = parse_timing(ui.get_clip_start().as_str())?;
            let timeline_start = if operation == "timeline.move_left" {
                current
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?
            } else {
                current
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?
            };
            if timeline_start.is_negative() {
                return Err("A clip cannot move before sequence start".to_owned());
            }
            let commands = {
                let project = self.project.borrow();
                let project = project
                    .as_ref()
                    .ok_or_else(|| "No project available".to_owned())?;
                move_selected_commands(project, ui.get_selected_clip_index(), timeline_start)?
            };
            return self.commit_commands(ui, commands, "Move clip one frame");
        }

        let original_start = ui.get_clip_start();
        let original_source = ui.get_clip_source_in();
        let original_duration = ui.get_clip_duration();
        let mut start = parse_timing(original_start.as_str())?;
        let mut source = parse_timing(original_source.as_str())?;
        let mut duration = parse_timing(original_duration.as_str())?;
        match operation {
            "timeline.trim_in_left" => {
                start = start
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                if start.is_negative() || source.is_negative() {
                    return Err("No earlier source frame is available".to_owned());
                }
            }
            "timeline.trim_in_right" => {
                start = start
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
            }
            "timeline.trim_out_left" => {
                duration = duration
                    .checked_sub(frame)
                    .map_err(|error| error.to_string())?;
            }
            "timeline.trim_out_right" => {
                duration = duration
                    .checked_add(frame)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("Unknown clip adjustment".to_owned()),
        }
        if !duration.is_positive() {
            return Err("A clip must remain at least one frame long".to_owned());
        }
        ui.set_clip_start(timing_text(start));
        ui.set_clip_source_in(timing_text(source));
        ui.set_clip_duration(timing_text(duration));
        if let Err(error) = self.apply_timing(ui) {
            ui.set_clip_start(original_start);
            ui.set_clip_source_in(original_source);
            ui.set_clip_duration(original_duration);
            return Err(error);
        }
        Ok(())
    }

    fn precision_edit_frame(&self, ui: &PrismStudio, operation: &str) -> Result<(), String> {
        let frame = self.frame_duration();
        let delta = if operation.ends_with("_left") {
            RationalTime::ZERO
                .checked_sub(frame)
                .map_err(|error| error.to_string())?
        } else {
            frame
        };
        let kind = if operation.contains("slip") {
            "slip"
        } else if operation.contains("roll_in") {
            "roll_in"
        } else if operation.contains("roll_out") {
            "roll_out"
        } else {
            "slide"
        };
        self.precision_edit_delta(ui, kind, delta)
    }

    fn precision_edit_delta(
        &self,
        ui: &PrismStudio,
        operation: &str,
        delta: RationalTime,
    ) -> Result<(), String> {
        let commands = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let sequence = project
                .sequences
                .values()
                .next()
                .ok_or_else(|| "No sequence available".to_owned())?;
            let selected = usize::try_from(ui.get_selected_clip_index())
                .map_err(|_| "Select a clip first".to_owned())?;
            let (track, clip_index) = sequence
                .tracks
                .iter()
                .flat_map(|track| (0..track.clips.len()).map(move |index| (track, index)))
                .nth(selected)
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let clip = &track.clips[clip_index];
            match operation {
                "slip" => slip_selected_commands(project, ui.get_selected_clip_index(), delta)?,
                "roll_in" => {
                    let previous = clip_index
                        .checked_sub(1)
                        .and_then(|index| track.clips.get(index))
                        .ok_or_else(|| {
                            "Roll In requires an adjacent clip before this clip".to_owned()
                        })?;
                    vec![Command::RollEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        left_clip_id: previous.id,
                        right_clip_id: clip.id,
                        delta,
                    }]
                }
                "roll_out" => {
                    let next = track.clips.get(clip_index + 1).ok_or_else(|| {
                        "Roll Out requires an adjacent clip after this clip".to_owned()
                    })?;
                    vec![Command::RollEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        left_clip_id: clip.id,
                        right_clip_id: next.id,
                        delta,
                    }]
                }
                "slide" => {
                    let previous = clip_index
                        .checked_sub(1)
                        .and_then(|index| track.clips.get(index))
                        .ok_or_else(|| "Slide requires clips on both sides".to_owned())?;
                    let next = track
                        .clips
                        .get(clip_index + 1)
                        .ok_or_else(|| "Slide requires clips on both sides".to_owned())?;
                    vec![Command::SlideEdit {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        previous_clip_id: previous.id,
                        clip_id: clip.id,
                        next_clip_id: next.id,
                        delta,
                    }]
                }
                _ => return Err("Unknown precision edit".to_owned()),
            }
        };
        let label = if operation == "slip" {
            "Slip clip"
        } else if operation.starts_with("roll") {
            "Roll edit"
        } else {
            "Slide clip"
        };
        self.commit_commands(ui, commands, label)
    }

    #[allow(clippy::too_many_lines)]
    fn drag_clip(
        &self,
        ui: &PrismStudio,
        index: i32,
        delta_ratio: f32,
        edge: &str,
    ) -> Result<(), String> {
        if !delta_ratio.is_finite() {
            return Err("Invalid timeline drag".to_owned());
        }
        ui.set_selected_clip_index(index);
        self.update_inspector(ui);
        let sequence_duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next()?.duration().ok())
            .ok_or_else(|| "No sequence available".to_owned())?;
        let delta = if ui.get_snap_enabled() {
            let frame = self.frame_duration();
            #[allow(clippy::cast_possible_truncation)]
            let frames = (sequence_duration.as_seconds_f64() * f64::from(delta_ratio)
                / frame.as_seconds_f64())
            .round() as i64;
            frame
                .checked_mul_i64(frames)
                .map_err(|error| error.to_string())?
        } else {
            #[allow(clippy::cast_possible_truncation)]
            let ticks = (sequence_duration.as_seconds_f64() * f64::from(delta_ratio) * 300.0)
                .round() as i64;
            RationalTime::new(ticks, 300).map_err(|error| error.to_string())?
        };
        if delta.is_zero() {
            return Ok(());
        }
        if matches!(edge, "slip" | "slide" | "roll_in" | "roll_out") {
            return self.precision_edit_delta(ui, edge, delta);
        }
        if edge == "move" {
            let current = parse_timing(ui.get_clip_start().as_str())?;
            let target = current
                .checked_add(delta)
                .map_err(|error| error.to_string())?;
            if target.is_negative() {
                return Err("A clip cannot move before sequence start".to_owned());
            }
            let commands = {
                let project = self.project.borrow();
                let project = project
                    .as_ref()
                    .ok_or_else(|| "No project available".to_owned())?;
                let sequence = project
                    .sequences
                    .values()
                    .next()
                    .ok_or_else(|| "No sequence available".to_owned())?;
                let (track, clip) = sequence
                    .tracks
                    .iter()
                    .flat_map(|track| track.clips.iter().map(move |clip| (track, clip)))
                    .nth(usize::try_from(index).map_err(|_| "Select a clip first".to_owned())?)
                    .ok_or_else(|| "Selected clip unavailable".to_owned())?;
                let (has_video, has_audio, _) = media_stream_summary(project, clip.media_id);
                if has_video && has_audio {
                    aligned_av_commands(project, index, "timeline.move_av", target)?
                } else {
                    vec![Command::MoveClip {
                        sequence_id: sequence.id,
                        track_id: track.id,
                        clip_id: clip.id,
                        timeline_start: target,
                    }]
                }
            };
            return self.commit_commands(ui, commands, "Drag clip");
        }

        let original_start = ui.get_clip_start();
        let original_source = ui.get_clip_source_in();
        let original_duration = ui.get_clip_duration();
        let mut start = parse_timing(original_start.as_str())?;
        let mut source = parse_timing(original_source.as_str())?;
        let mut duration = parse_timing(original_duration.as_str())?;
        match edge {
            "in" => {
                start = start
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
                source = source
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
                duration = duration
                    .checked_sub(delta)
                    .map_err(|error| error.to_string())?;
            }
            "out" => {
                duration = duration
                    .checked_add(delta)
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err("Unknown timeline drag edge".to_owned()),
        }
        if start.is_negative() || source.is_negative() || !duration.is_positive() {
            return Err("The drag would create an invalid clip range".to_owned());
        }
        ui.set_clip_start(timing_text(start));
        ui.set_clip_source_in(timing_text(source));
        ui.set_clip_duration(timing_text(duration));
        let paired_result = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "No project available".to_owned())?;
            let clip = project
                .sequences
                .values()
                .next()
                .and_then(|sequence| {
                    sequence
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .nth(usize::try_from(index).ok()?)
                })
                .ok_or_else(|| "Selected clip unavailable".to_owned())?;
            let (has_video, has_audio, _) = media_stream_summary(project, clip.media_id);
            if has_video && has_audio {
                let asset = project
                    .media
                    .get(&clip.media_id)
                    .ok_or_else(|| "Media unavailable".to_owned())?;
                let available = FfprobeInspector::new(media_tool("ffprobe"))
                    .inspect(Path::new(&asset.source_path))
                    .map_err(|error| error.to_string())?
                    .duration
                    .ok_or_else(|| "Source duration unavailable".to_owned())?;
                Some(aligned_av_range_commands(
                    project, index, start, source, duration, available,
                )?)
            } else {
                None
            }
        };
        let result = if let Some(commands) = paired_result {
            self.commit_commands(ui, commands, "Drag aligned A/V")
        } else {
            self.apply_timing(ui)
        };
        if let Err(error) = result {
            ui.set_clip_start(original_start);
            ui.set_clip_source_in(original_source);
            ui.set_clip_duration(original_duration);
            return Err(error);
        }
        Ok(())
    }

    fn update_ruler(&self, ui: &PrismStudio) {
        let duration = self
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next()?.duration().ok())
            .unwrap_or(RationalTime::ZERO);
        let zoom = f64::from(ui.get_timeline_zoom()).clamp(1.0, 8.0);
        let offset = f64::from(ui.get_timeline_offset()).clamp(0.0, 1.0 - 1.0 / zoom);
        let labels = (0..6)
            .map(|index| {
                self.display_timecode(self.quantize_to_frame(
                    duration.as_seconds_f64() * (offset + f64::from(index) / (5.0 * zoom)),
                ))
                .into()
            })
            .collect::<Vec<slint::SharedString>>();
        ui.set_ruler_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
    }

    fn update_source_labels(&self, ui: &PrismStudio) {
        ui.set_source_timecode(self.display_timecode(self.source_position.get()).into());
        ui.set_source_duration_label(self.display_timecode(self.source_duration.get()).into());
        ui.set_source_in_label(self.display_timecode(self.source_in.get()).into());
        ui.set_source_out_label(self.display_timecode(self.source_out.get()).into());
        #[allow(clippy::cast_possible_truncation)]
        let ratio = if self.source_duration.get().is_positive() {
            (self.source_position.get().as_seconds_f64()
                / self.source_duration.get().as_seconds_f64()) as f32
        } else {
            0.0
        };
        ui.set_source_ratio(ratio);
        let duration = self.source_duration.get().as_seconds_f64();
        #[allow(clippy::cast_possible_truncation)]
        let (start, end) = if duration > 0.0 {
            (
                (self.source_in.get().as_seconds_f64() / duration) as f32,
                (self.source_out.get().as_seconds_f64() / duration) as f32,
            )
        } else {
            (0.0, 0.0)
        };
        ui.set_source_in_ratio(start.clamp(0.0, 1.0));
        ui.set_source_out_ratio(end.clamp(0.0, 1.0));
        ui.set_source_range_label(
            self.source_out
                .get()
                .checked_sub(self.source_in.get())
                .map_or_else(|_| "—".into(), display_timecode)
                .into(),
        );
    }

    fn seek_source(&self, ui: &PrismStudio, ratio: f32) {
        if !ratio.is_finite() || !self.source_duration.get().is_positive() {
            return;
        }
        let asset = self.project.borrow().as_ref().and_then(|project| {
            let asset = project
                .media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).ok()?)
                .cloned()?;
            let (has_video, _, _) = media_stream_summary(project, asset.id);
            Some((asset, has_video))
        });
        if let Some((asset, has_video)) = asset {
            let time = self.quantize_to_frame(
                self.source_duration.get().as_seconds_f64() * f64::from(ratio.clamp(0.0, 1.0)),
            );
            {
                let last_frame = self
                    .source_duration
                    .get()
                    .checked_sub(self.frame_duration())
                    .unwrap_or(RationalTime::ZERO)
                    .max(RationalTime::ZERO);
                let time = time.min(last_frame);
                self.source_position.set(time);
                self.update_source_labels(ui);
                if has_video {
                    let path = if self.use_proxy.get() && self.proxy_is_available(&asset) {
                        asset.proxy_path.as_deref().unwrap_or(&asset.source_path)
                    } else {
                        &asset.source_path
                    };
                    self.source_preview.request(PathBuf::from(path), time);
                }
            }
        }
    }

    fn edit_source(&self, ui: &PrismStudio, overwrite: bool) -> Result<(), String> {
        if !ui.get_source_ready() {
            return Err("Select an available source and mark its range first".into());
        }
        let duration = self
            .source_out
            .get()
            .checked_sub(self.source_in.get())
            .map_err(|e| e.to_string())?;
        let position = self.playhead.get();
        let commands = {
            let project = self.project.borrow();
            let project = project.as_ref().ok_or("Open a project first")?;
            let asset = project
                .media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).map_err(|e| e.to_string())?)
                .ok_or("Select a source first")?;
            source_edit_commands(
                project,
                asset.id,
                self.source_in.get(),
                duration,
                position,
                overwrite,
            )?
        };
        self.commit_commands(
            ui,
            commands,
            if overwrite {
                "Overwrite source range"
            } else {
                "Insert source range"
            },
        )?;
        ui.set_selected_clip_index(-1);
        self.update_inspector(ui);
        self.seek_time(
            ui,
            position.checked_add(duration).map_err(|e| e.to_string())?,
        );
        Ok(())
    }

    fn append_source(&self, ui: &PrismStudio) -> Result<(), String> {
        if !ui.get_source_ready() {
            return Err("Select an available source clip first".to_owned());
        }
        let duration = self
            .source_out
            .get()
            .checked_sub(self.source_in.get())
            .map_err(|e| e.to_string())?;
        if !duration.is_positive() {
            return Err("Mark a positive source range".to_owned());
        }
        let commands = {
            let project = self.project.borrow();
            let project = project
                .as_ref()
                .ok_or_else(|| "Open a project first".to_owned())?;
            let media = project
                .media
                .values()
                .nth(usize::try_from(ui.get_selected_media_index()).map_err(|e| e.to_string())?)
                .ok_or_else(|| "Select source footage".to_owned())?;
            append_source_commands(project, media.id, self.source_in.get(), duration)?
        };
        self.commit_commands(ui, commands, "Append source range")
    }

    #[allow(clippy::collapsible_if, clippy::too_many_lines)]
    fn poll(&self, ui: &PrismStudio) {
        let cache_result = self
            .cache_eviction
            .borrow()
            .as_ref()
            .and_then(|worker| worker.try_result().ok());
        if let Some(result) = cache_result {
            self.cache_eviction.borrow_mut().take();
            match result.result {
                Ok(report) => ui.set_feedback(
                    format!(
                        "Cache cleanup removed {} artifacts ({})",
                        report.removed_files,
                        format_bytes(report.removed_bytes)
                    )
                    .into(),
                ),
                Err(error) => ui.set_feedback(format!("Cache cleanup failed: {error}").into()),
            }
            self.update_cache_state(ui);
        }
        loop {
            let result = self
                .autosave_scheduler
                .borrow()
                .as_ref()
                .and_then(prism_persistence::AutosaveScheduler::try_result);
            let Some(result) = result else {
                break;
            };
            match result.result {
                Ok(()) => {
                    ui.set_autosave_status(format!("SAVED R{}", result.revision).into());
                    if let Some(path) = self.project_path.borrow().as_deref() {
                        if let Err(error) = prism_persistence::retain_autosaves(
                            path,
                            self.autosave_preferences.get().retained_revisions,
                        ) {
                            ui.set_feedback(format!("Autosave retention failed: {error}").into());
                        }
                    }
                    self.refresh_recovery(ui);
                }
                Err(error) => {
                    ui.set_autosave_status("FAILED".into());
                    ui.set_feedback(format!("Autosave failed: {error}").into());
                }
            }
        }
        self.retry_pending_autosave(ui);
        let proxy_result = self
            .proxy_generation
            .borrow()
            .as_ref()
            .and_then(|generation| {
                generation.receiver.try_recv().ok().map(|result| {
                    (
                        generation.media_id,
                        generation.source_fingerprint.clone(),
                        result,
                    )
                })
            });
        if let Some((media_id, source_fingerprint, result)) = proxy_result {
            self.proxy_generation.borrow_mut().take();
            ui.set_proxy_generating(false);
            match result {
                Ok(path) => {
                    let command = Command::SetMediaProxy {
                        media_id,
                        proxy_path: Some(path.display().to_string()),
                        source_fingerprint: Some(source_fingerprint),
                    };
                    if let Err(error) = self.commit(ui, command, "Generate media proxy") {
                        ui.set_feedback(error.into());
                    } else {
                        self.use_proxy.set(true);
                        ui.set_proxy_active(true);
                        let index = self
                            .project
                            .borrow()
                            .as_ref()
                            .and_then(|project| project.media.keys().position(|id| *id == media_id))
                            .and_then(|index| i32::try_from(index).ok())
                            .unwrap_or(-1);
                        self.select_media(ui, index);
                    }
                }
                Err(error) => ui.set_feedback(error.into()),
            }
        }
        if self.playing.get() {
            if let Some(started) = self.playback_started.get() {
                let observed = self
                    .audio_clock
                    .borrow()
                    .as_ref()
                    .map(crate::audio_clock::AudioClock::position);
                ui.set_playback_buffering(observed == Some(None));
                let delta = match observed {
                    Some(position) => position,
                    None => RationalTime::new(
                        i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
                        1000,
                    )
                    .ok(),
                };
                if let Some(delta) = delta {
                    if let Ok(target) = self.playback_origin.get().checked_add(delta) {
                        self.seek_time(ui, target);
                        if ui.get_playhead_ratio() >= 1.0 {
                            self.stop_playback(ui);
                        }
                    }
                } else if started.elapsed() > Duration::from_secs(5) {
                    self.stop_playback(ui);
                    ui.set_feedback("Audio playback clock unavailable · playback stopped".into());
                }
                let stalled = self
                    .audio_clock
                    .borrow()
                    .as_ref()
                    .is_some_and(crate::audio_clock::AudioClock::stalled);
                if self.playing.get() && stalled {
                    self.stop_playback(ui);
                    ui.set_feedback("Audio playback clock stalled · playback stopped".into());
                } else if self.playing.get() {
                    self.follow_audio_timeline(ui);
                }
            }
        }
        let mut cache_changed = false;
        loop {
            let cache_result = self
                .media_artifacts
                .borrow()
                .as_ref()
                .and_then(|worker| worker.try_result().ok());
            let Some(cache_result) = cache_result else {
                break;
            };
            cache_changed = true;
            match cache_result.result {
                Ok(_) | Err(CacheError::AlreadyExists(_)) => {}
                Err(error) => {
                    ui.set_feedback(format!("Could not cache media preview: {error}").into());
                }
            }
        }
        if cache_changed {
            self.update_cache_state(ui);
            self.update_media_task_state(ui);
        }
        let mut waveform_changed = false;
        loop {
            match self.waveform_worker.borrow().try_result() {
                Ok(Ok(result)) => {
                    waveform_changed = true;
                    self.waveforms.borrow_mut().insert(
                        result.path.display().to_string(),
                        waveform_bars(&result.points, 96),
                    );
                    self.waveform_completed
                        .set(self.waveform_completed.get().saturating_add(1));
                }
                Ok(Err(error)) => {
                    waveform_changed = true;
                    self.waveform_completed
                        .set(self.waveform_completed.get().saturating_add(1));
                    self.waveform_failed
                        .set(self.waveform_failed.get().saturating_add(1));
                    ui.set_feedback(format!("Could not prepare audio waveform: {error}").into());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if self.waveform_worker.borrow().pending() > 0 {
                        ui.set_feedback("Audio waveform worker stopped unexpectedly".into());
                    }
                    break;
                }
            }
        }
        if waveform_changed {
            self.update_cache_state(ui);
            self.update_media_task_state(ui);
            self.refresh_timeline_waveforms(ui);
            self.update_inspector(ui);
        }
        if let Some(result) = self.thumbnail_preview.poll_decoded() {
            if let Some(index) = self.thumbnail_current.take() {
                if let Some(card) = self.cards.borrow_mut().get_mut(index) {
                    if let Ok((frame, _, source_fingerprint, cache_hit)) = result {
                        if let Some(worker) = self.media_artifacts.borrow().as_ref()
                            && let Some(asset) = self
                                .project
                                .borrow()
                                .as_ref()
                                .and_then(|project| project.media.values().nth(index))
                        {
                            let entry = MediaCatalogEntry {
                                path: PathBuf::from(&asset.source_path),
                                kind: MediaItemKind::Video,
                                metadata: None,
                                fingerprint: source_fingerprint,
                                offline: false,
                                diagnostic: None,
                            };
                            if !cache_hit
                                && let Err(error) = worker.schedule_thumbnail_variant(
                                    &entry,
                                    &frame,
                                    frame.width,
                                    frame.height,
                                    BROWSER_THUMBNAIL_VARIANT,
                                )
                            {
                                ui.set_feedback(
                                    format!("Could not queue thumbnail cache: {error}").into(),
                                );
                            }
                        }
                        let buffer =
                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                &frame.rgba,
                                frame.width,
                                frame.height,
                            );
                        card.frame = slint::Image::from_rgba8(buffer);
                        card.has_frame = true;
                        card.status = "".into();
                    } else {
                        card.status = if card.offline {
                            "Source offline"
                        } else {
                            "Preview unavailable"
                        }
                        .into();
                        self.thumbnail_failed
                            .set(self.thumbnail_failed.get().saturating_add(1));
                    }
                }
                self.thumbnail_completed
                    .set(self.thumbnail_completed.get().saturating_add(1));
                self.filter_cards(ui);
                self.next_thumbnail(ui);
            }
        }
        if let Some(frame) = self.program_preview.poll_decoded() {
            match frame {
                Ok((frame, _, _, _)) => {
                    ui.set_program_luma_scope(slint::ModelRc::new(slint::VecModel::from(
                        luma_scope(&frame.rgba, 32, 65_536),
                    )));
                    ui.set_program_luma_waveform(slint::ModelRc::new(slint::VecModel::from(
                        luma_waveform(&frame.rgba, frame.width, frame.height, 64, 65_536),
                    )));
                    ui.set_program_vectorscope(slint::ModelRc::new(slint::VecModel::from(
                        vectorscope(&frame.rgba, 16, 65_536),
                    )));
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &frame.rgba,
                        frame.width,
                        frame.height,
                    );
                    ui.set_program_frame(slint::Image::from_rgba8(buffer));
                    ui.set_has_program_frame(true);
                    ui.set_preview_status("Frame ready".into());
                }
                Err(error) => {
                    self.stop_playback(ui);
                    ui.set_has_program_frame(false);
                    ui.set_program_luma_scope(slint::ModelRc::new(slint::VecModel::from(
                        Vec::<f32>::new(),
                    )));
                    ui.set_program_luma_waveform(slint::ModelRc::new(slint::VecModel::from(
                        Vec::<LumaScopeColumn>::new(),
                    )));
                    ui.set_program_vectorscope(slint::ModelRc::new(slint::VecModel::from(Vec::<
                        f32,
                    >::new(
                    ))));
                    ui.set_preview_status(
                        "Preview unavailable · check FFmpeg and source media".into(),
                    );
                    ui.set_feedback(error.into());
                }
            }
        }
        if self.playing.get() && self.program_preview.timed_out(Duration::from_secs(5)) {
            self.stop_playback(ui);
            ui.set_preview_status("Video decoder timed out · playback stopped".into());
            ui.set_feedback(
                "Video decoder did not produce a frame within 5 seconds · playback stopped".into(),
            );
        }
        if let Some(frame) = self.source_preview.poll() {
            match frame {
                Ok((frame, duration)) => {
                    let size = frame.size();
                    ui.set_source_preview_size(
                        format!(
                            "{} · {} × {} · {}",
                            if ui.get_proxy_active() {
                                "Proxy preview"
                            } else {
                                "Source preview"
                            },
                            size.width,
                            size.height,
                            self.display_timecode(duration)
                        )
                        .into(),
                    );
                    ui.set_source_frame(frame);
                    ui.set_has_source_frame(true);
                    if self.source_duration.get() != duration {
                        self.source_duration.set(duration);
                        self.source_out.set(duration);
                    }
                    ui.set_source_ready(duration.is_positive());
                    self.update_source_labels(ui);
                }
                Err(error) => {
                    ui.set_has_source_frame(false);
                    ui.set_source_ready(false);
                    ui.set_source_file_status(
                        if Path::new(ui.get_selected_source_path().as_str()).is_file() {
                            "Source found · preview unavailable"
                        } else {
                            "Source offline · choose Relink source"
                        }
                        .into(),
                    );
                    ui.set_source_detail(
                        "Preview unavailable · check FFmpeg and source media".into(),
                    );
                    ui.set_feedback(error.into());
                }
            }
        }
        let event = self
            .worker
            .borrow()
            .as_ref()
            .and_then(|worker| worker.try_next().ok().flatten());
        let Some(event) = event else {
            return;
        };
        let terminal = matches!(
            event,
            ExportWorkerEvent::Completed
                | ExportWorkerEvent::Cancelled
                | ExportWorkerEvent::Failed(_)
        );
        let mut controller = self.controller.borrow_mut();
        let already_cancelled = matches!(
            (controller.state(), &event),
            (
                Some(prism_export::ExportJobState::Cancelled),
                ExportWorkerEvent::Cancelled
            )
        );
        if !already_cancelled {
            let _ = controller.apply_worker_event(event);
        }
        if terminal {
            *self.worker.borrow_mut() = None;
        }
        apply_export_status(ui, controller.status().as_ref());
    }
}

fn import_media_into_project(project: &mut Project, path: &Path) -> Result<UndoRecord, String> {
    let metadata = FfprobeInspector::new(media_tool("ffprobe"))
        .inspect(path)
        .map_err(|error| error.to_string())?;
    let has_video = metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Video(_)));
    let has_audio = metadata
        .streams
        .iter()
        .any(|stream| matches!(stream.kind, StreamKind::Audio(_)));
    let source_frame_rate = metadata
        .streams
        .iter()
        .find_map(|stream| match &stream.kind {
            StreamKind::Video(video) => video.nominal_frame_rate.or(video.average_frame_rate),
            _ => None,
        });
    if !has_video && !has_audio {
        return Err(format!(
            "{} has no supported video or audio stream",
            path.display()
        ));
    }
    let duration = metadata
        .duration
        .filter(|duration| duration.is_positive())
        .ok_or_else(|| format!("{} has no valid duration", path.display()))?;
    insert_stream_media_into_project(
        project,
        path,
        duration,
        has_video,
        has_audio,
        source_frame_rate,
    )
}

#[cfg(test)]
fn insert_video_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
) -> Result<UndoRecord, String> {
    insert_media_into_project(project, path, duration, false)
}

#[cfg(test)]
fn insert_media_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
    has_audio: bool,
) -> Result<UndoRecord, String> {
    insert_stream_media_into_project(project, path, duration, true, has_audio, None)
}

#[allow(clippy::too_many_lines)]
fn insert_stream_media_into_project(
    project: &mut Project,
    path: &Path,
    duration: RationalTime,
    has_video: bool,
    has_audio: bool,
    source_frame_rate: Option<RationalTime>,
) -> Result<UndoRecord, String> {
    if !has_video && !has_audio {
        return Err("media must contain a supported video or audio stream".to_owned());
    }
    let media_id = MediaAssetId::new();
    let primary_kind = if has_video {
        TrackKind::Video
    } else {
        TrackKind::Audio
    };
    let (sequence_id, track_id, timeline_start, mut commands) =
        if let Some((sequence_id, sequence)) = project.sequences.iter().next() {
            let sequence_id = *sequence_id;
            if let Some(track) = sequence
                .tracks
                .iter()
                .find(|track| track.kind == primary_kind && !track.locked)
            {
                let timeline_start = sequence.duration().unwrap_or(RationalTime::ZERO);
                (sequence_id, track.id, timeline_start, Vec::new())
            } else {
                let track_id = TrackId::new();
                (
                    sequence_id,
                    track_id,
                    sequence.duration().unwrap_or(RationalTime::ZERO),
                    vec![Command::CreateTrack {
                        sequence_id,
                        id: track_id,
                        kind: primary_kind,
                    }],
                )
            }
        } else {
            let sequence_id = SequenceId::new();
            let track_id = TrackId::new();
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("Sequence 01")
                .to_owned();
            let mut commands = vec![Command::CreateSequence {
                id: sequence_id,
                name,
            }];
            if let Some(frame_rate) = source_frame_rate {
                commands.push(Command::SetSequenceFrameRate {
                    sequence_id,
                    frame_rate,
                });
            }
            commands.push(Command::CreateTrack {
                sequence_id,
                id: track_id,
                kind: primary_kind,
            });
            (sequence_id, track_id, RationalTime::ZERO, commands)
        };
    commands.insert(
        0,
        Command::ImportMedia {
            id: media_id,
            source_path: path.to_string_lossy().into_owned(),
        },
    );
    commands.push(Command::InsertClip {
        sequence_id,
        track_id,
        id: ClipId::new(),
        media_id,
        timeline_start,
        source_start: RationalTime::ZERO,
        source_duration: duration,
    });
    if has_video && has_audio {
        let audio_track_id = project
            .sequences
            .get(&sequence_id)
            .and_then(|sequence| {
                sequence
                    .tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Audio && !track.locked)
            })
            .map_or_else(
                || {
                    let id = TrackId::new();
                    commands.push(Command::CreateTrack {
                        sequence_id,
                        id,
                        kind: TrackKind::Audio,
                    });
                    id
                },
                |track| track.id,
            );
        commands.push(Command::InsertClip {
            sequence_id,
            track_id: audio_track_id,
            id: ClipId::new(),
            media_id,
            timeline_start,
            source_start: RationalTime::ZERO,
            source_duration: duration,
        });
    }
    CommandEngine::execute_transaction(project, "Import media", commands)
        .map(|outcome| outcome.undo)
        .map_err(|error| error.to_string())
}

#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
fn apply_project_summary(ui: &PrismStudio, project: &Project, project_path: &Path) {
    let summary = prism_studio::project_summary(project);
    ui.set_has_project(true);
    ui.set_project_name(summary.name.into());
    ui.set_project_path(project_path.display().to_string().into());
    ui.set_media_count(i32::try_from(summary.media_count).unwrap_or(i32::MAX));
    ui.set_clip_count(i32::try_from(summary.clip_count).unwrap_or(i32::MAX));
    let media_items = project
        .media
        .values()
        .map(|asset| {
            Path::new(&asset.source_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&asset.source_path)
                .into()
        })
        .collect::<Vec<slint::SharedString>>();
    ui.set_media_items(slint::ModelRc::new(slint::VecModel::from(media_items)));
    let timeline_items = project
        .sequences
        .values()
        .next()
        .map(|sequence| {
            let total_seconds = sequence
                .duration()
                .map(|duration| duration.as_seconds_f64().max(0.001))
                .unwrap_or(1.0);
            let mut source_ends = BTreeMap::<MediaAssetId, f64>::new();
            for clip in sequence
                .tracks
                .iter()
                .filter(|track| track.kind == TrackKind::Audio)
                .flat_map(|track| &track.clips)
            {
                if let Ok(source_end) = clip.source_start.checked_add(clip.source_duration) {
                    source_ends
                        .entry(clip.media_id)
                        .and_modify(|current| *current = current.max(source_end.as_seconds_f64()))
                        .or_insert_with(|| source_end.as_seconds_f64());
                }
            }
            let source_ends = &source_ends;
            sequence
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(row, track)| {
                    track.clips.iter().map(move |clip| {
                        let name = project
                            .media
                            .get(&clip.media_id)
                            .and_then(|asset| Path::new(&asset.source_path).file_name())
                            .and_then(|name| name.to_str())
                            .unwrap_or("Offline Media");
                        TimelineClipItem {
                            name: name.into(),
                            source_path: project
                                .media
                                .get(&clip.media_id)
                                .map_or("", |asset| asset.source_path.as_str())
                                .into(),
                            start_ratio: (clip.timeline_start.as_seconds_f64() / total_seconds)
                                as f32,
                            width_ratio: (clip.source_duration.as_seconds_f64() / total_seconds)
                                as f32,
                            row: i32::try_from(row).unwrap_or(3),
                            audio: track.kind == TrackKind::Audio,
                            waveform_start_ratio: source_ends
                                .get(&clip.media_id)
                                .map_or(0.0, |end| {
                                    (clip.source_start.as_seconds_f64() / end.max(0.001)) as f32
                                }),
                            waveform_width_ratio: source_ends.get(&clip.media_id).map_or(
                                0.0,
                                |end| {
                                    (clip.source_duration.as_seconds_f64() / end.max(0.001)) as f32
                                },
                            ),
                            waveform: slint::ModelRc::new(slint::VecModel::from(Vec::<f32>::new())),
                        }
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_clips(slint::ModelRc::new(slint::VecModel::from(timeline_items)));
    let sequence = project.sequences.values().next();
    let transition_items = sequence
        .map(|sequence| {
            let total_seconds = sequence
                .duration()
                .map(|duration| duration.as_seconds_f64().max(0.001))
                .unwrap_or(1.0);
            sequence
                .transitions
                .iter()
                .filter_map(|transition| {
                    sequence.tracks.iter().enumerate().find_map(|(row, track)| {
                        track
                            .clips
                            .iter()
                            .find(|clip| clip.id == transition.outgoing_clip)
                            .and_then(|clip| {
                                clip.timeline_start
                                    .checked_add(clip.source_duration)
                                    .ok()
                                    .map(|boundary| TimelineTransitionItem {
                                        ratio: (boundary.as_seconds_f64() / total_seconds) as f32,
                                        width_ratio: (transition.duration.as_seconds_f64()
                                            / total_seconds)
                                            as f32,
                                        row: i32::try_from(row).unwrap_or(3),
                                    })
                            })
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_transitions(slint::ModelRc::new(slint::VecModel::from(transition_items)));
    let tracks = sequence
        .map(|s| {
            s.tracks
                .iter()
                .enumerate()
                .map(|(index, track)| TrackItem {
                    name: format!(
                        "{}{}",
                        if track.kind == TrackKind::Audio {
                            "A"
                        } else {
                            "V"
                        },
                        index + 1
                    )
                    .into(),
                    audio: track.kind == TrackKind::Audio,
                    locked: track.locked,
                    muted: track.muted,
                    visible: track.visible,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_tracks(slint::ModelRc::new(slint::VecModel::from(tracks)));
    ui.set_sequence_name(sequence.map_or("Main sequence", |s| s.name.as_str()).into());
    let frame_rate = sequence
        .and_then(|sequence| project.sequence_frame_rate(sequence.id))
        .unwrap_or(RationalTime::from_integer(30));
    let duration = sequence
        .and_then(|s| s.duration().ok())
        .unwrap_or(RationalTime::ZERO);
    let marker_items = sequence
        .map(|sequence| {
            sequence
                .markers
                .iter()
                .map(|marker| TimelineMarkerItem {
                    timecode: display_timecode_at_rate(marker.time, frame_rate).into(),
                    label: marker.label.clone().into(),
                    ratio: if duration.is_positive() {
                        (marker.time.as_seconds_f64() / duration.as_seconds_f64()) as f32
                    } else {
                        0.0
                    },
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    ui.set_timeline_markers(slint::ModelRc::new(slint::VecModel::from(marker_items)));
    ui.set_selected_marker_index(-1);
    ui.set_duration_label(display_timecode_at_rate(duration, frame_rate).into());
    update_export_summary(ui, project);
    let labels = (0..6)
        .map(|index| {
            duration
                .checked_mul_i64(index)
                .and_then(|t| t.checked_div_i64(5))
                .map_or_else(
                    |_| "--:--:--:--".to_owned(),
                    |time| display_timecode_at_rate(time, frame_rate),
                )
                .into()
        })
        .collect::<Vec<slint::SharedString>>();
    ui.set_ruler_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
    ui.set_project_summary(
        format!(
            "{} media · {} sequences · {} tracks · {} clips · revision {}",
            summary.media_count,
            summary.sequence_count,
            summary.track_count,
            summary.clip_count,
            summary.revision
        )
        .into(),
    );
}

fn load_project_into_ui(
    ui: &PrismStudio,
    project_path: Option<PathBuf>,
) -> Result<Option<Project>, prism_persistence::PersistenceError> {
    let Some(project_path) = project_path else {
        return Ok(None);
    };
    let project = prism_persistence::load(&project_path)?;
    apply_project_summary(ui, &project, &project_path);
    ui.set_export_output(
        project_path
            .with_extension("export.mp4")
            .display()
            .to_string()
            .into(),
    );
    Ok(Some(project))
}

fn refresh_shortcuts(ui: &PrismStudio) {
    let query = ui.get_shortcut_query().to_lowercase();
    let premiere = ui.get_keyboard_preset() == "Premiere";
    let items = crate::edit_shortcuts::bindings(premiere)
        .into_iter()
        .filter(|binding| {
            query.is_empty()
                || binding.name.to_lowercase().contains(&query)
                || binding.detail.to_lowercase().contains(&query)
                || binding.key.to_lowercase().contains(&query)
        })
        .map(|binding| {
            let mut modifiers = Vec::new();
            if binding.primary {
                modifiers.push("Ctrl/Cmd");
            }
            if binding.shift {
                modifiers.push("Shift");
            }
            modifiers.push(binding.key);
            ShortcutItem {
                keys: modifiers.join("+").into(),
                name: binding.name.into(),
                detail: binding.detail.into(),
                available: !matches!(binding.target, crate::edit_shortcuts::Target::Missing(_)),
            }
        })
        .collect::<Vec<_>>();
    ui.set_shortcut_items(slint::ModelRc::new(slint::VecModel::from(items)));
}

#[allow(clippy::too_many_lines)]
fn bind_editor_callbacks(ui: &PrismStudio, runtime: &Rc<NativeExportRuntime>) {
    let weak_ui = ui.as_weak();
    let weak = ui.as_weak();
    ui.on_shortcuts_changed(move || {
        if let Some(ui) = weak.upgrade() {
            refresh_shortcuts(&ui);
        }
    });
    let weak = ui.as_weak();
    let shortcut_runtime = Rc::clone(runtime);
    ui.on_edit_shortcut(move |key, primary, shift, alt| {
        let Some(ui) = weak.upgrade() else {
            return false;
        };
        let premiere = ui.get_keyboard_preset() == "Premiere";
        let Some(target) =
            crate::edit_shortcuts::resolve(key.as_str(), primary, shift, alt, premiere)
        else {
            return false;
        };
        match target {
            crate::edit_shortcuts::Target::Tool(tool) => {
                ui.set_active_tool(tool.into());
            }
            crate::edit_shortcuts::Target::Command(command) => {
                if let Err(error) = shortcut_runtime.handle(command, &ui) {
                    ui.set_feedback(error.into());
                }
            }
            crate::edit_shortcuts::Target::Missing(detail) => {
                ui.set_feedback(format!("Shortcut reserved · {detail} is not built yet").into());
            }
        }
        true
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_source_seek(move |ratio| {
        if let Some(ui) = weak.upgrade() {
            handler.seek_source(&ui, ratio);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_view_changed(move || {
        if let Some(ui) = weak.upgrade() {
            handler.update_ruler(&ui);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_filter_changed(move || {
        if let Some(ui) = weak.upgrade() {
            handler.filter_cards(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_palette_filter(move || {
        if let Some(ui) = weak.upgrade() {
            filter_launcher(&ui);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_palette_run(move |id| {
        if let Some(ui) = weak.upgrade() {
            let eligible = launcher_actions(&ui)
                .iter()
                .any(|item| item.id == id && item.enabled);
            if !eligible {
                ui.set_feedback("This action needs a project or an unlocked clip selection".into());
                return;
            }
            ui.set_palette_open(false);
            if id.starts_with("timeline.")
                || id.starts_with("source.")
                || id.starts_with("playback.")
            {
                ui.set_active_workspace("EDIT".into());
            }
            if let Err(error) = handler.handle(id.as_str(), &ui) {
                ui.set_feedback(error.into());
            }
        }
    });
    ui.on_media_matches(|name, query| name.to_lowercase().contains(&query.to_lowercase()));
    let command_runtime = Rc::clone(runtime);
    ui.on_command_invoked(move |id| {
        if let Some(ui) = weak_ui.upgrade() {
            match command_runtime.handle(id.as_str(), &ui) {
                Err(error) => ui.set_feedback(error.into()),
                Ok(()) if id.starts_with("export.") => {
                    apply_export_status(&ui, command_runtime.controller.borrow().status().as_ref());
                }
                Ok(()) => {}
            }
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_media_selected(move |index| {
        if let Some(ui) = weak.upgrade() {
            handler.select_media(&ui, index);
            ui.set_inspector_section("Source".into());
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_clip_selected(move |index| {
        if let Some(ui) = weak.upgrade() {
            handler.select_clip(&ui, index);
            ui.set_inspector_section("Clip".into());
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_clip_adjusted(move |index, delta, edge| {
        if let Some(ui) = weak.upgrade()
            && let Err(error) = handler.drag_clip(&ui, index, delta, edge.as_str())
        {
            ui.set_feedback(error.into());
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_seek(move |ratio| {
        if let Some(ui) = weak.upgrade() {
            handler.seek_ratio(&ui, ratio);
        }
    });
    let weak = ui.as_weak();
    let handler = Rc::clone(runtime);
    ui.on_track_action(move |index, action| {
        if let Some(ui) = weak.upgrade()
            && let Err(error) = handler.change_track(&ui, index, action.as_str())
        {
            ui.set_feedback(error.into());
        }
    });
    refresh_shortcuts(ui);
}

/// Opens the main editor window and runs the native event loop.
///
/// # Errors
///
/// Returns a platform error if a window cannot be created or the native event
/// loop cannot run.
pub fn launch(project_path: Option<PathBuf>) -> Result<(), NativeUiError> {
    let ui = PrismStudio::new()?;
    update_media_runtime_status(&ui);
    let runtime_project_path = project_path.clone();
    let loaded_project = load_project_into_ui(&ui, project_path)?;
    let export_runtime = Rc::new(NativeExportRuntime::new(
        loaded_project,
        runtime_project_path,
    ));
    if export_runtime.project.borrow().is_some() {
        export_runtime.reset_view(&ui);
    }
    bind_editor_callbacks(&ui, &export_runtime);
    let poll_ui = ui.as_weak();
    let poll_runtime = Rc::clone(&export_runtime);
    let export_timer = slint::Timer::default();
    export_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            if let Some(ui) = poll_ui.upgrade() {
                poll_runtime.poll(&ui);
            }
        },
    );
    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BROWSER_THUMBNAIL_VARIANT, Command, MediaArtifactKind, MediaArtifactWorker,
        MediaCatalogEntry, MediaItemKind, NativeExportStatus, PROXY_AVAILABLE, PreviewWorker,
        ProcessCommand, ProxyGeneration, ProxySourceFingerprint, TrackId, TrackKind,
        WaveformRequest, WaveformWorker, fingerprint, insert_media_into_project,
        insert_video_into_project, next_proxy_version,
    };
    use prism_cache::CacheLayout;
    use prism_project::Project;
    use prism_studio::desktop::ExportController;
    use prism_timeline::RationalTime;
    use std::path::PathBuf;

    #[test]
    #[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
    fn real_video_frames_complete_under_repeated_playback_ticks()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("PRISM_TEST_FFMPEG").is_none() {
            return Ok(());
        }
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media/av-ntsc-48k.mp4");
        let worker = PreviewWorker::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut completed = 0;
        while std::time::Instant::now() < deadline {
            worker.request_playback(
                fixture.clone(),
                RationalTime::new(1, 2)?,
                RationalTime::from_integer(30),
            );
            if let Some(frame) = worker.poll_decoded() {
                let (frame, _, _, _) = frame?;
                assert!(frame.width > 0 && frame.height > 0);
                completed += 1;
                if completed == 2 {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(completed, 2, "Playback timer starved video decoding");
        Ok(())
    }

    #[test]
    #[ignore = "requires PRISM_TASK0_LONG_MEDIA and installed FFmpeg tools"]
    fn task0_measures_long_media_scrub_latency() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os("PRISM_TASK0_LONG_MEDIA").map(PathBuf::from) else {
            return Ok(());
        };
        let worker = PreviewWorker::new();
        let mut samples = Vec::new();
        for seconds in (0..600).step_by(60) {
            let started = std::time::Instant::now();
            worker.request(path.clone(), RationalTime::from_integer(seconds));
            let result = loop {
                if let Some(result) = worker.poll_decoded() {
                    break result;
                }
                if started.elapsed() > std::time::Duration::from_secs(5) {
                    return Err(format!("scrub at {seconds}s exceeded five seconds").into());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            };
            result?;
            samples.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        let maximum = samples.last().copied().ok_or("no scrub samples")?;
        eprintln!("TASK0_SCRUB_MS samples={samples:?} median={median:.3} max={maximum:.3}");
        Ok(())
    }

    #[test]
    fn playback_waits_for_decoder_but_scrubbing_supersedes_stale_frames() {
        use std::{
            cell::{Cell, RefCell},
            sync::mpsc,
        };
        let (sender, requests) = mpsc::sync_channel(1);
        let (replies, receiver) = mpsc::sync_channel(2);
        let worker = PreviewWorker {
            sender,
            receiver,
            pending: RefCell::new(None),
            generation: Cell::new(0),
            busy: Cell::new(false),
            requested_at: Cell::new(None),
            stream_control: super::StreamControl::default(),
        };
        let path = PathBuf::from("slow-video.mp4");
        worker.request_playback(
            path.clone(),
            RationalTime::ZERO,
            RationalTime::from_integer(30),
        );
        let first = requests.recv().unwrap();
        // Simulate a decode taking longer than multiple playback timer ticks.
        for tick in 1..10 {
            worker.request_playback(
                path.clone(),
                RationalTime::new(tick, 20).unwrap(),
                RationalTime::from_integer(30),
            );
            assert_eq!(worker.generation.get(), first.generation);
        }
        assert!(requests.try_recv().is_err());
        let frame = || {
            Ok((
                prism_media::DecodedVideoFrame {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 255],
                },
                RationalTime::new(10, 1).unwrap(),
                None,
                false,
            ))
        };
        worker.requested_at.set(Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(6))
                .unwrap(),
        ));
        assert!(worker.timed_out(std::time::Duration::from_secs(5)));
        replies.send((first.generation, frame())).unwrap();
        assert!(worker.poll_decoded().unwrap().is_ok());
        assert!(!worker.timed_out(std::time::Duration::ZERO));
        assert!(!worker.busy.get());
        worker.request_playback(
            path.clone(),
            RationalTime::new(1, 1).unwrap(),
            RationalTime::from_integer(30),
        );
        let second = requests.recv().unwrap();
        worker.request(path, RationalTime::new(2, 1).unwrap());
        let scrub = requests.recv().unwrap();
        assert_ne!(second.generation, scrub.generation);
        replies.send((second.generation, frame())).unwrap();
        assert!(worker.poll_decoded().is_none());
        assert!(worker.busy.get());
        replies.send((scrub.generation, frame())).unwrap();
        assert!(worker.poll_decoded().unwrap().is_ok());
        assert!(!worker.busy.get());
    }

    #[test]
    fn source_insert_splits_shifts_and_preserves_source_offsets()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Insert edit");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("insert.mp4"),
            RationalTime::new(4, 1)?,
            true,
        )?;
        let media = *project.media.keys().next().unwrap();
        let append = super::append_source_commands(
            &project,
            media,
            RationalTime::ZERO,
            RationalTime::new(4, 1)?,
        )?;
        prism_commands::CommandEngine::execute_transaction(&mut project, "Append", append)?;
        let commands = super::source_edit_commands(
            &project,
            media,
            RationalTime::new(1, 2)?,
            RationalTime::new(1, 1)?,
            RationalTime::new(1, 1)?,
            false,
        )?;
        let result =
            prism_commands::CommandEngine::execute_transaction(&mut project, "Insert", commands)?;
        for track in &project.sequences.values().next().unwrap().tracks {
            assert_eq!(track.clips.len(), 4);
            let ranges = track
                .clips
                .iter()
                .map(|c| (c.timeline_start, c.source_start, c.source_duration))
                .collect::<Vec<_>>();
            assert_eq!(
                ranges,
                vec![
                    (
                        RationalTime::ZERO,
                        RationalTime::ZERO,
                        RationalTime::new(1, 1)?
                    ),
                    (
                        RationalTime::new(1, 1)?,
                        RationalTime::new(1, 2)?,
                        RationalTime::new(1, 1)?
                    ),
                    (
                        RationalTime::new(2, 1)?,
                        RationalTime::new(1, 1)?,
                        RationalTime::new(3, 1)?
                    ),
                    (
                        RationalTime::new(5, 1)?,
                        RationalTime::ZERO,
                        RationalTime::new(4, 1)?
                    )
                ]
            );
        }
        result.undo.apply(&mut project)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|t| t.clips.len() == 2)
        );
        project.sequences.values_mut().next().unwrap().tracks[1].locked = true;
        assert!(
            super::source_edit_commands(
                &project,
                media,
                RationalTime::ZERO,
                RationalTime::new(1, 1)?,
                RationalTime::new(1, 1)?,
                false
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn source_overwrite_preserves_tails_and_later_timing() -> Result<(), Box<dyn std::error::Error>>
    {
        for (position, duration, expected) in [
            (1, 2, vec![(0, 0, 1), (1, 0, 2), (3, 3, 1), (4, 0, 4)]),
            (3, 2, vec![(0, 0, 3), (3, 0, 2), (5, 1, 3)]),
            (0, 8, vec![(0, 0, 8)]),
            (10, 1, vec![(0, 0, 4), (4, 0, 4), (10, 0, 1)]),
        ] {
            let mut project = Project::new("Overwrite edit");
            insert_media_into_project(
                &mut project,
                std::path::Path::new("overwrite.mp4"),
                RationalTime::new(4, 1)?,
                true,
            )?;
            let media = *project.media.keys().next().unwrap();
            let append = super::append_source_commands(
                &project,
                media,
                RationalTime::ZERO,
                RationalTime::new(4, 1)?,
            )?;
            prism_commands::CommandEngine::execute_transaction(&mut project, "Append", append)?;
            let commands = super::source_edit_commands(
                &project,
                media,
                RationalTime::ZERO,
                RationalTime::new(duration, 1)?,
                RationalTime::new(position, 1)?,
                true,
            )?;
            let result = prism_commands::CommandEngine::execute_transaction(
                &mut project,
                "Overwrite",
                commands,
            )?;
            for track in &project.sequences.values().next().unwrap().tracks {
                let ranges = track
                    .clips
                    .iter()
                    .map(|c| (c.timeline_start, c.source_start, c.source_duration))
                    .collect::<Vec<_>>();
                assert_eq!(
                    ranges,
                    expected
                        .iter()
                        .map(|&(a, b, c)| (
                            RationalTime::new(a, 1).unwrap(),
                            RationalTime::new(b, 1).unwrap(),
                            RationalTime::new(c, 1).unwrap()
                        ))
                        .collect::<Vec<_>>()
                );
            }
            result.undo.apply(&mut project)?;
            assert!(
                project
                    .sequences
                    .values()
                    .next()
                    .unwrap()
                    .tracks
                    .iter()
                    .all(|t| t.clips.len() == 2)
            );
        }
        Ok(())
    }

    #[test]
    fn all_track_cuts_skip_locks_and_undo_atomically() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("All track cut");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("all-tracks.mp4"),
            RationalTime::new(2, 1)?,
            true,
        )?;
        let cut = RationalTime::new(1, 1)?;
        assert!(super::split_all_commands(&project, RationalTime::ZERO).is_err());
        let commands = super::split_all_commands(&project, cut)?;
        assert_eq!(commands.len(), 2);
        let original = project.clone();
        let result = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "All track cut",
            commands,
        )?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|track| track.clips.len() == 2)
        );
        result.undo.apply(&mut project)?;
        let clips = |project: &Project| {
            project
                .sequences
                .values()
                .flat_map(|sequence| &sequence.tracks)
                .flat_map(|track| {
                    track.clips.iter().map(move |clip| {
                        (
                            track.id,
                            clip.id,
                            clip.media_id,
                            clip.timeline_start,
                            clip.source_start,
                            clip.source_duration,
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(clips(&project), clips(&original));
        let sequence = project.sequences.values_mut().next().unwrap();
        sequence.tracks[0].locked = true;
        sequence.tracks[1].muted = true;
        assert_eq!(super::split_all_commands(&project, cut)?.len(), 1);
        project.sequences.values_mut().next().unwrap().tracks[1].locked = true;
        assert!(super::split_all_commands(&project, cut).is_err());
        Ok(())
    }

    #[test]
    fn program_match_uses_visible_picture_and_rejects_gaps()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Match Program");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("match-program.mp4"),
            RationalTime::new(2, 1)?,
            true,
        )?;
        assert_eq!(
            super::program_clip_index(&project, RationalTime::new(1, 1)?),
            Some(0)
        );
        assert_eq!(
            super::program_clip_index(&project, RationalTime::new(2, 1)?),
            None
        );
        project.sequences.values_mut().next().unwrap().tracks[0].visible = false;
        assert_eq!(
            super::program_clip_index(&project, RationalTime::ZERO),
            None
        );
        Ok(())
    }

    #[test]
    fn aligned_av_ranges_validate_bounds_undo_and_overlap() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut project = Project::new("Paired range");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("source.mp4"),
            RationalTime::new(2, 1)?,
            true,
        )?;
        let range = |project: &Project, start, duration, available| {
            let timeline_start = project
                .sequences
                .values()
                .next()
                .and_then(|sequence| sequence.tracks.first()?.clips.first())
                .map_or(RationalTime::ZERO, |clip| clip.timeline_start);
            super::aligned_av_range_commands(project, 0, timeline_start, start, duration, available)
        };
        let commands = range(
            &project,
            RationalTime::new(1, 4)?,
            RationalTime::new(3, 2)?,
            RationalTime::new(2, 1)?,
        )?;
        let outcome = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Paired range",
            commands,
        )?;
        for track in &project.sequences.values().next().unwrap().tracks {
            assert_eq!(track.clips[0].timeline_start, RationalTime::ZERO);
            assert_eq!(track.clips[0].source_start, RationalTime::new(1, 4)?);
            assert_eq!(track.clips[0].source_duration, RationalTime::new(3, 2)?);
        }
        outcome.undo.apply(&mut project)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|track| track.clips[0].source_start == RationalTime::ZERO
                    && track.clips[0].source_duration == RationalTime::new(2, 1).unwrap())
        );
        assert!(
            range(
                &project,
                RationalTime::new(1, 1)?,
                RationalTime::new(2, 1)?,
                RationalTime::new(2, 1)?
            )
            .is_err()
        );
        assert!(
            range(
                &project,
                RationalTime::ZERO,
                RationalTime::ZERO,
                RationalTime::new(2, 1)?
            )
            .is_err()
        );
        let sequence = project.sequences.values_mut().next().unwrap();
        let mut blocker = sequence.tracks[1].clips[0].clone();
        blocker.id = prism_core::ClipId::new();
        blocker.timeline_start = RationalTime::new(3, 1)?;
        sequence.tracks[1].clips.push(blocker);
        let revision = project.revision();
        let commands = range(
            &project,
            RationalTime::ZERO,
            RationalTime::new(4, 1)?,
            RationalTime::new(10, 1)?,
        )?;
        assert!(
            prism_commands::CommandEngine::execute_transaction(
                &mut project,
                "Blocked range",
                commands
            )
            .is_err()
        );
        assert_eq!(project.revision(), revision);
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|track| track.clips[0].source_duration == RationalTime::new(2, 1).unwrap())
        );
        Ok(())
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn aligned_av_edits_are_atomic_undoable_and_reject_unsafe_matches()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Paired edits");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("camera.mp4"),
            RationalTime::new(2, 1)?,
            true,
        )?;
        assert_eq!(super::aligned_av_companion_index(&project, 0), Some(1));
        assert_eq!(super::aligned_av_companion_index(&project, 1), Some(0));
        let commands =
            super::aligned_av_commands(&project, 0, "timeline.split_av", RationalTime::new(1, 1)?)?;
        assert_eq!(commands.len(), 2);
        let split = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Split aligned A/V",
            commands,
        )?;
        for track in &project.sequences.values().next().unwrap().tracks {
            assert_eq!(track.clips.len(), 2);
            assert_eq!(track.clips[1].timeline_start, RationalTime::new(1, 1)?);
            assert_eq!(track.clips[1].source_start, RationalTime::new(1, 1)?);
        }
        split.undo.apply(&mut project)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|track| track.clips.len() == 1)
        );
        let revision = project.revision();
        let invalid =
            super::aligned_av_commands(&project, 0, "timeline.split_av", RationalTime::ZERO)?;
        assert!(
            prism_commands::CommandEngine::execute_transaction(
                &mut project,
                "Invalid pair cut",
                invalid
            )
            .is_err()
        );
        assert_eq!(project.revision(), revision);
        let commands =
            super::aligned_av_commands(&project, 1, "timeline.delete_av", RationalTime::ZERO)?;
        let deleted = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Delete aligned A/V",
            commands,
        )?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|track| track.clips.is_empty())
        );
        deleted.undo.apply(&mut project)?;
        let commands =
            super::aligned_av_commands(&project, 0, "timeline.move_av", RationalTime::new(3, 1)?)?;
        let moved = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Move aligned A/V",
            commands,
        )?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(
                    |track| track.clips[0].timeline_start == RationalTime::new(3, 1).unwrap()
                        && track.clips[0].source_start == RationalTime::ZERO
                )
        );
        moved.undo.apply(&mut project)?;
        let sequence = project.sequences.values_mut().next().unwrap();
        let mut blocker = sequence.tracks[1].clips[0].clone();
        blocker.id = prism_core::ClipId::new();
        blocker.timeline_start = RationalTime::new(3, 1)?;
        sequence.tracks[1].clips.push(blocker);
        let revision = project.revision();
        let commands =
            super::aligned_av_commands(&project, 0, "timeline.move_av", RationalTime::new(3, 1)?)?;
        assert!(
            prism_commands::CommandEngine::execute_transaction(
                &mut project,
                "Blocked pair move",
                commands
            )
            .is_err()
        );
        assert_eq!(project.revision(), revision);
        assert!(
            project
                .sequences
                .values()
                .next()
                .unwrap()
                .tracks
                .iter()
                .all(|track| track.clips[0].timeline_start == RationalTime::ZERO)
        );
        let sequence = project.sequences.values_mut().next().unwrap();
        sequence.tracks[1].clips.pop();
        sequence.tracks[1].locked = true;
        assert!(
            super::aligned_av_commands(&project, 0, "timeline.delete_av", RationalTime::ZERO)
                .unwrap_err()
                .contains("Unlock both")
        );
        let sequence = project.sequences.values_mut().next().unwrap();
        sequence.tracks[1].locked = false;
        let mut extra = sequence.tracks[1].clone();
        extra.id = TrackId::new();
        extra.clips[0].id = prism_core::ClipId::new();
        sequence.tracks.push(extra);
        assert!(
            super::aligned_av_commands(&project, 0, "timeline.delete_av", RationalTime::ZERO)
                .unwrap_err()
                .contains("Multiple")
        );
        let sequence = project.sequences.values_mut().next().unwrap();
        sequence.tracks.pop();
        sequence.tracks[1].clips[0].source_start = RationalTime::new(1, 30)?;
        assert!(
            super::aligned_av_commands(&project, 0, "timeline.delete_av", RationalTime::ZERO)
                .unwrap_err()
                .contains("No audio/video")
        );
        Ok(())
    }

    #[test]
    fn aligned_av_ripple_delete_closes_both_tracks_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Paired ripple delete");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("camera.mp4"),
            RationalTime::new(2, 1)?,
            true,
        )?;
        let split_commands =
            super::aligned_av_commands(&project, 0, "timeline.split_av", RationalTime::new(1, 1)?)?;
        prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Split aligned A/V",
            split_commands,
        )?;

        let ripple_commands = super::aligned_av_commands(
            &project,
            0,
            "timeline.ripple_delete_av",
            RationalTime::ZERO,
        )?;
        assert_eq!(ripple_commands.len(), 2);
        let deleted = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Ripple delete aligned A/V",
            ripple_commands,
        )?;
        for track in &project.sequences.values().next().unwrap().tracks {
            assert_eq!(track.clips.len(), 1);
            assert_eq!(track.clips[0].timeline_start, RationalTime::ZERO);
            assert_eq!(track.clips[0].source_start, RationalTime::new(1, 1)?);
        }

        deleted.undo.apply(&mut project)?;
        for track in &project.sequences.values().next().unwrap().tracks {
            assert_eq!(track.clips.len(), 2);
            assert_eq!(track.clips[0].timeline_start, RationalTime::ZERO);
            assert_eq!(track.clips[1].timeline_start, RationalTime::new(1, 1)?);
        }
        Ok(())
    }

    #[test]
    fn audio_preview_respects_source_trims_cuts_gaps_and_mute()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Audio edit");
        let path = std::path::Path::new("take.mp4");
        insert_media_into_project(&mut project, path, RationalTime::new(1, 1)?, true)?;
        insert_media_into_project(
            &mut project,
            std::path::Path::new("next-take.mp4"),
            RationalTime::new(1, 1)?,
            true,
        )?;
        let audio = project
            .sequences
            .values_mut()
            .next()
            .unwrap()
            .tracks
            .iter_mut()
            .find(|track| track.kind == TrackKind::Audio)
            .unwrap();
        audio.clips[1].timeline_start = RationalTime::new(3, 1)?;
        audio.clips[1].source_start = RationalTime::new(1, 4)?;
        let second_id = audio.clips[1].id;
        let first = super::audio_preview_segment(&project, RationalTime::new(1, 2)?).unwrap();
        assert_eq!(first.source_time, RationalTime::new(1, 2)?);
        assert_eq!(first.remaining, RationalTime::new(1, 2)?);
        assert!(super::audio_preview_segment(&project, RationalTime::new(1, 1)?).is_none());
        assert!(super::audio_preview_segment(&project, RationalTime::new(2, 1)?).is_none());
        let second = super::audio_preview_segment(&project, RationalTime::new(7, 2)?).unwrap();
        assert_eq!(second.clip_id, second_id);
        assert_eq!(second.source_time, RationalTime::new(3, 4)?);
        assert_eq!(second.remaining, RationalTime::new(1, 2)?);
        assert!(super::audio_preview_segment(&project, RationalTime::new(4, 1)?).is_none());
        project
            .sequences
            .values_mut()
            .next()
            .unwrap()
            .tracks
            .iter_mut()
            .find(|track| track.kind == TrackKind::Audio)
            .unwrap()
            .muted = true;
        assert!(super::audio_preview_segment(&project, RationalTime::new(7, 2)?).is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dropping_runtime_terminates_audio_process() -> Result<(), Box<dyn std::error::Error>> {
        let runtime = super::NativeExportRuntime::new(None, None);
        let child = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
        let pid = child.id();
        *runtime.audio_process.borrow_mut() = Some(child);
        let decoder = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
        let decoder_pid = decoder.id();
        *runtime.audio_decoder.borrow_mut() = Some(decoder);
        drop(runtime);
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", &decoder_pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()?
                .success()
        );
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()?
                .success(),
            "dropping Studio left its audio process running"
        );
        Ok(())
    }

    #[test]
    fn matched_frame_maps_timeline_time_to_trimmed_source() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut project = Project::new("Match frame");
        insert_video_into_project(
            &mut project,
            std::path::Path::new("trimmed.mp4"),
            RationalTime::new(2, 1)?,
        )?;
        let clip = &mut project
            .sequences
            .values_mut()
            .next()
            .ok_or("Missing sequence")?
            .tracks[0]
            .clips[0];
        clip.timeline_start = RationalTime::new(3, 1)?;
        clip.source_start = RationalTime::new(7, 2)?;
        assert_eq!(
            super::matched_source(&project, 0, RationalTime::new(4, 1)?),
            Some((0, RationalTime::new(9, 2)?))
        );
        // Clip end is exclusive, as in program-frame decoding.
        assert_eq!(
            super::matched_source(&project, 0, RationalTime::new(5, 1)?),
            Some((0, RationalTime::new(7, 2)?))
        );
        assert_eq!(
            super::matched_source(&project, 0, RationalTime::ZERO),
            Some((0, RationalTime::new(7, 2)?))
        );
        assert_eq!(
            super::matched_source(&project, -1, RationalTime::ZERO),
            None
        );
        assert_eq!(super::matched_source(&project, 5, RationalTime::ZERO), None);
        Ok(())
    }

    #[test]
    fn matched_audio_source_preserves_time_without_video_classification()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Match audio");
        super::insert_stream_media_into_project(
            &mut project,
            std::path::Path::new("dialogue.wav"),
            RationalTime::new(3, 1)?,
            false,
            true,
            None,
        )?;
        let media_id = *project.media.keys().next().ok_or("missing media")?;
        assert_eq!(
            super::media_stream_summary(&project, media_id),
            (false, true, Some(RationalTime::new(3, 1)?))
        );
        assert_eq!(
            super::matched_source(&project, 0, RationalTime::new(3, 2)?),
            Some((0, RationalTime::new(3, 2)?))
        );
        Ok(())
    }

    #[test]
    fn edit_navigation_handles_gaps_edges_and_hidden_tracks()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Navigation");
        insert_video_into_project(
            &mut project,
            std::path::Path::new("first.mp4"),
            RationalTime::new(1, 1)?,
        )?;
        insert_video_into_project(
            &mut project,
            std::path::Path::new("second.mp4"),
            RationalTime::new(1, 1)?,
        )?;
        let sequence = project
            .sequences
            .values_mut()
            .next()
            .ok_or("Missing sequence")?;
        sequence.tracks[0].clips[1].timeline_start = RationalTime::new(3, 1)?;
        let time = |seconds| RationalTime::new(seconds, 1).unwrap();
        let target = |at, command| super::navigation_target(&project, time(at), command);
        assert_eq!(target(0, "playback.next_edit"), Some(time(1)));
        assert_eq!(target(1, "playback.next_edit"), Some(time(3)));
        assert_eq!(target(2, "playback.previous_edit"), Some(time(1)));
        assert_eq!(target(3, "playback.previous_edit"), Some(time(1)));
        assert_eq!(target(4, "playback.next_edit"), Some(time(4)));
        assert_eq!(target(0, "playback.previous_edit"), Some(time(0)));
        assert_eq!(target(2, "playback.start"), Some(time(0)));
        assert_eq!(target(2, "playback.end"), Some(time(4)));
        project.sequences.values_mut().next().unwrap().tracks[0].visible = false;
        assert_eq!(
            super::navigation_target(&project, time(0), "playback.next_edit"),
            Some(time(4))
        );
        assert_eq!(
            super::navigation_target(&Project::new("Empty"), time(0), "playback.next_edit"),
            None
        );
        Ok(())
    }

    #[test]
    fn export_status_maps_to_graphical_task_properties() -> Result<(), Box<dyn std::error::Error>> {
        let mut controller = ExportController::new();
        controller.queue(PathBuf::from("deliver.mp4"));
        controller.start()?;
        controller.report_progress(42)?;
        let running = controller
            .status()
            .ok_or_else(|| std::io::Error::other("missing export status"))?;
        let running = NativeExportStatus::from(&running);
        assert_eq!(running.label, "EXPORTING");
        assert_eq!(running.output, "deliver.mp4");
        assert_eq!(running.progress, 42);
        assert!(running.can_cancel);
        assert!(!running.can_retry);

        controller.fail("encoder unavailable")?;
        let failed = controller
            .status()
            .ok_or_else(|| std::io::Error::other("missing export status"))?;
        let failed = NativeExportStatus::from(&failed);
        assert_eq!(failed.label, "FAILED");
        assert_eq!(failed.error, "encoder unavailable");
        assert!(failed.can_retry);
        assert!(failed.can_remove);
        Ok(())
    }

    #[test]
    fn imported_videos_create_and_extend_a_timeline() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Edit");
        insert_video_into_project(
            &mut project,
            std::path::Path::new("first.mp4"),
            RationalTime::new(5, 1)?,
        )?;
        insert_video_into_project(
            &mut project,
            std::path::Path::new("second.mp4"),
            RationalTime::new(3, 1)?,
        )?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or_else(|| std::io::Error::other("missing sequence"))?;
        let track = sequence
            .tracks
            .first()
            .ok_or_else(|| std::io::Error::other("missing track"))?;
        assert_eq!(project.media.len(), 2);
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[1].timeline_start, RationalTime::new(5, 1)?);
        Ok(())
    }

    #[test]
    fn imported_av_media_creates_synchronized_video_and_audio_tracks()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Edit");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("interview.mp4"),
            RationalTime::new(5, 1)?,
            true,
        )?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        let video = sequence
            .tracks
            .iter()
            .find(|track| track.kind == prism_project::TrackKind::Video)
            .ok_or("missing video track")?;
        let audio = sequence
            .tracks
            .iter()
            .find(|track| track.kind == prism_project::TrackKind::Audio)
            .ok_or("missing audio track")?;
        assert_eq!(video.clips.len(), 1);
        assert_eq!(audio.clips.len(), 1);
        assert_eq!(video.clips[0].timeline_start, audio.clips[0].timeline_start);
        assert_eq!(
            video.clips[0].source_duration,
            audio.clips[0].source_duration
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires PRISM_FFPROBE and the generated media fixture"]
    fn real_media_import_probes_and_creates_editable_synchronized_tracks()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("PRISM_FFPROBE").is_none() {
            return Ok(());
        }
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media/av-ntsc-48k.mp4");
        let mut project = Project::new("Real import");
        super::import_media_into_project(&mut project, &fixture)?;
        project.validate()?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        let video = sequence
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .ok_or("missing video track")?;
        let audio = sequence
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .ok_or("missing audio track")?;
        assert_eq!(project.media.len(), 1);
        assert_eq!(video.clips.len(), 1);
        assert_eq!(audio.clips.len(), 1);
        assert_eq!(video.clips[0].timeline_start, RationalTime::ZERO);
        assert_eq!(
            video.clips[0].source_duration,
            audio.clips[0].source_duration
        );
        assert!(video.clips[0].source_duration.is_positive());
        Ok(())
    }

    #[test]
    #[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
    fn real_audio_only_import_reopens_and_generates_waveform()
    -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("PRISM_FFMPEG").is_none() || std::env::var_os("PRISM_FFPROBE").is_none()
        {
            return Ok(());
        }
        let root = std::env::temp_dir().join(format!(
            "prism-studio-real-audio-import-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media/av-ntsc-48k.mp4");
        let audio = root.join("narration.wav");
        let output = ProcessCommand::new(super::media_tool("ffmpeg"))
            .args(["-v", "error", "-i"])
            .arg(&fixture)
            .args(["-t", "1", "-vn", "-c:a", "pcm_s16le", "-y"])
            .arg(&audio)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }

        let mut project = Project::new("Audio import");
        super::import_media_into_project(&mut project, &audio)?;
        let project_path = root.join("audio.prism");
        prism_persistence::save_atomic(&project, &project_path)?;
        let reopened = prism_persistence::load(&project_path)?;
        let sequence = reopened
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        assert_eq!(sequence.tracks.len(), 1);
        assert_eq!(sequence.tracks[0].kind, TrackKind::Audio);

        let worker = WaveformWorker::spawn(super::media_tool("ffmpeg"), 1);
        worker.try_submit(WaveformRequest {
            path: audio,
            cache_root: root.join("cache"),
            sample_rate: 48_000,
            max_samples: 48_000,
            samples_per_point: 1_024,
        })?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let waveform = loop {
            match worker.try_result() {
                Ok(result) => break result?,
                Err(std::sync::mpsc::TryRecvError::Empty)
                    if std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => return Err(error.into()),
            }
        };
        assert!(!waveform.points.is_empty());
        assert!(
            waveform
                .points
                .iter()
                .any(|point| point.max > 0.0 || point.min < 0.0)
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn audio_only_import_creates_only_an_audio_track() -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Music edit");
        let imported = super::insert_stream_media_into_project(
            &mut project,
            std::path::Path::new("score.wav"),
            RationalTime::new(3, 1)?,
            false,
            true,
            None,
        )?;
        project.validate()?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        assert_eq!(sequence.tracks.len(), 1);
        assert_eq!(sequence.tracks[0].kind, TrackKind::Audio);
        assert_eq!(sequence.tracks[0].clips.len(), 1);
        assert_eq!(
            sequence.tracks[0].clips[0].source_duration,
            RationalTime::new(3, 1)?
        );
        imported.apply(&mut project)?;
        assert!(project.media.is_empty());
        assert!(project.sequences.is_empty());
        Ok(())
    }

    #[test]
    fn source_append_keeps_av_streams_aligned_and_undoes_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Source append");
        super::insert_stream_media_into_project(
            &mut project,
            std::path::Path::new("interview.mp4"),
            RationalTime::new(3, 1)?,
            true,
            true,
            None,
        )?;
        let media_id = *project.media.keys().next().ok_or("missing media")?;
        let commands = super::append_source_commands(
            &project,
            media_id,
            RationalTime::new(1, 2)?,
            RationalTime::new(1, 1)?,
        )?;
        assert_eq!(commands.len(), 2);
        let outcome = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Append source range",
            commands,
        )?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        for track in &sequence.tracks {
            assert_eq!(track.clips.len(), 2);
            assert_eq!(track.clips[1].timeline_start, RationalTime::new(3, 1)?);
            assert_eq!(track.clips[1].source_start, RationalTime::new(1, 2)?);
            assert_eq!(track.clips[1].source_duration, RationalTime::new(1, 1)?);
        }
        outcome.undo.apply(&mut project)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .ok_or("missing sequence")?
                .tracks
                .iter()
                .all(|track| track.clips.len() == 1)
        );
        Ok(())
    }

    #[test]
    fn duplicate_selected_av_pair_uses_one_aligned_transaction()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Duplicate pair");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("take.mp4"),
            RationalTime::new(2, 1)?,
            true,
        )?;
        let commands = super::duplicate_selected_commands(&project, 0)?;
        assert_eq!(commands.len(), 2);
        let outcome = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Duplicate aligned A/V",
            commands,
        )?;
        for track in &project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?
            .tracks
        {
            assert_eq!(track.clips.len(), 2);
            assert_eq!(track.clips[1].timeline_start, RationalTime::new(2, 1)?);
        }
        outcome.undo.apply(&mut project)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .ok_or("missing sequence")?
                .tracks
                .iter()
                .all(|track| track.clips.len() == 1)
        );
        Ok(())
    }

    #[test]
    fn slip_selected_av_pair_changes_both_sources_and_undoes_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Slip pair");
        insert_media_into_project(
            &mut project,
            std::path::Path::new("take.mp4"),
            RationalTime::new(2, 1)?,
            true,
        )?;
        let commands = super::slip_selected_commands(&project, 0, RationalTime::new(1, 2)?)?;
        assert_eq!(commands.len(), 2);
        let outcome = prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Slip aligned A/V",
            commands,
        )?;
        let slipped = RationalTime::new(1, 2)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .ok_or("missing sequence")?
                .tracks
                .iter()
                .all(|track| track.clips[0].source_start == slipped)
        );
        outcome.undo.apply(&mut project)?;
        assert!(
            project
                .sequences
                .values()
                .next()
                .ok_or("missing sequence")?
                .tracks
                .iter()
                .all(|track| track.clips[0].source_start == RationalTime::ZERO)
        );
        Ok(())
    }

    #[test]
    fn audio_source_append_targets_only_audio_at_sequence_end()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Audio append");
        super::insert_stream_media_into_project(
            &mut project,
            std::path::Path::new("score.wav"),
            RationalTime::new(3, 1)?,
            false,
            true,
            None,
        )?;
        let media_id = *project.media.keys().next().ok_or("missing media")?;
        let commands = super::append_source_commands(
            &project,
            media_id,
            RationalTime::new(1, 1)?,
            RationalTime::new(1, 2)?,
        )?;
        assert_eq!(commands.len(), 1);
        prism_commands::CommandEngine::execute_transaction(
            &mut project,
            "Append audio source range",
            commands,
        )?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        assert_eq!(sequence.tracks.len(), 1);
        assert_eq!(sequence.tracks[0].kind, TrackKind::Audio);
        assert_eq!(sequence.tracks[0].clips.len(), 2);
        assert_eq!(
            sequence.tracks[0].clips[1].timeline_start,
            RationalTime::new(3, 1)?
        );
        assert_eq!(
            sequence.tracks[0].clips[1].source_start,
            RationalTime::new(1, 1)?
        );
        Ok(())
    }

    #[test]
    fn mixed_media_imports_append_after_the_complete_sequence()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = Project::new("Mixed edit");
        super::insert_stream_media_into_project(
            &mut project,
            std::path::Path::new("score.wav"),
            RationalTime::new(3, 1)?,
            false,
            true,
            None,
        )?;
        super::insert_stream_media_into_project(
            &mut project,
            std::path::Path::new("scene.mp4"),
            RationalTime::new(2, 1)?,
            true,
            true,
            None,
        )?;
        project.validate()?;
        let sequence = project
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?;
        let video = sequence
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .ok_or("missing video track")?;
        let audio = sequence
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .ok_or("missing audio track")?;
        assert_eq!(video.clips[0].timeline_start, RationalTime::new(3, 1)?);
        assert_eq!(audio.clips[1].timeline_start, RationalTime::new(3, 1)?);
        assert_eq!(sequence.duration()?, RationalTime::new(5, 1)?);
        Ok(())
    }

    #[test]
    #[allow(clippy::float_cmp, clippy::too_many_lines)]
    fn native_workspace_renders_and_edits_are_persisted() -> Result<(), Box<dyn std::error::Error>>
    {
        use super::{
            NativeExportRuntime, PrismStudio, apply_project_summary, bind_editor_callbacks,
            display_timecode,
        };
        use slint::platform::{
            Platform, WindowAdapter,
            software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        };
        use slint::{ComponentHandle, Model};
        use std::rc::Rc;

        struct TestPlatform(Rc<MinimalSoftwareWindow>);
        impl Platform for TestPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::default());
        slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
        let ui = PrismStudio::new()?;
        ui.on_media_matches(|name, query| name.to_lowercase().contains(&query.to_lowercase()));
        window.set_size(slint::PhysicalSize::new(1440, 900));
        ui.show()?;
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/media/av-ntsc-48k.mp4");
        let mut project = Project::new("A different perspective");
        insert_video_into_project(&mut project, &fixture, RationalTime::new(2, 1)?)?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "prism-ui-test-{}-{nonce}.prism",
            std::process::id()
        ));
        prism_persistence::save_atomic(&project, &path)?;
        let autosave_path = prism_persistence::save_autosave(&project, &path)?;
        let autosave_revision = project.revision();
        apply_project_summary(&ui, &project, &path);
        let runtime = Rc::new(NativeExportRuntime::new(Some(project), Some(path.clone())));
        bind_editor_callbacks(&ui, &runtime);
        assert!(ui.invoke_edit_shortcut("n".into(), false, false, false));
        assert_eq!(ui.get_active_tool(), "R");
        ui.set_active_tool("V".into());
        assert!(ui.invoke_edit_shortcut("r".into(), false, false, false));
        assert_eq!(ui.get_active_tool(), "V");
        assert!(ui.get_feedback().contains("not built"));
        ui.set_keyboard_preset("Prism".into());
        assert!(ui.invoke_edit_shortcut("r".into(), false, false, false));
        assert_eq!(ui.get_active_tool(), "R");
        ui.set_keyboard_preset("Premiere".into());
        ui.set_active_tool("V".into());
        assert!(ui.invoke_edit_shortcut("=".into(), false, false, false));
        assert_eq!(ui.get_timeline_zoom(), 2.0);
        assert!(ui.invoke_edit_shortcut("\\".into(), false, false, false));
        assert_eq!(ui.get_timeline_zoom(), 1.0);

        runtime.reset_view(&ui);
        assert_eq!(ui.get_media_task_status(), "DECODING");
        assert_eq!(ui.get_media_task_pending(), 1);
        assert_eq!(ui.get_recovery_count(), 1);
        assert_eq!(ui.get_recovery_path(), autosave_path.display().to_string());
        assert_eq!(ui.get_recovery_revision(), autosave_revision.to_string());
        // Exercise actual keyboard dispatch: open, type, navigate, run, restore focus.
        {
            use slint::platform::{Key, WindowEvent};
            let press = |text: slint::SharedString| {
                window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                window.dispatch_event(WindowEvent::KeyReleased { text });
            };
            window.dispatch_event(WindowEvent::KeyPressed {
                text: Key::Control.into(),
            });
            window.dispatch_event(WindowEvent::KeyPressed {
                text: Key::Shift.into(),
            });
            press("p".into());
            window.dispatch_event(WindowEvent::KeyReleased {
                text: Key::Shift.into(),
            });
            window.dispatch_event(WindowEvent::KeyReleased {
                text: Key::Control.into(),
            });
            assert!(ui.get_palette_open());
            for character in ["m", "a", "r", "k", "e", "r"] {
                press(character.into());
            }
            assert_eq!(ui.get_palette_query(), "marker");
            press(Key::DownArrow.into());
            assert_eq!(ui.get_palette_index(), 1);
            press(Key::Tab.into());
            press(Key::Return.into());
            assert!(!ui.get_palette_open());
            assert_eq!(ui.get_browser_tab(), "Markers");
            press("s".into());
            assert!(!ui.get_snap_enabled());
            press("s".into());
        }
        assert!(ui.get_snap_enabled());
        runtime.handle("timeline.toggle_snap", &ui)?;
        assert!(!ui.get_snap_enabled());
        runtime.handle("timeline.toggle_snap", &ui)?;
        runtime.add_marker(&ui)?;
        let marker_revision = runtime
            .project
            .borrow()
            .as_ref()
            .map(Project::revision)
            .ok_or("missing project")?;
        let autosave_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while ui.get_autosave_status() != format!("SAVED R{marker_revision}")
            && std::time::Instant::now() < autosave_deadline
        {
            runtime.poll(&ui);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            ui.get_autosave_status(),
            format!("SAVED R{marker_revision}")
        );
        assert_eq!(
            runtime
                .project
                .borrow()
                .as_ref()
                .and_then(|project| project.sequences.values().next())
                .map(|sequence| sequence.markers.len()),
            Some(1)
        );
        assert_eq!(ui.get_timeline_markers().row_count(), 1);
        ui.set_selected_marker_index(0);
        runtime.delete_marker(&ui)?;
        assert_eq!(ui.get_timeline_markers().row_count(), 0);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_markers().row_count(), 1);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_markers().row_count(), 0);
        assert_eq!(ui.get_tracks().row_count(), 1);
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        assert_eq!(ui.get_duration_label(), "00:00:02:00");
        assert!(ui.invoke_media_matches("Interview.MP4".into(), "interview".into()));
        assert!(!ui.invoke_media_matches("Interview.MP4".into(), "music".into()));

        ui.set_selected_clip_index(0);
        runtime.seek_time(&ui, RationalTime::new(1, 1)?);
        ui.invoke_track_action(0, "lock".into());
        assert!(
            ui.get_tracks()
                .row_data(0)
                .is_some_and(|track| track.locked)
        );
        assert!(runtime.edit_clip("timeline.split", &ui).is_err());
        assert_eq!(
            prism_persistence::load(&path)?
                .sequences
                .values()
                .next()
                .map(|s| s.tracks[0].clips.len()),
            Some(1)
        );
        ui.invoke_track_action(0, "lock".into());
        ui.invoke_command_invoked("timeline.split".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        let saved = prism_persistence::load(&path)?;
        let sequence = saved.sequences.values().next().ok_or("missing sequence")?;
        assert_eq!(
            sequence.tracks[0].clips[1].source_start,
            RationalTime::new(1, 1)?
        );
        assert_eq!(
            sequence.tracks[0].clips[1].timeline_start,
            RationalTime::new(1, 1)?
        );
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        assert!(ui.get_selected_has_next());
        runtime.toggle_dissolve(&ui)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 1);
        assert!(ui.get_selected_has_transition());
        assert!(ui.get_can_export());
        assert!(ui.get_export_preflight().contains("Ready"));
        runtime.toggle_dissolve(&ui)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 0);
        assert!(ui.get_can_export());
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 1);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_transitions().row_count(), 0);

        if std::env::var_os("PRISM_TEST_FFMPEG").is_some() {
            runtime.seek_time(&ui, RationalTime::new(1, 2)?);
            ui.set_selected_media("av-ntsc-48k.mp4".into());
            runtime.select_media(&ui, 0);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while (!ui.get_has_program_frame()
                || !ui.get_has_source_frame()
                || !runtime
                    .cards
                    .borrow()
                    .first()
                    .is_some_and(|card| card.has_frame))
                && std::time::Instant::now() < deadline
            {
                runtime.poll(&ui);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                ui.get_has_program_frame(),
                "Program preview did not decode: {}",
                ui.get_feedback()
            );
            assert_eq!(ui.get_program_luma_scope().row_count(), 32);
            assert_eq!(ui.get_program_luma_waveform().row_count(), 64);
            assert_eq!(ui.get_program_vectorscope().row_count(), 256);
            assert!(
                ui.get_has_source_frame(),
                "Source preview did not decode: {}",
                ui.get_feedback()
            );
        }
        let media_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while ui.get_media_task_pending() > 0 && std::time::Instant::now() < media_deadline {
            runtime.poll(&ui);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(ui.get_media_task_pending(), 0);
        assert!(matches!(
            ui.get_media_task_status().as_str(),
            "COMPLETE" | "COMPLETE WITH ERRORS"
        ));

        assert_eq!(ui.get_media_cards().row_count(), 1);
        ui.set_search("no match".into());
        ui.invoke_filter_changed();
        assert_eq!(ui.get_media_cards().row_count(), 0);
        ui.set_search("AV-NTSC".into());
        ui.invoke_filter_changed();
        assert_eq!(
            ui.get_media_cards().row_data(0).map(|card| card.index),
            Some(0)
        );
        ui.set_search("".into());
        ui.invoke_filter_changed();
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        assert!(!ui.get_inspector_name().is_empty());
        assert!(ui.get_can_undo());
        assert_eq!(ui.get_history_items().row_count(), 5);

        // Render all four workspaces and the minimum supported desktop size.
        // Optional PPM artifacts support visual review without a live desktop session.
        let snapshot_directory = std::env::var_os("PRISM_UI_SNAPSHOTS");
        for (width, height) in [(1440, 900), (1100, 720)] {
            window.set_size(slint::PhysicalSize::new(width, height));
            for workspace in [
                "HOME",
                "IMPORT",
                "EDIT",
                "EXPORT",
                "HISTORY",
                "JOBS",
                "ASSEMBLY",
                "COLLAPSED",
                "ZOOM",
                "SOURCE",
                "PRECISION",
                "PAIRED",
                "SCOPES",
                "WAVEFORM",
                "VECTOR",
                "MARKERS",
                "COMMANDS",
                "COMMANDS_EMPTY",
                "SHORTCUTS",
                "EXPORT4K",
            ] {
                ui.set_palette_open(matches!(workspace, "COMMANDS" | "COMMANDS_EMPTY"));
                ui.set_palette_query(
                    if workspace == "COMMANDS_EMPTY" {
                        "no such action 123"
                    } else {
                        "marker"
                    }
                    .into(),
                );
                ui.invoke_palette_filter();
                ui.set_palette_index(0);
                ui.set_inspector_section(
                    if workspace == "SOURCE" {
                        "Source"
                    } else {
                        "Clip"
                    }
                    .into(),
                );
                ui.set_precision_open(workspace == "PRECISION");
                ui.set_paired_tools_open(workspace == "PAIRED");
                ui.set_program_scope_mode(match workspace {
                    "WAVEFORM" => 1,
                    "VECTOR" => 2,
                    _ => 0,
                });
                ui.set_browser_tab(
                    if workspace == "MARKERS" {
                        "Markers"
                    } else {
                        "Media"
                    }
                    .into(),
                );
                ui.set_export_preset_index(i32::from(workspace == "EXPORT4K"));
                ui.set_layout_preset(
                    if workspace == "ASSEMBLY" {
                        "Assembly"
                    } else {
                        "Editing"
                    }
                    .into(),
                );
                ui.set_inspector_open(workspace != "COLLAPSED");
                ui.set_inspector_tab(
                    match workspace {
                        "SHORTCUTS" => "Shortcuts",
                        "HISTORY" => "History",
                        "JOBS" => "Jobs",
                        "SCOPES" | "WAVEFORM" | "VECTOR" => "Scopes",
                        _ => "Properties",
                    }
                    .into(),
                );
                ui.set_timeline_zoom(if workspace == "ZOOM" { 2.0 } else { 1.0 });
                ui.set_timeline_offset(if workspace == "ZOOM" { 0.25 } else { 0.0 });
                runtime.update_ruler(&ui);
                ui.set_active_workspace(
                    match workspace {
                        "HOME" | "IMPORT" | "EXPORT" => workspace,
                        "EXPORT4K" => "EXPORT",
                        _ => "EDIT",
                    }
                    .into(),
                );
                let mut buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(width, height);
                window.request_redraw();
                assert!(window.draw_if_needed(|renderer| {
                    renderer.render(buffer.make_mut_slice(), width as usize);
                }));
                assert!(buffer.as_bytes().iter().any(|byte| *byte > 80));
                if let Some(directory) = &snapshot_directory {
                    use std::io::Write;
                    std::fs::create_dir_all(directory)?;
                    let mut file = std::fs::File::create(
                        std::path::Path::new(directory).join(format!("{workspace}-{width}.ppm")),
                    )?;
                    write!(file, "P6\n{width} {height}\n255\n")?;
                    file.write_all(buffer.as_bytes())?;
                }
            }
        }
        // Launcher filtering, context eligibility, and execution use the real callbacks.
        ui.set_palette_query("split selected".into());
        ui.invoke_palette_filter();
        assert_eq!(ui.get_command_items().row_count(), 1);
        ui.set_inspector_locked(true);
        ui.invoke_palette_filter();
        assert!(
            !ui.get_command_items()
                .row_data(0)
                .is_some_and(|item| item.enabled)
        );
        let revision = runtime.project.borrow().as_ref().map(Project::revision);
        ui.invoke_palette_run("timeline.split".into());
        assert_eq!(
            runtime.project.borrow().as_ref().map(Project::revision),
            revision
        );
        ui.set_inspector_locked(false);
        ui.set_palette_query("trim tool".into());
        ui.invoke_palette_filter();
        assert_eq!(ui.get_command_items().row_count(), 1);
        ui.set_palette_open(true);
        ui.invoke_palette_run("tool.T".into());
        assert_eq!(ui.get_active_tool(), "T");
        assert!(!ui.get_palette_open());
        ui.invoke_palette_run("tool.V".into());
        ui.invoke_palette_run("view.source".into());
        assert_eq!(ui.get_inspector_section(), "Source");
        assert_eq!(ui.get_inspector_tab(), "Properties");
        ui.invoke_palette_run("view.markers".into());
        assert_eq!(ui.get_browser_tab(), "Markers");
        ui.set_palette_query("no matching action 987".into());
        ui.invoke_palette_filter();
        assert_eq!(ui.get_command_items().row_count(), 0);

        // Rename is one undoable transaction that preserves marker identity/time.
        let initial_markers = ui.get_timeline_markers().row_count();
        runtime.add_marker(&ui)?;
        let marker_index = ui.get_timeline_markers().row_count() - 1;
        let marker_before = runtime
            .project
            .borrow()
            .as_ref()
            .and_then(|p| p.sequences.values().next())
            .and_then(|s| s.markers.get(marker_index))
            .cloned()
            .ok_or("marker missing")?;
        ui.set_selected_marker_index(i32::try_from(marker_index)?);
        ui.set_marker_label("Opening beat".into());
        ui.invoke_command_invoked("timeline.rename_marker".into());
        let saved_markers = prism_persistence::load(&path)?;
        let renamed = saved_markers
            .sequences
            .values()
            .next()
            .and_then(|s| s.markers.iter().find(|m| m.id == marker_before.id))
            .ok_or("renamed marker missing")?;
        assert_eq!(renamed.label, "Opening beat");
        assert_eq!(renamed.time, marker_before.time);
        ui.set_marker_label("   ".into());
        assert!(runtime.rename_marker(&ui).is_err());
        runtime.apply_history(&ui, false)?;
        assert_eq!(
            ui.get_timeline_markers()
                .row_data(marker_index)
                .map(|m| m.label.to_string()),
            Some(marker_before.label)
        );
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_markers().row_count(), initial_markers);

        ui.set_selected_clip_index(1);
        ui.invoke_command_invoked("timeline.delete".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        runtime.seek_ratio(&ui, 1.0);
        assert!(!ui.get_has_program_frame()); // Half-open clip interval at sequence end.
        assert_eq!(ui.get_timecode(), "00:00:01:00");
        assert_eq!(display_timecode(RationalTime::new(3661, 1)?), "01:01:01:00");
        ui.invoke_command_invoked("edit.undo".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        ui.invoke_command_invoked("edit.redo".into());
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        assert!(ui.get_can_undo());
        let undo_count = runtime.undo.borrow().len();
        *runtime.project_path.borrow_mut() = Some(path.join("cannot-save-here.prism"));
        assert!(runtime.apply_history(&ui, false).is_err());
        assert_eq!(runtime.undo.borrow().len(), undo_count);
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        *runtime.project_path.borrow_mut() = Some(path.clone());

        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        ui.set_clip_start("0".into());
        ui.set_clip_source_in("0".into());
        ui.set_clip_duration("0.5".into());
        runtime.apply_timing(&ui)?;
        assert_eq!(ui.get_duration_label(), "00:00:00:15");
        assert!(!ui.get_can_redo());
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_duration_label(), "00:00:01:00");

        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.adjust_clip_frame(&ui, "timeline.move_right")?;
        assert_eq!(ui.get_clip_start(), "1/30");
        runtime.apply_history(&ui, false)?;
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.adjust_clip_frame(&ui, "timeline.trim_out_left")?;
        assert_eq!(ui.get_clip_duration(), "29/30");
        runtime.apply_history(&ui, false)?;
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.drag_clip(&ui, 0, 1.0 / 30.0, "slip")?;
        assert_eq!(ui.get_clip_source_in(), "1/30");
        runtime.apply_history(&ui, false)?;
        assert!(
            runtime
                .precision_edit_frame(&ui, "timeline.roll_out_right")
                .is_err()
        );

        runtime.drag_clip(&ui, 0, 1.0 / 30.0, "move")?;
        assert_eq!(ui.get_clip_start(), "1/30");
        runtime.apply_history(&ui, false)?;
        runtime.drag_clip(&ui, 0, -1.0 / 30.0, "out")?;
        assert_eq!(ui.get_clip_duration(), "29/30");
        runtime.apply_history(&ui, false)?;

        runtime.seek_time(&ui, RationalTime::ZERO);
        runtime.handle("playback.play_pause", &ui)?;
        assert!(ui.get_playing());
        let decoding_generation = runtime.program_preview.generation.get();
        ui.set_has_program_frame(true);
        runtime.seek_time(&ui, RationalTime::new(1, 20)?);
        assert!(ui.get_has_program_frame());
        assert_eq!(
            runtime.program_preview.generation.get(),
            decoding_generation
        );
        let playback_started = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(100))
            .ok_or("playback start instant underflow")?;
        let master = crate::audio_clock::AudioClock::default();
        *runtime.audio_clock.borrow_mut() = Some(master.clone());
        runtime.playback_started.set(Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(2))
                .ok_or("playback start instant underflow")?,
        ));
        let held = runtime.playhead.get();
        runtime.poll(&ui);
        assert_eq!(runtime.playhead.get(), held);
        assert!(ui.get_playback_buffering());
        master.observe(b"0.12 M-A: 0 aq=4KB");
        runtime.poll(&ui);
        assert_eq!(
            runtime.playhead.get(),
            runtime
                .playback_origin
                .get()
                .checked_add(RationalTime::new(120, 1000)?)?
        );
        assert!(!ui.get_playback_buffering());
        runtime.audio_clock.borrow_mut().take();
        runtime.playback_started.set(Some(playback_started));
        runtime.poll(&ui);
        assert!(runtime.playhead.get() > RationalTime::ZERO);
        // A real child stands in for ffplay, so Pause must terminate and reap it.
        #[cfg(unix)]
        let audio_pid = {
            let child = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
            let pid = child.id();
            *runtime.audio_process.borrow_mut() = Some(child);
            pid
        };
        #[cfg(unix)]
        let decoder_pid = {
            let child = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
            let pid = child.id();
            *runtime.audio_decoder.borrow_mut() = Some(child);
            pid
        };
        let paused_position = runtime.playhead.get();
        runtime.handle("playback.play_pause", &ui)?;
        assert!(!ui.get_playing());
        assert!(!runtime.playing.get());
        assert!(runtime.playback_started.get().is_none());
        assert!(runtime.audio_process.borrow().is_none());
        assert!(runtime.audio_decoder.borrow().is_none());
        #[cfg(unix)]
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", &decoder_pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()?
                .success()
        );
        assert_eq!(runtime.playhead.get(), paused_position);
        #[cfg(unix)]
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", &audio_pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()?
                .success(),
            "Pause left the audio process running"
        );

        *runtime.audio_clock.borrow_mut() = Some(crate::audio_clock::AudioClock::default());
        runtime.playing.set(true);
        ui.set_playing(true);
        runtime.playback_started.set(Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(6))
                .ok_or("playback start instant underflow")?,
        ));
        runtime.poll(&ui);
        assert!(!runtime.playing.get());
        assert!(!ui.get_playback_buffering());
        assert!(runtime.audio_clock.borrow().is_none());
        runtime.playing.set(true);
        ui.set_playing(true);
        runtime
            .playback_started
            .set(Some(std::time::Instant::now()));
        runtime
            .program_preview
            .generation
            .set(runtime.program_preview.generation.get().wrapping_add(1));
        runtime.program_preview.pending.borrow_mut().take();
        runtime.program_preview.busy.set(true);
        runtime.program_preview.requested_at.set(Some(
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(6))
                .ok_or("preview request instant underflow")?,
        ));
        #[cfg(unix)]
        let watchdog_pids = {
            let player = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
            let decoder = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
            let pids = [player.id(), decoder.id()];
            *runtime.audio_process.borrow_mut() = Some(player);
            *runtime.audio_decoder.borrow_mut() = Some(decoder);
            pids
        };
        runtime.poll(&ui);
        #[cfg(unix)]
        for pid in watchdog_pids {
            assert!(
                !std::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stderr(std::process::Stdio::null())
                    .status()?
                    .success()
            );
        }
        assert!(ui.get_preview_status().contains("timed out"));
        assert!(!runtime.playing.get());
        assert!(!runtime.program_preview.busy.get());
        assert!(runtime.program_preview.requested_at.get().is_none());
        assert!(runtime.audio_process.borrow().is_none());
        assert!(runtime.audio_decoder.borrow().is_none());
        let sequence_position = runtime.playhead.get();
        runtime.source_duration.set(RationalTime::new(1, 1)?);
        runtime.source_in.set(RationalTime::new(1, 4)?);
        runtime.source_out.set(RationalTime::new(1, 2)?);
        ui.set_source_ready(true);
        ui.set_selected_media_index(0);
        runtime.seek_source(&ui, 0.4);
        assert_eq!(runtime.playhead.get(), sequence_position);
        runtime.append_source(&ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        ui.set_selected_clip_index(0);
        runtime.edit_clip("timeline.ripple_delete", &ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 1);
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        ui.set_selected_clip_index(0);
        runtime.drag_clip(&ui, 0, 1.0 / 30.0, "roll_out")?;
        runtime.apply_history(&ui, false)?;
        runtime.append_source(&ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 3);
        ui.set_selected_clip_index(1);
        runtime.drag_clip(&ui, 1, 1.0 / 30.0, "slide")?;
        runtime.apply_history(&ui, false)?;
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        let saved = prism_persistence::load(&path)?;
        let sequence = saved.sequences.values().next().ok_or("missing sequence")?;
        assert_eq!(
            sequence.tracks[0].clips[1].source_start,
            RationalTime::new(1, 4)?
        );
        assert_eq!(
            sequence.tracks[0].clips[1].source_duration,
            RationalTime::new(1, 4)?
        );
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 1);

        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.duplicate_clip(&ui)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 2);
        let duplicated = prism_persistence::load(&path)?;
        let duplicated_track = &duplicated
            .sequences
            .values()
            .next()
            .ok_or("missing sequence")?
            .tracks[0];
        assert_eq!(
            duplicated_track.clips[1].source_start,
            duplicated_track.clips[0].source_start
        );
        assert_eq!(
            duplicated_track.clips[1].source_duration,
            duplicated_track.clips[0].source_duration
        );
        assert_eq!(
            duplicated_track.clips[1].timeline_start,
            duplicated_track.clips[0]
                .timeline_start
                .checked_add(duplicated_track.clips[0].source_duration)?
        );
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_timeline_clips().row_count(), 1);

        let replacement = std::env::temp_dir().join(format!(
            "prism-relink-test-{}-{nonce}.mp4",
            std::process::id()
        ));
        std::fs::copy(&fixture, &replacement)?;
        ui.set_selected_media_index(0);
        runtime.relink_source_to(&ui, &replacement)?;
        assert_eq!(ui.get_source_detail(), replacement.display().to_string());
        assert_eq!(
            prism_persistence::load(&path)?
                .media
                .values()
                .next()
                .map(|asset| asset.source_path.as_str()),
            replacement.to_str()
        );
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_source_detail(), fixture.display().to_string());

        let media_id = runtime
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.media.keys().next().copied())
            .ok_or("missing media")?;
        let proxy_path =
            std::env::temp_dir().join(format!("prism-ui-proxy-test-{}.mp4", std::process::id()));
        std::fs::write(&proxy_path, b"proxy")?;
        let (proxy_sender, proxy_receiver) = std::sync::mpsc::sync_channel(1);
        proxy_sender.send(Ok(proxy_path.clone()))?;
        let recorded_source = fingerprint(&fixture)?;
        *runtime.proxy_generation.borrow_mut() = Some(ProxyGeneration {
            receiver: proxy_receiver,
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            media_id,
            source_fingerprint: ProxySourceFingerprint {
                sha256: recorded_source.sha256,
                byte_length: recorded_source.byte_length,
            },
        });
        ui.set_proxy_generating(true);
        runtime.poll(&ui);
        assert_eq!(ui.get_selected_proxy(), proxy_path.display().to_string());
        assert_eq!(ui.get_proxy_status(), PROXY_AVAILABLE);
        assert!(ui.get_proxy_active());
        assert!(!ui.get_proxy_generating());
        runtime.clear_proxy(&ui)?;
        assert_eq!(ui.get_selected_proxy(), "");
        assert!(!ui.get_proxy_active());
        let sequence_id = runtime
            .project
            .borrow()
            .as_ref()
            .and_then(|project| project.sequences.values().next())
            .map(|sequence| sequence.id)
            .ok_or("missing sequence")?;
        let audio_track_id = TrackId::new();
        runtime.commit(
            &ui,
            Command::CreateTrack {
                sequence_id,
                id: audio_track_id,
                kind: TrackKind::Audio,
            },
            "Create test audio track",
        )?;
        ui.set_selected_clip_index(0);
        runtime.update_inspector(&ui);
        runtime.change_track(&ui, 0, "down")?;
        assert_eq!(ui.get_selected_track_index(), 1);
        assert_eq!(
            prism_persistence::load(&path)?
                .sequences
                .values()
                .next()
                .and_then(|sequence| sequence.tracks.first())
                .map(|track| track.id),
            Some(audio_track_id)
        );
        runtime.apply_history(&ui, false)?;
        runtime.apply_history(&ui, false)?;
        assert_eq!(ui.get_tracks().row_count(), 1);
        let cache_root = path.with_extension("prism-cache");
        let cache_layout = CacheLayout::new(&cache_root);
        let cache_started = std::time::Instant::now();
        while runtime
            .media_artifacts
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.pending_writes() > 0)
            && cache_started.elapsed() < std::time::Duration::from_secs(2)
        {
            runtime.poll(&ui);
            std::thread::yield_now();
        }
        runtime.poll(&ui);
        assert!(cache_layout.inventory()?.artifact_count >= 1);
        runtime.update_cache_state(&ui);
        assert_eq!(ui.get_cache_status(), "READY");
        assert!(!ui.get_cache_detail().starts_with("0 artifacts"));
        runtime.clean_cache(&ui)?;
        assert!(ui.get_cache_cleaning());
        let cleanup_started = std::time::Instant::now();
        while ui.get_cache_cleaning()
            && cleanup_started.elapsed() < std::time::Duration::from_secs(2)
        {
            runtime.poll(&ui);
            std::thread::yield_now();
        }
        assert!(!ui.get_cache_cleaning());
        assert_eq!(ui.get_cache_detail(), "0 artifacts · 0 B");
        runtime.update_autosave_preferences(&ui, true)?;
        assert!(!ui.get_autosave_enabled());
        assert_eq!(ui.get_autosave_status(), "DISABLED");
        runtime.update_autosave_preferences(&ui, false)?;
        assert_eq!(ui.get_autosave_retention(), 10);
        runtime.update_autosave_preferences(&ui, true)?;
        assert!(ui.get_autosave_enabled());
        let mut av = Project::new("Export checks");
        super::insert_media_into_project(&mut av, &fixture, RationalTime::new(1, 1)?, true)?;
        super::update_export_summary(&ui, &av);
        assert!(ui.get_can_export());
        assert!(ui.get_export_audio_summary().contains("AAC"));
        let mut layered = av.clone();
        let sequence = layered.sequences.values_mut().next().unwrap();
        let mut extra = sequence
            .tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .unwrap()
            .clone();
        extra.id = TrackId::new();
        for clip in &mut extra.clips {
            clip.id = prism_core::ClipId::new();
        }
        sequence.tracks.push(extra);
        super::update_export_summary(&ui, &layered);
        assert!(ui.get_can_export());
        assert!(ui.get_export_video_summary().contains("2 active tracks"));
        {
            let sequence = av
                .sequences
                .values_mut()
                .next()
                .ok_or("sequence unavailable")?;
            let audio = sequence
                .tracks
                .iter_mut()
                .find(|t| t.kind == TrackKind::Audio)
                .ok_or("audio unavailable")?;
            audio.clips[0].source_duration = RationalTime::new(1, 2)?;
        }
        super::update_export_summary(&ui, &av);
        assert!(ui.get_can_export());
        assert!(ui.get_export_audio_summary().contains("1 active tracks"));
        {
            let sequence = av
                .sequences
                .values_mut()
                .next()
                .ok_or("sequence unavailable")?;
            for track in &mut sequence.tracks {
                if track.kind == TrackKind::Audio {
                    track.muted = true;
                }
            }
        }
        super::update_export_summary(&ui, &av);
        assert!(ui.get_can_export());
        assert!(ui.get_export_audio_summary().contains("video-only"));
        {
            let sequence = av
                .sequences
                .values_mut()
                .next()
                .ok_or("sequence unavailable")?;
            for track in &mut sequence.tracks {
                if track.kind == TrackKind::Video {
                    track.visible = false;
                }
            }
        }
        super::update_export_summary(&ui, &av);
        assert!(!ui.get_can_export());
        let match_revision = runtime.project.borrow().as_ref().map(Project::revision);
        ui.set_selected_clip_index(0);
        let expected_match = runtime
            .project
            .borrow()
            .as_ref()
            .and_then(|project| super::matched_source(project, 0, runtime.playhead.get()))
            .ok_or("Missing match")?;
        runtime.handle("source.match_frame", &ui)?;
        assert_eq!(runtime.source_position.get(), expected_match.1);
        assert_eq!(
            ui.get_selected_media_index(),
            i32::try_from(expected_match.0)?
        );
        assert_eq!(ui.get_inspector_section(), "Source");
        assert_eq!(
            runtime.project.borrow().as_ref().map(Project::revision),
            match_revision
        );
        ui.set_selected_clip_index(-1);
        assert!(runtime.handle("source.match_frame", &ui).is_err());
        // Navigation reveals off-screen playheads, and Fit restores the complete view.
        ui.set_timeline_zoom(8.0);
        ui.set_timeline_offset(0.0);
        runtime.handle("playback.end", &ui)?;
        assert_eq!(ui.get_playhead_ratio(), 1.0);
        assert_eq!(ui.get_timeline_offset(), 0.875);
        runtime.handle("playback.start", &ui)?;
        assert_eq!(ui.get_playhead_ratio(), 0.0);
        assert_eq!(ui.get_timeline_offset(), 0.0);
        runtime.handle("timeline.fit", &ui)?;
        assert_eq!(ui.get_timeline_zoom(), 1.0);
        assert_eq!(ui.get_timeline_offset(), 0.0);
        // Source range controls describe exact duration and never edit the sequence.
        let before_range = runtime.project.borrow().as_ref().map(Project::revision);
        runtime.source_duration.set(RationalTime::new(4, 1)?);
        runtime.source_in.set(RationalTime::new(1, 1)?);
        runtime.source_out.set(RationalTime::new(3, 1)?);
        runtime.update_source_labels(&ui);
        assert_eq!(ui.get_source_in_ratio(), 0.25);
        assert_eq!(ui.get_source_out_ratio(), 0.75);
        assert_eq!(ui.get_source_range_label(), "00:00:02:00");
        ui.set_source_ready(true);
        runtime.handle("source.clear_marks", &ui)?;
        assert_eq!(runtime.source_in.get(), RationalTime::ZERO);
        assert_eq!(runtime.source_out.get(), RationalTime::new(4, 1)?);
        assert_eq!(ui.get_source_range_label(), "00:00:04:00");
        assert_eq!(ui.get_source_in_ratio(), 0.0);
        assert_eq!(ui.get_source_out_ratio(), 1.0);
        assert_eq!(
            runtime.project.borrow().as_ref().map(Project::revision),
            before_range
        );
        ui.set_source_ready(false);
        assert!(runtime.handle("source.clear_marks", &ui).is_err());
        runtime.source_duration.set(RationalTime::ZERO);
        runtime.source_in.set(RationalTime::ZERO);
        runtime.source_out.set(RationalTime::ZERO);
        runtime.update_source_labels(&ui);
        assert_eq!(ui.get_source_out_ratio(), 0.0);
        // Media lookup follows source IDs and does not create an editing transaction.
        let lookup_revision = runtime.project.borrow().as_ref().map(Project::revision);
        ui.set_selected_media_index(0);
        ui.set_selected_clip_index(-1);
        runtime.find_media_in_timeline(&ui)?;
        let first_found = ui.get_selected_clip_index();
        assert!(first_found >= 0);
        assert_eq!(ui.get_inspector_section(), "Clip");
        runtime.find_media_in_timeline(&ui)?;
        assert!(ui.get_selected_clip_index() >= 0);
        assert_eq!(
            runtime.project.borrow().as_ref().map(Project::revision),
            lookup_revision
        );
        ui.set_search("".into());
        ui.set_media_filter(1);
        runtime.filter_cards(&ui);
        assert!(ui.get_media_cards().row_count() > 0);
        assert!(ui.get_media_cards().iter().all(|card| card.uses > 0));
        ui.set_media_filter(2);
        runtime.filter_cards(&ui);
        assert!(ui.get_media_cards().iter().all(|card| card.uses == 0));
        ui.set_media_filter(3);
        runtime.filter_cards(&ui);
        assert!(ui.get_media_cards().iter().all(|card| card.offline));
        ui.set_media_filter(0);
        runtime.filter_cards(&ui);
        // View persistence must not mutate editing history or source identities.
        let edit_revision = runtime.project.borrow().as_ref().map(Project::revision);
        ui.set_layout_preset("Assembly".into());
        ui.set_inspector_section("Source".into());
        ui.set_media_sort(2);
        runtime.handle("view.scopes", &ui)?;
        ui.set_media_filter(2);
        ui.set_program_scope_mode(2);
        ui.set_show_monitor_scope(true);
        runtime.save_view(&ui)?;
        runtime.handle("view.reset", &ui)?;
        assert_eq!(ui.get_layout_preset(), "Editing");
        runtime.restore_view(&ui);
        assert_eq!(ui.get_layout_preset(), "Assembly");
        assert_eq!(ui.get_inspector_section(), "Source");
        assert_eq!(ui.get_media_sort(), 2);
        assert_eq!(ui.get_inspector_tab(), "Scopes");
        assert_eq!(ui.get_program_scope_mode(), 2);
        assert!(ui.get_show_monitor_scope());
        assert_eq!(ui.get_media_filter(), 2);
        ui.set_media_filter(0);
        assert_eq!(
            runtime.project.borrow().as_ref().map(Project::revision),
            edit_revision
        );
        let saved_cards = runtime.cards.borrow().clone();
        let mut sorted_cards = Vec::new();
        for (name, index) in [("Beta", 0), ("Alpha", 8), ("Gamma", 3)] {
            let mut card = saved_cards[0].clone();
            card.name = name.into();
            card.index = index;
            sorted_cards.push(card);
        }
        *runtime.cards.borrow_mut() = sorted_cards;
        ui.set_search("".into());
        ui.set_media_sort(1);
        runtime.filter_cards(&ui);
        assert_eq!(
            ui.get_media_cards().row_data(0).map(|card| card.index),
            Some(8)
        );
        ui.set_media_sort(2);
        runtime.filter_cards(&ui);
        assert_eq!(
            ui.get_media_cards().row_data(0).map(|card| card.index),
            Some(3)
        );
        ui.set_search("bEtA".into());
        runtime.filter_cards(&ui);
        assert_eq!(ui.get_media_cards().row_count(), 1);
        assert_eq!(
            ui.get_media_cards().row_data(0).map(|card| card.index),
            Some(0)
        );
        *runtime.cards.borrow_mut() = saved_cards;
        std::fs::write(path.with_extension("workspace.json"), br#"{"version":999}"#)?;
        runtime.restore_view(&ui);
        assert_eq!(ui.get_layout_preset(), "Editing");
        assert!(ui.get_feedback().contains("Could not restore"));
        std::fs::remove_file(path.with_extension("workspace.json"))?;
        // Save As protects the original and directs subsequent edits to the copy.
        let original_revision = prism_persistence::load(&path)?.revision();
        let copy_path = path.with_extension("copy.prism");
        let undo_count = runtime.undo.borrow().len();
        runtime.save_as_to(&ui, &copy_path)?;
        assert_eq!(runtime.project_path.borrow().as_ref(), Some(&copy_path));
        assert_eq!(runtime.undo.borrow().len(), undo_count);
        assert!(runtime.save_as_to(&ui, &path).is_err());
        assert_eq!(runtime.project_path.borrow().as_ref(), Some(&copy_path));
        runtime.handle("timeline.add_marker", &ui)?;
        assert!(prism_persistence::load(&copy_path)?.revision() > original_revision);
        assert_eq!(
            prism_persistence::load(&path)?.revision(),
            original_revision
        );
        runtime.autosave_scheduler.borrow_mut().take();
        for candidate in prism_persistence::list_autosaves(&copy_path)? {
            std::fs::remove_file(candidate.path)?;
        }
        std::fs::remove_file(&copy_path)?;
        let copy_preferences = copy_path.with_extension("autosave.json");
        if copy_preferences.exists() {
            std::fs::remove_file(copy_preferences)?;
        }
        let copy_cache = copy_path.with_extension("prism-cache");
        if copy_cache.exists() {
            std::fs::remove_dir_all(copy_cache)?;
        }
        runtime.autosave_scheduler.borrow_mut().take();
        for candidate in prism_persistence::list_autosaves(&path)? {
            std::fs::remove_file(candidate.path)?;
        }
        std::fs::remove_file(proxy_path)?;
        std::fs::remove_file(replacement)?;
        std::fs::remove_file(&path)?;
        std::fs::remove_file(path.with_extension("autosave.json"))?;
        std::fs::remove_dir_all(cache_root)?;
        ui.hide()?;
        Ok(())
    }

    #[test]
    fn proxy_versions_never_overwrite_existing_media() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("prism-proxy-version-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let base = directory.join("interview.proxy.mp4");
        let version_two = directory.join("interview.proxy.v2.mp4");
        std::fs::write(&base, b"original proxy")?;
        std::fs::write(&version_two, b"second proxy")?;
        assert_eq!(
            next_proxy_version(&base)?,
            directory.join("interview.proxy.v3.mp4")
        );
        assert_eq!(
            next_proxy_version(&version_two)?,
            directory.join("interview.proxy.v3.mp4")
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn inspector_timing_parser_is_exact_and_rejects_invalid_values()
    -> Result<(), Box<dyn std::error::Error>> {
        use super::parse_timing;
        assert_eq!(parse_timing("1.25")?, RationalTime::new(5, 4)?);
        assert_eq!(
            parse_timing(" 1001/30000 ")?,
            RationalTime::new(1001, 30000)?
        );
        assert_eq!(parse_timing(".5")?, RationalTime::new(1, 2)?);
        for input in [
            "-1",
            "-0.5",
            "NaN",
            "inf",
            "1/0",
            "0.1234567",
            "1..2",
            "9223372036854775807.9",
            "",
        ] {
            assert!(parse_timing(input).is_err(), "accepted {input}");
        }
        Ok(())
    }
    #[test]
    fn export_preset_selection_uses_the_typed_encoder_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(super::export_preset(0)?.settings().0, 1920);
        assert_eq!(super::export_preset(1)?.settings().0, 3840);
        assert_eq!(super::export_preset(1)?.settings().3, 35_000);
        assert_eq!(super::export_preset(2)?.settings().1, 1920);
        assert_eq!(super::export_preset(3)?.settings().0, 1080);
        assert!(super::export_preset(-1).is_err());
        assert!(super::export_preset(4).is_err());
        Ok(())
    }

    #[test]
    fn waveform_bars_preserve_bucket_peaks_and_bound_ui_nodes() {
        let points = (0..250)
            .map(|index| prism_audio::WaveformPoint {
                min: if index == 120 { -1.0 } else { -0.25 },
                max: 0.5,
                rms: 0.2,
            })
            .collect::<Vec<_>>();
        let bars = super::waveform_bars(&points, 96);
        assert!(bars.len() <= 96);
        assert!(bars.iter().any(|peak| (*peak - 1.0).abs() < f32::EPSILON));
        assert!(bars.iter().all(|peak| (0.0..=1.0).contains(peak)));
    }

    #[test]
    fn waveform_range_projects_only_the_trimmed_source_interval() {
        let bars = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
        assert_eq!(super::waveform_range(&bars, 0.25, 0.5), bars[2..6]);
        assert_eq!(super::waveform_range(&bars, 0.875, 0.5), bars[7..8]);
        assert!(super::waveform_range(&bars, 0.0, 0.0).is_empty());
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn luma_scope_is_bounded_normalized_and_tracks_brightness() {
        let mut rgba = Vec::new();
        for value in [0_u8, 0, 128, 255] {
            rgba.extend([value, value, value, 255]);
        }
        let scope = super::luma_scope(&rgba, 4, 4);
        assert_eq!(scope.len(), 4);
        assert_eq!(scope[0], 1.0);
        assert_eq!(scope[2], 0.5);
        assert_eq!(scope[3], 0.5);
        assert!(scope.iter().all(|value| (0.0..=1.0).contains(value)));
        assert!(super::luma_scope(&rgba, 0, 4).is_empty());
    }

    #[test]
    fn luma_waveform_preserves_horizontal_brightness_ranges() {
        let rgba = [
            0, 0, 0, 255, 255, 255, 255, 255, 128, 128, 128, 255, 64, 64, 64, 255,
        ];
        let waveform = super::luma_waveform(&rgba, 2, 2, 2, 4);
        assert_eq!(waveform.len(), 2);
        assert!(waveform[0].low.abs() < f32::EPSILON);
        assert!((waveform[0].high - 128.0 / 255.0).abs() < 0.001);
        assert!((waveform[1].low - 64.0 / 255.0).abs() < 0.001);
        assert!((waveform[1].high - 1.0).abs() < f32::EPSILON);
        assert!(super::luma_waveform(&rgba, 0, 2, 2, 4).is_empty());
    }

    #[test]
    fn vectorscope_places_neutral_and_saturated_samples_in_chroma_space() {
        let rgba = [128, 128, 128, 255, 255, 0, 0, 255];
        let scope = super::vectorscope(&rgba, 4, 8);
        assert_eq!(scope.len(), 16);
        assert!(
            (scope[10] - 1.0).abs() < f32::EPSILON,
            "neutral gray belongs at scope center"
        );
        assert!(
            (scope[1] - 1.0).abs() < f32::EPSILON,
            "red belongs above and left of center"
        );
        assert_eq!(scope.iter().filter(|density| **density > 0.0).count(), 2);
        assert!(super::vectorscope(&rgba, 0, 8).is_empty());
        assert!(super::vectorscope(&rgba, 4, 0).is_empty());
    }

    #[test]
    fn thumbnail_preview_reuses_cache_before_decoding_source()
    -> Result<(), Box<dyn std::error::Error>> {
        let root =
            std::env::temp_dir().join(format!("prism-studio-thumbnail-hit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        let source = root.join("not-a-video.mov");
        std::fs::write(&source, b"fingerprinted but intentionally not a video")?;
        let cache_root = root.join("cache");
        let fingerprint = fingerprint(&source)?;
        let entry = MediaCatalogEntry {
            path: source.clone(),
            kind: MediaItemKind::Video,
            metadata: None,
            fingerprint: Some(fingerprint),
            offline: false,
            diagnostic: None,
        };
        let artifacts = MediaArtifactWorker::spawn(&cache_root, 1);
        let frame = prism_media::DecodedVideoFrame {
            width: 1,
            height: 1,
            rgba: vec![12, 34, 56, 255],
        };
        artifacts.schedule_thumbnail_variant(&entry, &frame, 1, 1, BROWSER_THUMBNAIL_VARIANT)?;
        artifacts
            .result_timeout(std::time::Duration::from_secs(1))?
            .result?;

        let preview = PreviewWorker::with_thumbnails(true);
        preview.request_thumbnail(source.clone(), Some(cache_root.clone()));
        let started = std::time::Instant::now();
        let result = loop {
            if let Some(result) = preview.poll_decoded() {
                break result;
            }
            if started.elapsed() >= std::time::Duration::from_secs(1) {
                return Err("cached thumbnail timed out".into());
            }
            std::thread::yield_now();
        }?;
        assert_eq!(result.0, frame);
        assert!(result.3);

        let key = MediaArtifactWorker::thumbnail_variant_key(&entry, BROWSER_THUMBNAIL_VARIANT);
        let layout = CacheLayout::new(&cache_root);
        let artifact_path = layout
            .lookup(key, MediaArtifactKind::Thumbnail.extension())?
            .ok_or("cached thumbnail missing")?;
        std::fs::write(artifact_path, b"corrupt thumbnail")?;
        preview.request_thumbnail(source, Some(cache_root));
        let started = std::time::Instant::now();
        let corrupt_result = loop {
            if let Some(result) = preview.poll_decoded() {
                break result;
            }
            if started.elapsed() >= std::time::Duration::from_secs(1) {
                return Err("corrupt thumbnail repair timed out".into());
            }
            std::thread::yield_now();
        };
        assert!(corrupt_result.is_err());
        assert_eq!(
            layout.lookup(key, MediaArtifactKind::Thumbnail.extension())?,
            None
        );
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
