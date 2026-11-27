use super::{
    Clip, ClipId, CommandError, Event, Marker, MarkerId, MediaAsset, MediaAssetId, Project,
    ProxySourceFingerprint, RationalTime, Sequence, SequenceId, Track, TrackId, TrackKind,
    Transition, TransitionId,
};

pub(super) fn import_media(
    project: &mut Project,
    id: MediaAssetId,
    source_path: String,
) -> Result<Event, CommandError> {
    if source_path.trim().is_empty() {
        return Err(CommandError::EmptyField {
            field: "source_path",
        });
    }
    if project.media.contains_key(&id) {
        return Err(CommandError::DuplicateId);
    }
    if project
        .media
        .values()
        .any(|asset| asset.source_path == source_path)
    {
        return Err(CommandError::DuplicateMediaPath);
    }
    project.media.insert(
        id,
        MediaAsset {
            id,
            source_path,
            proxy_path: None,
            proxy_source_fingerprint: None,
        },
    );
    Ok(Event::MediaImported { id })
}

pub(super) fn set_media_proxy(
    project: &mut Project,
    media_id: MediaAssetId,
    proxy_path: Option<String>,
    source_fingerprint: Option<ProxySourceFingerprint>,
) -> Result<Event, CommandError> {
    if proxy_path
        .as_deref()
        .is_some_and(|path| path.trim().is_empty())
    {
        return Err(CommandError::EmptyField {
            field: "proxy_path",
        });
    }
    if proxy_path.is_none() && source_fingerprint.is_some() {
        return Err(CommandError::InvalidProxyFingerprint);
    }
    if let Some(fingerprint) = &source_fingerprint
        && (fingerprint.byte_length == 0
            || fingerprint.sha256.len() != 64
            || !fingerprint
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(CommandError::InvalidProxyFingerprint);
    }
    let asset = project
        .media
        .get_mut(&media_id)
        .ok_or(CommandError::MediaNotFound)?;
    asset.proxy_path = proxy_path;
    asset.proxy_source_fingerprint = source_fingerprint;
    Ok(Event::MediaProxyUpdated { id: media_id })
}

pub(super) fn relink_media(
    project: &mut Project,
    media_id: MediaAssetId,
    source_path: String,
) -> Result<Event, CommandError> {
    if source_path.trim().is_empty() {
        return Err(CommandError::EmptyField {
            field: "source_path",
        });
    }
    if project
        .media
        .iter()
        .any(|(id, asset)| *id != media_id && asset.source_path == source_path)
    {
        return Err(CommandError::DuplicateMediaPath);
    }
    let asset = project
        .media
        .get_mut(&media_id)
        .ok_or(CommandError::MediaNotFound)?;
    asset.source_path = source_path;
    Ok(Event::MediaRelinked { id: media_id })
}

pub(super) fn create_sequence(
    project: &mut Project,
    id: SequenceId,
    name: String,
) -> Result<Event, CommandError> {
    if name.trim().is_empty() {
        return Err(CommandError::EmptyField { field: "name" });
    }
    if project.sequences.contains_key(&id) {
        return Err(CommandError::DuplicateId);
    }
    project.sequences.insert(
        id,
        Sequence {
            id,
            name,
            markers: Vec::new(),
            transitions: Vec::new(),
            tracks: Vec::new(),
        },
    );
    Ok(Event::SequenceCreated { id })
}

pub(super) fn set_sequence_frame_rate(
    project: &mut Project,
    sequence_id: SequenceId,
    frame_rate: RationalTime,
) -> Result<Event, CommandError> {
    project
        .set_sequence_frame_rate(sequence_id, frame_rate)
        .map_err(|error| CommandError::Invariant(error.to_string()))?;
    Ok(Event::SequenceFrameRateChanged {
        sequence_id,
        frame_rate,
    })
}

