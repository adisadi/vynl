//! Dual playback engine contracts and adaptive synchronization policy.
//!
//! Compile-time feature flags allow `vynl` to cross-compile cleanly:
//! - `rodio-backend` (default): cross-platform `cpal`/`rodio` output.
//! - `subprocess-backend`: 100% pure-Rust PCM pipe fallback for `musl`/FreeBSD.
//! - `--no-default-features`: remote-only Spotify Connect controller.

use std::time::Duration;

use vynl_core::config::PlaybackMode;

/// Compiled audio backend capabilities of the current binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompiledPlayerFeatures {
    /// Built with `rodio-backend`.
    pub rodio: bool,
    /// Built with pure-Rust `subprocess-backend`.
    pub subprocess: bool,
    /// Built with FFT `visualizer` support.
    pub visualizer: bool,
}

impl CompiledPlayerFeatures {
    /// Inspects feature flags enabled at compile time.
    pub const fn current() -> Self {
        Self {
            rodio: cfg!(feature = "rodio-backend"),
            subprocess: cfg!(feature = "subprocess-backend"),
            visualizer: cfg!(feature = "visualizer"),
        }
    }

    /// Returns `true` if at least one integrated audio backend is compiled in.
    pub const fn supports_integrated_playback(self) -> bool {
        self.rodio || self.subprocess
    }
}

/// Computes the recommended remote-state polling interval.
///
/// - Integrated local `librespot` playback uses event-driven push (`None`).
/// - Remote Spotify Connect devices poll adaptively: 4 s while playing, 25 s while paused.
pub const fn remote_poll_interval(mode: PlaybackMode, is_playing: bool) -> Option<Duration> {
    match mode {
        PlaybackMode::Integrated => None,
        PlaybackMode::Remote if is_playing => Some(Duration::from_secs(4)),
        PlaybackMode::Remote => Some(Duration::from_secs(25)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use vynl_core::config::PlaybackMode;

    use super::{CompiledPlayerFeatures, remote_poll_interval};

    #[test]
    fn adaptive_polling_disables_polling_for_integrated_player() {
        assert_eq!(remote_poll_interval(PlaybackMode::Integrated, true), None);
        assert_eq!(remote_poll_interval(PlaybackMode::Integrated, false), None);
        assert_eq!(
            remote_poll_interval(PlaybackMode::Remote, true),
            Some(Duration::from_secs(4))
        );
        assert_eq!(
            remote_poll_interval(PlaybackMode::Remote, false),
            Some(Duration::from_secs(25))
        );
    }

    #[test]
    fn reports_compiled_features_deterministically() {
        let feats = CompiledPlayerFeatures::current();
        assert_eq!(
            feats.supports_integrated_playback(),
            feats.rodio || feats.subprocess
        );
    }
}
