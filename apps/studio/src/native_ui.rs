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

mod launcher;
mod preview;
mod project;
mod project_edit;
mod runtime;
mod runtime_edit;
mod runtime_timeline;

use launcher::{filter_launcher, launch_default_export, launcher_actions};
use preview::{
    PreviewWorker, ProxyGeneration, display_timecode, display_timecode_at_rate, parse_timing,
    timing_text,
};
use project::import_media_into_project;
use project_edit::{apply_project_summary, insert_stream_media_into_project};
use runtime::NativeExportRuntime;

#[cfg(test)]
use project_edit::{bind_editor_callbacks, insert_media_into_project, insert_video_into_project};

pub use preview::NativeUiError;
pub use project_edit::launch;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod workspace_tests;
