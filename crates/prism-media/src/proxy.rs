use super::{
    AtomicBool, Child, Command, Duration, MediaError, Ordering, Path, PathBuf, Read, Stdio,
    fingerprint, fs, thread,
};

/// Full-file SHA-256 fingerprint for relinking and change detection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaFingerprint {
    /// Lowercase hexadecimal SHA-256 digest.
    pub sha256: String,
    /// Number of bytes hashed.
    pub byte_length: u64,
}

impl MediaFingerprint {
    /// Returns whether two fingerprints describe identical source bytes.
    #[must_use]
    pub fn same_content(&self, other: &Self) -> bool {
        self.byte_length == other.byte_length && self.sha256 == other.sha256
    }
}

/// Deterministic proxy-generation settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyProfile {
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
    /// Video codec identifier passed to the encoder.
    pub video_codec: String,
    /// Optional audio codec identifier.
    pub audio_codec: Option<String>,
    /// Cache artifact extension.
    pub extension: String,
}

/// A nondestructive proxy-generation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProxyRequest {
    /// Original media source.
    pub source: PathBuf,
    /// No-clobber proxy destination.
    pub output: PathBuf,
    /// Encoding profile.
    pub profile: ProxyProfile,
}

/// Source variant selected for preview or proxy playback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaSourceSelection {
    /// Use the original source media.
    Original,
    /// Use a relinked proxy source.
    Proxy,
}

/// Current validity of a relinked proxy against its source media.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyStatus {
    /// No proxy path is configured or the configured file is missing.
    Missing,
    /// The proxy exists and has the same fingerprint as the source.
    Available,
    /// The proxy exists but no longer matches the source fingerprint.
    Stale,
}

/// Selects a source path without silently falling back from proxy mode.
///
/// # Errors
///
/// Returns [`MediaError::ProxyUnavailable`] when proxy mode is requested but
/// no proxy path has been provided.
pub fn select_media_source<'a>(
    original: &'a Path,
    proxy: Option<&'a Path>,
    selection: MediaSourceSelection,
) -> Result<&'a Path, MediaError> {
    match selection {
        MediaSourceSelection::Original => Ok(original),
        MediaSourceSelection::Proxy => proxy.ok_or(MediaError::ProxyUnavailable),
    }
}

/// Compares a configured proxy with its source fingerprint.
///
/// # Errors
///
/// Returns [`MediaError::InvalidProxySource`] when the source is not a regular
/// file, or an I/O error while fingerprinting either regular file.
pub fn proxy_status(
    source: &Path,
    proxy: Option<&Path>,
    recorded_source: Option<&MediaFingerprint>,
) -> Result<ProxyStatus, MediaError> {
    if !source.is_file() {
        return Err(MediaError::InvalidProxySource(source.to_path_buf()));
    }
    let Some(proxy) = proxy else {
        return Ok(ProxyStatus::Missing);
    };
    if !proxy.is_file() {
        return Ok(ProxyStatus::Missing);
    }
    let Some(recorded_source) = recorded_source else {
        return Ok(ProxyStatus::Stale);
    };
    Ok(if fingerprint(source)?.same_content(recorded_source) {
        ProxyStatus::Available
    } else {
        ProxyStatus::Stale
    })
}

/// Explicit `FFmpeg` proxy generator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FfmpegProxyGenerator {
    executable: PathBuf,
}

