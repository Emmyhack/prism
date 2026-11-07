//! Typed production codec policy independent of encoder process invocation.

use std::collections::BTreeSet;
use thiserror::Error;

/// Supported output container families.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ContainerFormat {
    /// ISO base media container, optimized for broad delivery compatibility.
    Mp4,
    /// `QuickTime` container, including professional intermediate codecs.
    Mov,
    /// Open web-media container.
    WebM,
}

impl ContainerFormat {
    /// Returns the `FFmpeg` muxer name for this container family.
    #[must_use]
    pub const fn ffmpeg_muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::WebM => "webm",
        }
    }
}

/// Video codec families selectable by export policy.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum VideoCodec {
    /// AVC/H.264 delivery codec.
    H264,
    /// HEVC/H.265 delivery codec.
    H265,
    /// Apple `ProRes` intermediate codec family.
    ProRes,
    /// Google VP9 web delivery codec.
    Vp9,
    /// `AOMedia` AV1 delivery codec.
    Av1,
}

impl VideoCodec {
    /// Returns the conventional `FFmpeg` encoder name for this codec family.
    #[must_use]
    pub const fn ffmpeg_encoder(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::ProRes => "prores_ks",
            Self::Vp9 => "libvpx-vp9",
            Self::Av1 => "libaom-av1",
        }
    }
}

/// Explicit video bitstream/profile choices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoProfile {
    /// H.264 Baseline.
    H264Baseline,
    /// H.264 Main.
    H264Main,
    /// H.264 High.
    H264High,
    /// HEVC Main 8-bit.
    H265Main,
    /// HEVC Main 10-bit.
    H265Main10,
    /// `ProRes` Proxy.
    ProResProxy,
    /// `ProRes` 422.
    ProRes422,
    /// `ProRes` 422 HQ.
    ProRes422Hq,
    /// `ProRes` 4444 with alpha capability.
    ProRes4444,
    /// VP9 profile 0, 8-bit 4:2:0.
    Vp9Profile0,
    /// VP9 profile 2, up to 10/12-bit.
    Vp9Profile2,
    /// AV1 Main profile.
    Av1Main,
}

impl VideoProfile {
    const fn codec(self) -> VideoCodec {
        match self {
            Self::H264Baseline | Self::H264Main | Self::H264High => VideoCodec::H264,
            Self::H265Main | Self::H265Main10 => VideoCodec::H265,
            Self::ProResProxy | Self::ProRes422 | Self::ProRes422Hq | Self::ProRes4444 => {
                VideoCodec::ProRes
            }
            Self::Vp9Profile0 | Self::Vp9Profile2 => VideoCodec::Vp9,
            Self::Av1Main => VideoCodec::Av1,
        }
    }

    /// Returns the `FFmpeg` profile/pixel-format hint for this profile.
    #[must_use]
    pub const fn ffmpeg_profile(self) -> &'static str {
        match self {
            Self::H264Baseline => "baseline",
            Self::H264Main | Self::H265Main | Self::Av1Main => "main",
            Self::H264High => "high",
            Self::H265Main10 => "main10",
            Self::ProResProxy => "proxy",
            Self::ProRes422 => "standard",
            Self::ProRes422Hq => "hq",
            Self::ProRes4444 => "4444",
            Self::Vp9Profile0 => "0",
            Self::Vp9Profile2 => "2",
        }
    }
}

/// Audio codec families selectable by export policy.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum AudioCodec {
    /// Advanced Audio Coding delivery codec.
    Aac,
    /// Uncompressed linear PCM.
    Pcm,
    /// Opus web delivery codec.
    Opus,
}

impl AudioCodec {
    /// Returns the conventional `FFmpeg` encoder name for this codec family.
    #[must_use]
    pub const fn ffmpeg_encoder(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Pcm => "pcm_s24le",
            Self::Opus => "libopus",
        }
    }
}

/// Encoder and muxer capabilities observed in the current runtime.
///
/// This value is intentionally supplied by an adapter. Policy validation never
/// assumes that a codec compiled into one `FFmpeg` distribution exists in another.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CodecCapabilities {
    containers: BTreeSet<ContainerFormat>,
    video_codecs: BTreeSet<VideoCodec>,
    audio_codecs: BTreeSet<AudioCodec>,
}

