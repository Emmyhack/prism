//! Bounded continuous video preview. Still-frame scrubbing uses the reference decoder.
use prism_media::{DecodedVideoFrame, FfprobeInspector, MediaProcessControl, StreamKind};
use prism_timeline::{RationalTime, RoundingMode};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[derive(Clone, Debug, Default)]
pub(crate) struct StreamControl {
    active: Arc<AtomicBool>,
    epoch: Arc<AtomicU64>,
    child: Arc<Mutex<Option<Child>>>,
    cancellation: Arc<Mutex<Arc<AtomicBool>>>,
}
impl StreamControl {
    pub fn request_token(&self) -> Arc<AtomicBool> {
        let Ok(mut token) = self.cancellation.lock() else {
            return Arc::new(AtomicBool::new(true));
        };
        if token.load(Ordering::Acquire) {
            *token = Arc::new(AtomicBool::new(false));
        }
        token.clone()
    }
    pub fn activate(&self) {
        self.request_token();
        self.active.store(true, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
    pub fn stop(&self) {
        self.active.store(false, Ordering::Release);
        if let Ok(token) = self.cancellation.lock() {
            token.store(true, Ordering::Release);
        }
        self.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    fn install(&self, mut child: Child, epoch: u64) -> Result<(), String> {
        let Ok(mut slot) = self.child.lock() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Video decoder control unavailable".into());
        };
        if !self.active() || epoch != self.epoch.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Video playback stopped".into());
        }
        if let Some(mut previous) = slot.take() {
            let _ = previous.kill();
            let _ = previous.wait();
        }
        *slot = Some(child);
        Ok(())
    }
    fn release(&self, pid: u32) {
        if let Ok(mut slot) = self.child.lock() {
            if slot.as_ref().map(Child::id) != Some(pid) {
                return;
            }
            if let Some(mut child) = slot.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    #[cfg(test)]
    fn pid(&self) -> Option<u32> {
        self.child.lock().ok()?.as_ref().map(Child::id)
    }
}

pub(crate) struct StreamPreview {
    path: PathBuf,
    base: RationalTime,
    duration: RationalTime,
    width: u32,
    height: u32,
    next_index: usize,
    epoch: u64,
    last: Option<DecodedVideoFrame>,
    stdout: ChildStdout,
    pid: u32,
    control: StreamControl,
}

fn preview_size(width: u32, height: u32) -> Result<(u32, u32), String> {
    if width == 0 || height == 0 {
        return Err("Invalid source dimensions".into());
    }
    let scale = (960.0 / f64::from(width))
        .min(540.0 / f64::from(height))
        .min(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok((
        (f64::from(width) * scale).floor().max(1.0) as u32,
        (f64::from(height) * scale).floor().max(1.0) as u32,
    ))
}
impl StreamPreview {
    pub fn open(
        path: &Path,
        base: RationalTime,
        ffmpeg: &Path,
        ffprobe: &Path,
        control: StreamControl,
        cancelled: &AtomicBool,
    ) -> Result<Self, String> {
        if !control.active() || cancelled.load(Ordering::Acquire) {
            return Err("Video playback stopped".into());
        }
        let epoch = control.epoch.load(Ordering::Acquire);
        let metadata = FfprobeInspector::new(ffprobe)
            .inspect_with_control(
                path,
                MediaProcessControl {
                    timeout: std::time::Duration::from_secs(5),
                    cancelled,
                },
            )
            .map_err(|error| error.to_string())?;
        let (width, height) = metadata
            .streams
            .iter()
            .find_map(|stream| match &stream.kind {
                StreamKind::Video(video) => Some((video.width?, video.height?)),
                _ => None,
            })
            .ok_or("No video dimensions available")?;
        let (width, height) = preview_size(width, height)?;
        if cancelled.load(Ordering::Acquire) {
            return Err("Video playback stopped".into());
        }
        let mut child = Command::new(ffmpeg)
            .args(["-nostdin", "-v", "error", "-ss"])
            .arg(format!("{:.9}", base.as_seconds_f64()))
            .arg("-i")
            .arg(path)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-dn",
                "-threads",
                "1",
                "-filter_threads",
                "1",
                "-vf",
            ])
            .arg(format!("fps=30,scale={width}:{height}"))
            .args(["-pix_fmt", "rgba", "-f", "rawvideo", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not start continuous video preview: {error}"))?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Video decoder output unavailable".into());
        };
        let pid = child.id();
        control.install(child, epoch)?;
        Ok(Self {
            path: path.to_path_buf(),
            base,
            duration: metadata.duration.unwrap_or(RationalTime::ZERO),
            width,
            height,
            next_index: 0,
            epoch,
            last: None,
            stdout,
            pid,
            control,
        })
    }
    fn index(&self, time: RationalTime) -> Option<usize> {
        if time < self.base {
            return None;
        }
        usize::try_from(
            time.checked_sub(self.base)
                .ok()?
                .to_ticks(30, RoundingMode::Floor)
                .ok()?,
        )
        .ok()
    }
    pub fn reusable(&self, path: &Path, time: RationalTime) -> bool {
        self.control.active()
            && self.epoch == self.control.epoch.load(Ordering::Acquire)
            && self.path == path
            && self.index(time).is_some_and(|index| {
                index.saturating_add(1) >= self.next_index
                    && index < self.next_index.saturating_add(30)
            })
    }
    pub fn frame(
        &mut self,
        time: RationalTime,
    ) -> Result<(DecodedVideoFrame, RationalTime), String> {
        if !self.control.active() || self.epoch != self.control.epoch.load(Ordering::Acquire) {
            return Err("Video playback session stopped".into());
        }
        let target = self
            .index(time)
            .ok_or("Video seek precedes decoder session")?;
        if target >= self.next_index.saturating_add(30) {
            return Err("Video seek exceeds bounded preview window".into());
        }
        while self.next_index <= target {
            if !self.control.active() {
                return Err("Video playback stopped".into());
            }
            let bytes = usize::try_from(u64::from(self.width) * u64::from(self.height) * 4)
                .map_err(|error| error.to_string())?;
            let mut rgba = vec![0; bytes];
            self.stdout
                .read_exact(&mut rgba)
                .map_err(|error| format!("Continuous video decode ended: {error}"))?;
            self.last = Some(DecodedVideoFrame {
                width: self.width,
                height: self.height,
                rgba,
            });
            self.next_index += 1;
        }
        self.last
            .clone()
            .map(|frame| (frame, self.duration))
            .ok_or_else(|| "No preview frame decoded".into())
    }
}
impl Drop for StreamPreview {
    fn drop(&mut self) {
        self.control.release(self.pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restarting_does_not_reactivate_a_cancelled_request() {
        let control = StreamControl::default();
        let first = control.request_token();
        control.stop();
        control.activate();
        let second = control.request_token();
        assert!(first.load(Ordering::Acquire));
        assert!(!second.load(Ordering::Acquire));
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(
            StreamPreview::open(
                Path::new("source.mp4"),
                RationalTime::ZERO,
                Path::new("/nonexistent/ffmpeg"),
                Path::new("/nonexistent/ffprobe"),
                control,
                &first
            )
            .err()
            .as_deref(),
            Some("Video playback stopped")
        );
    }

    #[test]
    fn preview_dimensions_are_bounded_and_preserve_portrait_sources() -> Result<(), String> {
        assert_eq!(preview_size(3840, 2160)?, (960, 540));
        assert_eq!(preview_size(1080, 1920)?, (303, 540));
        assert_eq!(preview_size(320, 180)?, (320, 180));
        assert!(preview_size(0, 1080).is_err());
        Ok(())
    }
    #[test]
    #[ignore = "requires PRISM_FFMPEG, PRISM_FFPROBE, and the generated media fixture"]
    fn continuous_decode_reuses_process_and_stops_it() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var_os("PRISM_TEST_FFMPEG").is_none() {
            return Ok(());
        }
        let started = std::time::Instant::now();
        let control = StreamControl::default();
        control.activate();
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/media/av-ntsc-48k.mp4");
        let ffmpeg =
            std::env::var_os("PRISM_FFMPEG").map_or_else(|| PathBuf::from("ffmpeg"), PathBuf::from);
        let ffprobe = std::env::var_os("PRISM_FFPROBE")
            .map_or_else(|| PathBuf::from("ffprobe"), PathBuf::from);
        let mut preview = StreamPreview::open(
            &path,
            RationalTime::ZERO,
            &ffmpeg,
            &ffprobe,
            control.clone(),
            &control.request_token(),
        )?;
        let pid = control.pid().ok_or("Missing decoder process")?;
        let (first, _) = preview.frame(RationalTime::ZERO)?;
        let position = RationalTime::new(1, 2)?;
        assert!(preview.reusable(&path, position));
        let (later, _) = preview.frame(position)?;
        assert_eq!(control.pid(), Some(pid));
        assert_ne!(first.rgba, later.rgba);
        assert!(!preview.reusable(&path, RationalTime::ZERO));
        assert!(!preview.reusable(Path::new("other.mp4"), position));
        assert!(!preview.reusable(&path, RationalTime::new(5, 1)?));
        control.stop();
        assert!(control.pid().is_none());
        control.activate();
        assert!(!preview.reusable(&path, position));
        assert!(preview.frame(RationalTime::new(3, 4)?).is_err());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "Continuous video fixture exceeded the 10-second acceptance budget"
        );
        #[cfg(unix)]
        assert!(
            !Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(Stdio::null())
                .status()?
                .success()
        );
        Ok(())
    }
}
