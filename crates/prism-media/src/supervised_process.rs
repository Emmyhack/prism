//! Bounded output capture with cooperative cancellation of trusted media tools.
use crate::MediaError;
use std::{
    io::Read,
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

/// Deadline and cancellation token for one media subprocess operation.
#[derive(Clone, Copy, Debug)]
pub struct MediaProcessControl<'a> {
    /// Maximum operation duration, including output collection.
    pub timeout: Duration,
    /// Cancellation flag owned by this operation's caller.
    pub cancelled: &'a AtomicBool,
}
struct ManagedChild(Child);
impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn reader(
    input: impl Read + Send + 'static,
    maximum: usize,
    stream: &'static str,
) -> Result<Receiver<Result<Vec<u8>, MediaError>>, MediaError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!("prism-media-{stream}"))
        .spawn(move || {
            let result = (|| {
                let limit = u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1);
                let mut bytes = Vec::with_capacity(maximum.min(8192));
                input.take(limit).read_to_end(&mut bytes)?;
                if bytes.len() > maximum {
                    return Err(MediaError::ProcessOutputLimit { stream, maximum });
                }
                Ok(bytes)
            })();
            let _ = sender.send(result);
        })?;
    Ok(receiver)
}
fn receive(
    receiver: &Receiver<Result<Vec<u8>, MediaError>>,
    destination: &mut Option<Vec<u8>>,
) -> Result<(), MediaError> {
    if destination.is_some() {
        return Ok(());
    }
    match receiver.try_recv() {
        Ok(result) => *destination = Some(result?),
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => {
            return Err(std::io::Error::other("Media output reader stopped unexpectedly").into());
        }
    }
    Ok(())
}

pub(crate) fn capture(
    command: &mut Command,
    maximum_stdout: usize,
    maximum_stderr: usize,
    control: MediaProcessControl<'_>,
) -> Result<Output, MediaError> {
    if control.cancelled.load(Ordering::Acquire) {
        return Err(MediaError::OperationCancelled);
    }
    if control.timeout.is_zero() {
        return Err(MediaError::OperationTimedOut);
    }
    let started = Instant::now();
    let mut child = ManagedChild(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let stdout = reader(
        child
            .0
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("Media stdout unavailable"))?,
        maximum_stdout,
        "stdout",
    )?;
    let stderr = reader(
        child
            .0
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("Media stderr unavailable"))?,
        maximum_stderr,
        "stderr",
    )?;
    let mut captured_stdout = None;
    let mut captured_stderr = None;
    let mut status = None;
    loop {
        if control.cancelled.load(Ordering::Acquire) {
            return Err(MediaError::OperationCancelled);
        }
        if started.elapsed() >= control.timeout {
            return Err(MediaError::OperationTimedOut);
        }
        receive(&stdout, &mut captured_stdout)?;
        receive(&stderr, &mut captured_stderr)?;
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if let Some(status) = status
            && captured_stdout.is_some()
            && captured_stderr.is_some()
        {
            return Ok(Output {
                status,
                stdout: captured_stdout.take().unwrap_or_default(),
                stderr: captured_stderr.take().unwrap_or_default(),
            });
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn capture_preserves_output_and_caps_overproduction() -> Result<(), Box<dyn std::error::Error>>
    {
        let cancelled = AtomicBool::new(false);
        let control = MediaProcessControl {
            timeout: Duration::from_secs(3),
            cancelled: &cancelled,
        };
        let output = capture(Command::new("/bin/echo").arg("media"), 64, 64, control)?;
        assert!(output.status.success());
        assert_eq!(output.stdout, b"media\n");
        assert!(matches!(
            capture(&mut Command::new("/usr/bin/yes"), 1024, 1024, control),
            Err(MediaError::ProcessOutputLimit {
                stream: "stdout",
                ..
            })
        ));
        Ok(())
    }
    #[test]
    fn cancellation_and_deadline_reap_the_tool_process() -> Result<(), Box<dyn std::error::Error>> {
        for cancel_during_run in [false, true] {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("prism-probe-pid-{}-{nonce}", std::process::id()));
            let cancelled = Arc::new(AtomicBool::new(false));
            let signal = cancelled.clone();
            let pid_path = path.clone();
            let worker = thread::spawn(move || {
                let mut command = Command::new("/bin/sh");
                command
                    .args(["-c", "echo $$ > \"$1\"; exec /bin/sleep 30", "prism-test"])
                    .arg(pid_path);
                capture(
                    &mut command,
                    1024,
                    1024,
                    MediaProcessControl {
                        timeout: if cancel_during_run {
                            Duration::from_secs(5)
                        } else {
                            Duration::from_secs(1)
                        },
                        cancelled: &signal,
                    },
                )
            });
            let started = Instant::now();
            while !path.exists() && started.elapsed() < Duration::from_secs(2) {
                thread::sleep(Duration::from_millis(5));
            }
            if cancel_during_run {
                cancelled.store(true, Ordering::Release);
            }
            let result = worker
                .join()
                .map_err(|_| "Media supervisor test worker failed")?;
            assert!(if cancel_during_run {
                matches!(result, Err(MediaError::OperationCancelled))
            } else {
                matches!(result, Err(MediaError::OperationTimedOut))
            });
            let pid = std::fs::read_to_string(&path)?;
            assert!(
                !Command::new("/bin/kill")
                    .args(["-0", pid.trim()])
                    .stderr(Stdio::null())
                    .status()?
                    .success()
            );
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
    #[test]
    fn pre_cancelled_operations_never_spawn() {
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            capture(
                &mut Command::new("/nonexistent/media-tool"),
                1,
                1,
                MediaProcessControl {
                    timeout: Duration::from_secs(1),
                    cancelled: &cancelled
                }
            ),
            Err(MediaError::OperationCancelled)
        ));
    }
}