impl CodecCapabilities {
    /// Constructs an explicit availability snapshot.
    #[must_use]
    pub fn new(
        containers: impl IntoIterator<Item = ContainerFormat>,
        video_codecs: impl IntoIterator<Item = VideoCodec>,
        audio_codecs: impl IntoIterator<Item = AudioCodec>,
    ) -> Self {
        Self {
            containers: containers.into_iter().collect(),
            video_codecs: video_codecs.into_iter().collect(),
            audio_codecs: audio_codecs.into_iter().collect(),
        }
    }

    /// Builds a capability snapshot from the names reported by an encoder and
    /// muxer adapter (for example, `FFmpeg`'s `-encoders`/`-muxers` output).
    /// Unknown names are ignored so newer runtime builds remain forward
    /// compatible with this policy layer.
    #[must_use]
    pub fn from_runtime_names(
        containers: impl IntoIterator<Item = impl AsRef<str>>,
        video_encoders: impl IntoIterator<Item = impl AsRef<str>>,
        audio_encoders: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Self {
        let containers = containers.into_iter().filter_map(|name| {
            match name.as_ref().to_ascii_lowercase().as_str() {
                "mp4" | "mov,mp4,m4a,3gp,3g2,mj2" => Some(ContainerFormat::Mp4),
                "mov" | "3g2" => Some(ContainerFormat::Mov),
                "webm" => Some(ContainerFormat::WebM),
                _ => None,
            }
        });
        let video_codecs = video_encoders.into_iter().filter_map(|name| {
            let name = name.as_ref().to_ascii_lowercase();
            if name.contains("264") {
                Some(VideoCodec::H264)
            } else if name.contains("265") || name.contains("hevc") {
                Some(VideoCodec::H265)
            } else if name.contains("prores") {
                Some(VideoCodec::ProRes)
            } else if name.contains("vp9") {
                Some(VideoCodec::Vp9)
            } else if name.contains("av1") || name.contains("aom") {
                Some(VideoCodec::Av1)
            } else {
                None
            }
        });
        let audio_codecs = audio_encoders.into_iter().filter_map(|name| {
            let name = name.as_ref().to_ascii_lowercase();
            if name.contains("aac") {
                Some(AudioCodec::Aac)
            } else if name.contains("pcm") {
                Some(AudioCodec::Pcm)
            } else if name.contains("opus") {
                Some(AudioCodec::Opus)
            } else {
                None
            }
        });
        Self::new(containers, video_codecs, audio_codecs)
    }

    /// Returns whether the container muxer was observed.
    #[must_use]
    pub fn supports_container(&self, container: ContainerFormat) -> bool {
        self.containers.contains(&container)
    }

    /// Returns whether a video encoder was observed.
    #[must_use]
    pub fn supports_video(&self, codec: VideoCodec) -> bool {
        self.video_codecs.contains(&codec)
    }

    /// Returns whether an audio encoder was observed.
    #[must_use]
    pub fn supports_audio(&self, codec: AudioCodec) -> bool {
        self.audio_codecs.contains(&codec)
    }
}

/// Requested production codec/container policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExportCodecPolicy {
    /// Output container.
    pub container: ContainerFormat,
    /// Video encoder family.
    pub video_codec: VideoCodec,
    /// Explicit codec profile.
    pub video_profile: VideoProfile,
    /// Optional audio encoder; `None` creates a video-only output.
    pub audio_codec: Option<AudioCodec>,
}

/// A policy proven structurally compatible and available in this runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedCodecPolicy(ExportCodecPolicy);

impl ResolvedCodecPolicy {
    /// Returns the validated policy fields.
    #[must_use]
    pub const fn policy(self) -> ExportCodecPolicy {
        self.0
    }
}

/// Structured codec compatibility and runtime availability failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CodecPolicyError {
    /// Profile belongs to a different codec family.
    #[error("the selected video profile does not belong to the selected video codec")]
    ProfileCodecMismatch,
    /// Container specifications do not support this video codec.
    #[error("the selected container does not support the selected video codec")]
    UnsupportedVideoContainer,
    /// Container specifications do not support this audio codec.
    #[error("the selected container does not support the selected audio codec")]
    UnsupportedAudioContainer,
    /// The runtime has no requested output muxer.
    #[error("the selected container muxer is unavailable")]
    ContainerUnavailable,
    /// The runtime has no encoder for the requested video codec.
    #[error("the selected video encoder is unavailable")]
    VideoEncoderUnavailable,
    /// The runtime has no encoder for the requested audio codec.
    #[error("the selected audio encoder is unavailable")]
    AudioEncoderUnavailable,
}

