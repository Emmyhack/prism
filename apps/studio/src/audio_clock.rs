//! Coarse `FFplay` audio-master observations; not a sample-accurate native device clock.
use prism_timeline::RationalTime;
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicI64, AtomicU64, Ordering},
    },
};

#[derive(Clone, Debug)]
pub(crate) struct AudioClock {
    milliseconds: Arc<AtomicI64>,
    last_advance: Arc<AtomicU64>,
    started: std::time::Instant,
}
impl Default for AudioClock {
    fn default() -> Self {
        Self {
            milliseconds: Arc::new(AtomicI64::new(-1)),
            last_advance: Arc::new(AtomicU64::new(0)),
            started: std::time::Instant::now(),
        }
    }
}
impl AudioClock {
    pub fn position(&self) -> Option<RationalTime> {
        let milliseconds = self.milliseconds.load(Ordering::Acquire);
        if milliseconds < 0 {
            None
        } else {
            RationalTime::new(milliseconds, 1000).ok()
        }
    }
    pub fn stalled(&self) -> bool {
        let elapsed = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.position().is_some()
            && elapsed.saturating_sub(self.last_advance.load(Ordering::Acquire)) > 2000
    }
    pub fn observe(&self, line: &[u8]) {
        if let Some(milliseconds) = parse_position(line) {
            let previous = self.milliseconds.fetch_max(milliseconds, Ordering::AcqRel);
            // A live status stream with a frozen clock is still a playback stall.
            if milliseconds > previous {
                self.last_advance.store(
                    u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    Ordering::Release,
                );
            }
        }
    }
}
fn parse_position(line: &[u8]) -> Option<i64> {
    let line = std::str::from_utf8(line).ok()?;
    if !line.contains(" M-A:") || !line.contains("aq=") {
        return None;
    }
    let seconds = line.split_ascii_whitespace().next()?.parse::<f64>().ok()?;
    #[allow(clippy::cast_precision_loss)]
    let maximum_seconds = i64::MAX as f64 / 1000.0;
    if !seconds.is_finite() || seconds < 0.0 || seconds >= maximum_seconds {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    Some((seconds * 1000.0).round() as i64)
}
/// Read both carriage-return status updates and newline diagnostics, with bounded lines.
pub(crate) fn read_status(mut reader: impl Read, clock: &AudioClock) {
    let mut chunk = [0_u8; 512];
    let mut line = Vec::with_capacity(4096);
    let mut discarded = false;
    loop {
        let count = match reader.read(&mut chunk) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        for byte in &chunk[..count] {
            if *byte == b'\r' || *byte == b'\n' {
                if !discarded {
                    clock.observe(&line);
                }
                line.clear();
                discarded = false;
            } else if !discarded {
                if line.len() < 4096 {
                    line.push(*byte);
                } else {
                    line.clear();
                    discarded = true;
                }
            }
        }
    }
    if !discarded {
        clock.observe(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_or_regressing_reports_do_not_hide_a_stalled_clock() {
        let mut clock = AudioClock::default();
        clock.observe(b"0.50 M-A: 0 aq=4KB");
        let Some(started) =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(3))
        else {
            return;
        };
        clock.started = started;
        clock.observe(b"0.50 M-A: 0 aq=4KB");
        assert!(clock.stalled());
        clock.observe(b"0.49 M-A: 0 aq=4KB");
        assert!(clock.stalled());
        clock.observe(b"0.51 M-A: 0 aq=4KB");
        assert!(!clock.stalled());
    }

    #[test]
    fn status_clock_is_monotonic_and_ignores_metadata_and_invalid_times() {
        let clock = AudioClock::default();
        assert!(clock.position().is_none());
        read_status(std::io::Cursor::new(b"Input #0: wav\n nan M-A: nan aq=0KB\r -0.03 M-A: 0 aq=0KB\r 0.12 M-A: 0 aq=2KB\r 0.09 M-A: 0 aq=2KB\r"), &clock);
        assert_eq!(clock.position(), RationalTime::new(120, 1000).ok());
        clock.observe(b"inf M-A: 0 aq=0KB");
        clock.observe(b"42 encoder metadata");
        assert_eq!(clock.position(), RationalTime::new(120, 1000).ok());
        let mut long_line = vec![b'x'; 8192];
        long_line.extend_from_slice(b"\r 0.23 M-A: 0 aq=0KB\r");
        read_status(std::io::Cursor::new(long_line), &clock);
        assert_eq!(clock.position(), RationalTime::new(230, 1000).ok());
        assert!(AudioClock::default().position().is_none());
        let Some(started) =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(3))
        else {
            return;
        };
        let stalled = AudioClock {
            started,
            ..AudioClock::default()
        };
        stalled.milliseconds.store(120, Ordering::Release);
        assert!(stalled.stalled());
        stalled.observe(b"0.13 M-A: 0 aq=2KB");
        assert!(!stalled.stalled());
    }
}