pub(super) fn add_marker(
    project: &mut Project,
    sequence_id: SequenceId,
    id: MarkerId,
    time: RationalTime,
    label: String,
) -> Result<Event, CommandError> {
    if !time.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "marker_time",
            constraint: "non-negative",
        });
    }
    if label.trim().is_empty() {
        return Err(CommandError::EmptyField { field: "label" });
    }
    if project
        .sequences
        .values()
        .any(|sequence| sequence.markers.iter().any(|marker| marker.id == id))
    {
        return Err(CommandError::DuplicateId);
    }
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let marker = Marker { id, time, label };
    let index = sequence
        .markers
        .partition_point(|existing| existing.time <= time);
    sequence.markers.insert(index, marker);
    Ok(Event::MarkerAdded { sequence_id, id })
}

pub(super) fn delete_marker(
    project: &mut Project,
    sequence_id: SequenceId,
    marker_id: MarkerId,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let index = sequence
        .markers
        .iter()
        .position(|marker| marker.id == marker_id)
        .ok_or(CommandError::MarkerNotFound)?;
    sequence.markers.remove(index);
    Ok(Event::MarkerDeleted {
        sequence_id,
        id: marker_id,
    })
}

pub(super) fn add_transition(
    project: &mut Project,
    sequence_id: SequenceId,
    transition: Transition,
) -> Result<Event, CommandError> {
    if !transition.duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "transition_duration",
            constraint: "positive",
        });
    }
    if project.sequences.values().any(|sequence| {
        sequence
            .transitions
            .iter()
            .any(|existing| existing.id == transition.id)
    }) {
        return Err(CommandError::DuplicateId);
    }
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let track = sequence
        .tracks
        .iter()
        .find(|track| {
            track.kind == TrackKind::Video
                && track
                    .clips
                    .iter()
                    .any(|clip| clip.id == transition.outgoing_clip)
                && track
                    .clips
                    .iter()
                    .any(|clip| clip.id == transition.incoming_clip)
        })
        .ok_or(CommandError::ClipNotFound)?;
    let outgoing = track
        .clips
        .iter()
        .find(|clip| clip.id == transition.outgoing_clip)
        .ok_or(CommandError::ClipNotFound)?;
    let incoming = track
        .clips
        .iter()
        .find(|clip| clip.id == transition.incoming_clip)
        .ok_or(CommandError::ClipNotFound)?;
    let outgoing_end = outgoing
        .timeline_start
        .checked_add(outgoing.source_duration)
        .map_err(|_| CommandError::InvalidTime {
            field: "transition_boundary",
            constraint: "representable",
        })?;
    let handles = transition
        .handles()
        .map_err(|_| CommandError::InvalidTime {
            field: "transition_duration",
            constraint: "representable",
        })?;
    if incoming.timeline_start != outgoing_end
        || handles.before_cut > outgoing.source_duration
        || handles.after_cut > incoming.source_duration
        || handles.incoming_preroll > incoming.source_start
    {
        return Err(CommandError::InvalidTransition);
    }
    let id = transition.id;
    sequence.transitions.push(transition);
    Ok(Event::TransitionAdded { sequence_id, id })
}

pub(super) fn delete_transition(
    project: &mut Project,
    sequence_id: SequenceId,
    transition_id: TransitionId,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let index = sequence
        .transitions
        .iter()
        .position(|transition| transition.id == transition_id)
        .ok_or(CommandError::TransitionNotFound)?;
    sequence.transitions.remove(index);
    Ok(Event::TransitionDeleted {
        sequence_id,
        id: transition_id,
    })
}

pub(super) fn create_track(
    project: &mut Project,
    sequence_id: SequenceId,
    id: TrackId,
    kind: TrackKind,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    if sequence.tracks.iter().any(|track| track.id == id) {
        return Err(CommandError::DuplicateId);
    }
    sequence.tracks.push(Track {
        id,
        kind,
        clips: Vec::new(),
        locked: false,
        visible: true,
        muted: false,
    });
    Ok(Event::TrackCreated { sequence_id, id })
}