impl ExportCodecPolicy {
    /// Builds deterministic `FFmpeg` encoder and muxer arguments for this policy.
    /// The caller must resolve the policy against runtime capabilities first.
    #[must_use]
    pub fn ffmpeg_arguments(self) -> Vec<String> {
        let mut arguments = vec![
            "-c:v".to_owned(),
            self.video_codec.ffmpeg_encoder().to_owned(),
            "-profile:v".to_owned(),
            self.video_profile.ffmpeg_profile().to_owned(),
        ];
        if let Some(audio) = self.audio_codec {
            arguments.extend(["-c:a".to_owned(), audio.ffmpeg_encoder().to_owned()]);
        }
        arguments.extend(["-f".to_owned(), self.container.ffmpeg_muxer().to_owned()]);
        arguments
    }

    /// Validates specification compatibility and observed runtime availability.
    ///
    /// Structural errors are reported before runtime availability errors so an
    /// invalid combination cannot appear valid merely because encoders exist.
    ///
    /// # Errors
    ///
    /// Returns [`CodecPolicyError`] for mismatched profiles, unsupported
    /// container/codec combinations, or unavailable muxers/encoders.
    pub fn resolve(
        self,
        capabilities: &CodecCapabilities,
    ) -> Result<ResolvedCodecPolicy, CodecPolicyError> {
        if self.video_profile.codec() != self.video_codec {
            return Err(CodecPolicyError::ProfileCodecMismatch);
        }
        if !container_supports_video(self.container, self.video_codec) {
            return Err(CodecPolicyError::UnsupportedVideoContainer);
        }
        if self
            .audio_codec
            .is_some_and(|codec| !container_supports_audio(self.container, codec))
        {
            return Err(CodecPolicyError::UnsupportedAudioContainer);
        }
        if !capabilities.supports_container(self.container) {
            return Err(CodecPolicyError::ContainerUnavailable);
        }
        if !capabilities.supports_video(self.video_codec) {
            return Err(CodecPolicyError::VideoEncoderUnavailable);
        }
        if self
            .audio_codec
            .is_some_and(|codec| !capabilities.supports_audio(codec))
        {
            return Err(CodecPolicyError::AudioEncoderUnavailable);
        }
        Ok(ResolvedCodecPolicy(self))
    }
}

const fn container_supports_video(container: ContainerFormat, codec: VideoCodec) -> bool {
    matches!(
        (container, codec),
        (
            ContainerFormat::Mp4,
            VideoCodec::H264 | VideoCodec::H265 | VideoCodec::Av1
        ) | (
            ContainerFormat::Mov,
            VideoCodec::H264 | VideoCodec::H265 | VideoCodec::ProRes
        ) | (ContainerFormat::WebM, VideoCodec::Vp9 | VideoCodec::Av1)
    )
}