impl FfmpegProxyGenerator {
    /// Creates a generator using an explicitly configured `FFmpeg` executable.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// Generates a proxy with temporary-output cleanup and no-clobber finalization.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when validation, process execution, cancellation,
    /// or finalization fails.
    pub fn generate(
        &self,
        request: &ProxyRequest,
        cancelled: Option<&AtomicBool>,
    ) -> Result<PathBuf, MediaError> {
        if !request.source.is_file() {
            return Err(MediaError::InvalidProxySource(request.source.clone()));
        }
        if request.output.exists() {
            return Err(MediaError::ProxyDestinationExists(request.output.clone()));
        }
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return Err(MediaError::ProxyCancelled);
        }
        let parent = request
            .output
            .parent()
            .ok_or_else(|| MediaError::InvalidProxyOutputParent(request.output.clone()))?;
        if !parent.is_dir() {
            return Err(MediaError::InvalidProxyOutputParent(parent.to_path_buf()));
        }
        let temporary = request.output.with_file_name(format!(
            ".{}.proxy-{}.tmp",
            request
                .output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("proxy"),
            std::process::id()
        ));
        let mut command = Command::new(&self.executable);
        command
            .args(["-v", "error", "-y", "-i"])
            .arg(&request.source)
            .args([
                "-vf",
                &format!("scale={}:{}", request.profile.width, request.profile.height),
                "-c:v",
            ])
            .arg(&request.profile.video_codec);
        if let Some(audio) = &request.profile.audio_codec {
            command.args(["-c:a", audio]);
        } else {
            command.arg("-an");
        }
        command
            .arg(&temporary)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| MediaError::ProxyEncoder {
            status: -1,
            stderr: error.to_string(),
        })?;
        let output = wait_proxy_child(&mut child, cancelled)?;
        if !output.status.success() {
            let _ = fs::remove_file(&temporary);
            return Err(MediaError::ProxyEncoder {
                status: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if !temporary.is_file() {
            return Err(MediaError::ProxyOutputMissing);
        }
        let result = fs::hard_link(&temporary, &request.output);
        let _ = fs::remove_file(&temporary);
        result.map_err(|error| {
            if request.output.exists() {
                MediaError::ProxyDestinationExists(request.output.clone())
            } else {
                MediaError::ProxyEncoder {
                    status: -1,
                    stderr: error.to_string(),
                }
            }
        })?;
        Ok(request.output.clone())
    }
}

fn wait_proxy_child(
    child: &mut Child,
    cancelled: Option<&AtomicBool>,
) -> Result<std::process::Output, MediaError> {
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MediaError::ProxyCancelled);
        }
        if child
            .try_wait()
            .map_err(|error| MediaError::ProxyEncoder {
                status: -1,
                stderr: error.to_string(),
            })?
            .is_some()
        {
            let status = child.wait().map_err(|error| MediaError::ProxyEncoder {
                status: -1,
                stderr: error.to_string(),
            })?;
            let mut stderr = Vec::new();
            if let Some(mut stream) = child.stderr.take() {
                stream
                    .read_to_end(&mut stderr)
                    .map_err(|error| MediaError::ProxyEncoder {
                        status: -1,
                        stderr: error.to_string(),
                    })?;
            }
            return Ok(std::process::Output {
                status,
                stdout: Vec::new(),
                stderr,
            });
        }
        thread::sleep(Duration::from_millis(10));
    }
}

impl ProxyProfile {
    /// Creates a validated proxy profile.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError::InvalidProxyProfile`] when dimensions or codec
    /// identifiers are empty/unsafe.
    pub fn new(
        width: u32,
        height: u32,
        video_codec: impl Into<String>,
        audio_codec: Option<String>,
        extension: impl Into<String>,
    ) -> Result<Self, MediaError> {
        let profile = Self {
            width,
            height,
            video_codec: video_codec.into(),
            audio_codec,
            extension: extension.into(),
        };
        if profile.width == 0 || profile.height == 0 {
            return Err(MediaError::InvalidProxyProfile {
                reason: "dimensions must be nonzero",
            });
        }
        for value in std::iter::once(&profile.video_codec)
            .chain(profile.audio_codec.as_ref())
            .chain(std::iter::once(&profile.extension))
        {
            if value.is_empty()
                || !value.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
            {
                return Err(MediaError::InvalidProxyProfile {
                    reason: "codec and extension identifiers must be filename-safe",
                });
            }
        }
        Ok(profile)
    }

    /// Returns canonical bytes for cache-key derivation.
    #[must_use]
    pub fn cache_parameters(&self) -> Vec<u8> {
        format!(
            "{}x{};v={};a={};ext={}",
            self.width,
            self.height,
            self.video_codec,
            self.audio_codec.as_deref().unwrap_or(""),
            self.extension
        )
        .into_bytes()
    }
}