pub(super) fn set_track_locked(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    locked: bool,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    track.locked = locked;
    Ok(Event::TrackLockChanged { track_id, locked })
}

pub(super) fn set_track_visible(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    visible: bool,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    track.visible = visible;
    Ok(Event::TrackVisibilityChanged { track_id, visible })
}

pub(super) fn set_track_muted(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    muted: bool,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    track.muted = muted;
    Ok(Event::TrackMuteChanged { track_id, muted })
}

pub(super) fn reorder_track(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    destination_index: usize,
) -> Result<Event, CommandError> {
    let sequence = project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?;
    let current_index = sequence
        .tracks
        .iter()
        .position(|track| track.id == track_id)
        .ok_or(CommandError::TrackNotFound)?;
    if destination_index >= sequence.tracks.len() {
        return Err(CommandError::InvalidTrackIndex);
    }
    if current_index != destination_index {
        let track = sequence.tracks.remove(current_index);
        sequence.tracks.insert(destination_index, track);
    }
    Ok(Event::TrackReordered {
        sequence_id,
        track_id,
        destination_index,
    })
}

#[derive(Clone, Copy)]
pub(super) struct ClipLocation {
    pub(super) sequence_id: SequenceId,
    pub(super) track_id: TrackId,
    pub(super) id: ClipId,
    pub(super) media_id: MediaAssetId,
    pub(super) timeline_start: RationalTime,
    pub(super) source_start: RationalTime,
    pub(super) source_duration: RationalTime,
}

pub(super) fn insert_clip(
    project: &mut Project,
    location: ClipLocation,
) -> Result<Event, CommandError> {
    validate_clip_time(
        location.timeline_start,
        location.source_start,
        location.source_duration,
    )?;
    if !project.media.contains_key(&location.media_id) {
        return Err(CommandError::MediaNotFound);
    }
    let track = find_track_mut(project, location.sequence_id, location.track_id)?;
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    if track.clips.iter().any(|clip| clip.id == location.id) {
        return Err(CommandError::DuplicateId);
    }
    ensure_range_available(
        track,
        location.timeline_start,
        location.source_duration,
        None,
    )?;
    track.clips.push(Clip {
        id: location.id,
        media_id: location.media_id,
        timeline_start: location.timeline_start,
        source_start: location.source_start,
        source_duration: location.source_duration,
    });
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipInserted {
        track_id: location.track_id,
        id: location.id,
    })
}

pub(super) fn duplicate_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    new_clip_id: ClipId,
    timeline_start: RationalTime,
) -> Result<Event, CommandError> {
    let (media_id, source_start, source_duration) = {
        let track = find_track_mut(project, sequence_id, track_id)?;
        track
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .map(|clip| (clip.media_id, clip.source_start, clip.source_duration))
            .ok_or(CommandError::ClipNotFound)?
    };
    insert_clip(
        project,
        ClipLocation {
            sequence_id,
            track_id,
            id: new_clip_id,
            media_id,
            timeline_start,
            source_start,
            source_duration,
        },
    )?;
    Ok(Event::ClipDuplicated {
        source_id: clip_id,
        id: new_clip_id,
    })
}

pub(super) fn move_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    timeline_start: RationalTime,
) -> Result<Event, CommandError> {
    if !timeline_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "timeline_start",
            constraint: "non-negative",
        });
    }
    let track = find_track_mut(project, sequence_id, track_id)?;
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    let duration = track
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?
        .source_duration;
    ensure_range_available(track, timeline_start, duration, Some(clip_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    clip.timeline_start = timeline_start;
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipMoved {
        id: clip_id,
        timeline_start,
    })
}

pub(super) fn slip_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    source_delta: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let source_start =
        clip.source_start
            .checked_add(source_delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "source_delta",
                constraint: "representable",
            })?;
    if !source_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "source_delta",
            constraint: "source start must remain non-negative",
        });
    }
    clip.source_start = source_start;
    Ok(Event::ClipSlipped { id: clip_id })
}