const fn container_supports_audio(container: ContainerFormat, codec: AudioCodec) -> bool {
    matches!(
        (container, codec),
        (ContainerFormat::Mp4, AudioCodec::Aac)
            | (ContainerFormat::Mov, AudioCodec::Aac | AudioCodec::Pcm)
            | (ContainerFormat::WebM, AudioCodec::Opus)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        AudioCodec, CodecCapabilities, CodecPolicyError, ContainerFormat, ExportCodecPolicy,
        ResolvedCodecPolicy, VideoCodec, VideoProfile,
    };

    fn production_capabilities() -> CodecCapabilities {
        CodecCapabilities::new(
            [
                ContainerFormat::Mp4,
                ContainerFormat::Mov,
                ContainerFormat::WebM,
            ],
            [
                VideoCodec::H264,
                VideoCodec::H265,
                VideoCodec::ProRes,
                VideoCodec::Vp9,
            ],
            [AudioCodec::Aac, AudioCodec::Pcm, AudioCodec::Opus],
        )
    }

    #[test]
    fn discovers_known_runtime_names_and_ignores_unknown_entries() {
        let capabilities = CodecCapabilities::from_runtime_names(
            ["mp4", "webm", "mystery"],
            [
                "libx264",
                "hevc_nvenc",
                "prores_ks",
                "libvpx-vp9",
                "libaom-av1",
            ],
            ["aac", "pcm_s24le", "libopus", "unknown"],
        );
        assert!(capabilities.supports_container(ContainerFormat::Mp4));
        assert!(capabilities.supports_container(ContainerFormat::WebM));
        assert!(capabilities.supports_video(VideoCodec::H264));
        assert!(capabilities.supports_video(VideoCodec::H265));
        assert!(capabilities.supports_video(VideoCodec::ProRes));
        assert!(capabilities.supports_video(VideoCodec::Vp9));
        assert!(capabilities.supports_video(VideoCodec::Av1));
        assert!(capabilities.supports_audio(AudioCodec::Aac));
        assert!(capabilities.supports_audio(AudioCodec::Pcm));
        assert!(capabilities.supports_audio(AudioCodec::Opus));
    }

    #[test]
    fn maps_resolved_policy_to_deterministic_ffmpeg_arguments() {
        let policy = ExportCodecPolicy {
            container: ContainerFormat::Mov,
            video_codec: VideoCodec::ProRes,
            video_profile: VideoProfile::ProRes422Hq,
            audio_codec: Some(AudioCodec::Pcm),
        };
        assert_eq!(
            policy.ffmpeg_arguments(),
            [
                "-c:v",
                "prores_ks",
                "-profile:v",
                "hq",
                "-c:a",
                "pcm_s24le",
                "-f",
                "mov"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn resolves_available_delivery_and_intermediate_policies() {
        let capabilities = production_capabilities();
        let delivery = ExportCodecPolicy {
            container: ContainerFormat::Mp4,
            video_codec: VideoCodec::H264,
            video_profile: VideoProfile::H264High,
            audio_codec: Some(AudioCodec::Aac),
        };
        assert_eq!(
            delivery
                .resolve(&capabilities)
                .map(ResolvedCodecPolicy::policy),
            Ok(delivery)
        );

        let intermediate = ExportCodecPolicy {
            container: ContainerFormat::Mov,
            video_codec: VideoCodec::ProRes,
            video_profile: VideoProfile::ProRes422Hq,
            audio_codec: Some(AudioCodec::Pcm),
        };
        assert_eq!(
            intermediate
                .resolve(&capabilities)
                .map(ResolvedCodecPolicy::policy),
            Ok(intermediate)
        );
    }

    #[test]
    fn rejects_profile_and_container_mismatches_before_availability() {
        let capabilities = production_capabilities();
        let wrong_profile = ExportCodecPolicy {
            container: ContainerFormat::Mp4,
            video_codec: VideoCodec::H264,
            video_profile: VideoProfile::H265Main,
            audio_codec: None,
        };
        assert_eq!(
            wrong_profile.resolve(&capabilities),
            Err(CodecPolicyError::ProfileCodecMismatch)
        );

        let wrong_container = ExportCodecPolicy {
            container: ContainerFormat::WebM,
            video_codec: VideoCodec::H264,
            video_profile: VideoProfile::H264Main,
            audio_codec: Some(AudioCodec::Aac),
        };
        assert_eq!(
            wrong_container.resolve(&capabilities),
            Err(CodecPolicyError::UnsupportedVideoContainer)
        );
    }

    #[test]
    fn distinguishes_missing_muxer_video_and_audio_encoders() {
        let policy = ExportCodecPolicy {
            container: ContainerFormat::Mp4,
            video_codec: VideoCodec::H264,
            video_profile: VideoProfile::H264High,
            audio_codec: Some(AudioCodec::Aac),
        };
        assert_eq!(
            policy.resolve(&CodecCapabilities::default()),
            Err(CodecPolicyError::ContainerUnavailable)
        );
        assert_eq!(
            policy.resolve(&CodecCapabilities::new([ContainerFormat::Mp4], [], [])),
            Err(CodecPolicyError::VideoEncoderUnavailable)
        );
        assert_eq!(
            policy.resolve(&CodecCapabilities::new(
                [ContainerFormat::Mp4],
                [VideoCodec::H264],
                []
            )),
            Err(CodecPolicyError::AudioEncoderUnavailable)
        );
    }
}
