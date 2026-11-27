use super::*;

pub(super) fn import_media_into_project(
    project: &mut Project,
    path: &Path,
) -> Result<UndoRecord, String> {
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