pub(super) fn roll_edit(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    left_clip_id: ClipId,
    right_clip_id: ClipId,
    delta: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let left_index = track
        .clips
        .iter()
        .position(|clip| clip.id == left_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let right_index = track
        .clips
        .iter()
        .position(|clip| clip.id == right_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    if right_index != left_index + 1 {
        return Err(CommandError::InvalidTime {
            field: "clip_boundary",
            constraint: "clips must be adjacent",
        });
    }
    let left_duration = track.clips[left_index].source_duration;
    let right_duration = track.clips[right_index].source_duration;
    let new_left_duration =
        left_duration
            .checked_add(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    let new_right_duration =
        right_duration
            .checked_sub(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    if !new_left_duration.is_positive() || !new_right_duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "resulting durations must be positive",
        });
    }
    let right_source_start = track.clips[right_index]
        .source_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    if !right_source_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "source start must remain non-negative",
        });
    }
    track.clips[left_index].source_duration = new_left_duration;
    track.clips[right_index].timeline_start = track.clips[right_index]
        .timeline_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    track.clips[right_index].source_start = right_source_start;
    track.clips[right_index].source_duration = new_right_duration;
    Ok(Event::ClipsRolled {
        left_clip_id,
        right_clip_id,
    })
}

pub(super) fn slide_edit(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    previous_clip_id: ClipId,
    clip_id: ClipId,
    next_clip_id: ClipId,
    delta: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let previous_index = track
        .clips
        .iter()
        .position(|clip| clip.id == previous_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let clip_index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let next_index = track
        .clips
        .iter()
        .position(|clip| clip.id == next_clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    if clip_index != previous_index + 1 || next_index != clip_index + 1 {
        return Err(CommandError::InvalidTime {
            field: "clip_neighbors",
            constraint: "clips must be adjacent",
        });
    }
    let previous_duration = track.clips[previous_index].source_duration;
    let next_duration = track.clips[next_index].source_duration;
    let new_previous_duration =
        previous_duration
            .checked_add(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    let new_next_duration =
        next_duration
            .checked_sub(delta)
            .map_err(|_| CommandError::InvalidTime {
                field: "delta",
                constraint: "representable",
            })?;
    if !new_previous_duration.is_positive() || !new_next_duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "neighbor durations must remain positive",
        });
    }
    let new_clip_start = track.clips[clip_index]
        .timeline_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    if !new_clip_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "clip start must remain non-negative",
        });
    }
    let new_next_source_start = track.clips[next_index]
        .source_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    if !new_next_source_start.is_non_negative() {
        return Err(CommandError::InvalidTime {
            field: "delta",
            constraint: "source start must remain non-negative",
        });
    }
    track.clips[previous_index].source_duration = new_previous_duration;
    track.clips[clip_index].timeline_start = new_clip_start;
    track.clips[next_index].timeline_start = track.clips[next_index]
        .timeline_start
        .checked_add(delta)
        .map_err(|_| CommandError::InvalidTime {
            field: "delta",
            constraint: "representable",
        })?;
    track.clips[next_index].source_start = new_next_source_start;
    track.clips[next_index].source_duration = new_next_duration;
    Ok(Event::ClipsSlid { id: clip_id })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trim_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
) -> Result<Event, CommandError> {
    validate_clip_time(timeline_start, source_start, source_duration)?;
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    if !track.clips.iter().any(|clip| clip.id == clip_id) {
        return Err(CommandError::ClipNotFound);
    }
    ensure_range_available(track, timeline_start, source_duration, Some(clip_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    clip.timeline_start = timeline_start;
    clip.source_start = source_start;
    clip.source_duration = source_duration;
    track.clips.sort_by_key(|clip| clip.timeline_start);
    Ok(Event::ClipTrimmed { id: clip_id })
}

pub(super) fn split_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
    right_clip_id: ClipId,
    timeline_position: RationalTime,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    if track.clips.iter().any(|clip| clip.id == right_clip_id) {
        return Err(CommandError::DuplicateId);
    }
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let original = track.clips[index].clone();
    let original_end = original
        .timeline_start
        .checked_add(original.source_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    if timeline_position <= original.timeline_start || timeline_position >= original_end {
        return Err(CommandError::InvalidTime {
            field: "timeline_position",
            constraint: "strictly inside the clip",
        });
    }
    let left_duration = timeline_position
        .checked_sub(original.timeline_start)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    let right_source_start = original
        .source_start
        .checked_add(left_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    let right_duration = original
        .source_duration
        .checked_sub(left_duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    track.clips[index].source_duration = left_duration;
    track.clips.insert(
        index + 1,
        Clip {
            id: right_clip_id,
            media_id: original.media_id,
            timeline_start: timeline_position,
            source_start: right_source_start,
            source_duration: right_duration,
        },
    );
    Ok(Event::ClipSplit {
        left_id: clip_id,
        right_id: right_clip_id,
    })
}

pub(super) fn delete_clip(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    track.clips.remove(index);
    Ok(Event::ClipDeleted { id: clip_id })
}

pub(super) fn ripple_delete(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
    clip_id: ClipId,
) -> Result<Event, CommandError> {
    let track = find_track_mut(project, sequence_id, track_id)?;
    ensure_track_editable(track)?;
    let index = track
        .clips
        .iter()
        .position(|clip| clip.id == clip_id)
        .ok_or(CommandError::ClipNotFound)?;
    let removed = track.clips.remove(index);
    let mut shifted_count = 0;
    for clip in track
        .clips
        .iter_mut()
        .filter(|clip| clip.timeline_start > removed.timeline_start)
    {
        clip.timeline_start = clip
            .timeline_start
            .checked_sub(removed.source_duration)
            .map_err(|_| CommandError::InvalidTime {
                field: "timeline_start",
                constraint: "representable",
            })?;
        shifted_count += 1;
    }
    Ok(Event::ClipRippleDeleted {
        id: clip_id,
        shifted_count,
    })
}

pub(super) fn ensure_track_editable(track: &Track) -> Result<(), CommandError> {
    if track.locked {
        return Err(CommandError::TrackLocked);
    }
    Ok(())
}

pub(super) fn find_track_mut(
    project: &mut Project,
    sequence_id: SequenceId,
    track_id: TrackId,
) -> Result<&mut Track, CommandError> {
    project
        .sequences
        .get_mut(&sequence_id)
        .ok_or(CommandError::SequenceNotFound)?
        .tracks
        .iter_mut()
        .find(|track| track.id == track_id)
        .ok_or(CommandError::TrackNotFound)
}

pub(super) fn validate_clip_time(
    timeline_start: RationalTime,
    source_start: RationalTime,
    source_duration: RationalTime,
) -> Result<(), CommandError> {
    for (field, value) in [
        ("timeline_start", timeline_start),
        ("source_start", source_start),
    ] {
        if !value.is_non_negative() {
            return Err(CommandError::InvalidTime {
                field,
                constraint: "non-negative",
            });
        }
    }
    if !source_duration.is_positive() {
        return Err(CommandError::InvalidTime {
            field: "source_duration",
            constraint: "positive",
        });
    }
    Ok(())
}

pub(super) fn ensure_range_available(
    track: &Track,
    start: RationalTime,
    duration: RationalTime,
    ignored_clip: Option<ClipId>,
) -> Result<(), CommandError> {
    let end = start
        .checked_add(duration)
        .map_err(|error| CommandError::Commit(error.to_string()))?;
    for clip in &track.clips {
        if ignored_clip == Some(clip.id) {
            continue;
        }
        let clip_end = clip
            .timeline_start
            .checked_add(clip.source_duration)
            .map_err(|error| CommandError::Commit(error.to_string()))?;
        if start < clip_end && clip.timeline_start < end {
            return Err(CommandError::ClipOverlap);
        }
    }
    Ok(())
}
