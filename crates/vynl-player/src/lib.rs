//! Dual playback engine, bounded audio stream cache, PCM audio sinks,
//! adaptive synchronization policy, and pure-Rust Radix-2 FFT visualizer.
//!
//! Compile-time feature flags allow `vynl` to cross-compile cleanly:
//! - `rodio-backend` (default): cross-platform `cpal`/`rodio` output seam.
//! - `subprocess-backend`: 100% pure-Rust PCM pipe fallback for `musl`/FreeBSD.
//! - `visualizer`: pure-Rust Cooley-Tukey Radix-2 FFT spectrum analyzer.
//! - `--no-default-features`: remote-only Spotify Connect controller.

use std::time::Duration;

use vynl_core::config::PlaybackMode;

pub mod error;
#[cfg(all(
    not(target_os = "freebsd"),
    any(feature = "rodio-backend", feature = "subprocess-backend")
))]
pub mod librespot_engine;
pub mod sink;
pub mod stream_cache;
pub mod sync;
pub mod visualizer;

pub use error::PlayerError;
#[cfg(all(
    not(target_os = "freebsd"),
    any(feature = "rodio-backend", feature = "subprocess-backend")
))]
pub use librespot_engine::{
    IntegratedCommand, IntegratedEvent, IntegratedPlayerConfig, IntegratedPlayerHandle,
    VynlLibrespotSink,
};
#[cfg(feature = "rodio-backend")]
pub use sink::RodioAudioSink;
pub use sink::{AudioFormat, AudioSink, NullAudioSink, scale_pcm_sample, scale_pcm_volume};
#[cfg(feature = "subprocess-backend")]
pub use sink::{SubprocessAudioSink, SubprocessCommand};
pub use stream_cache::{
    BYTES_PER_MIB, DEFAULT_MAX_CACHE_BYTES, StreamCache, StreamCacheStats, default_cache_dir,
};
pub use sync::{
    CrossfadeCurve, CrossfadeGains, PlaybackEngine, SyncCoordinator, crossfade_gains,
    crossfade_progress, gain_db_to_linear, mix_crossfade_pcm, replay_gain_scale, resolve_backend,
    resolve_mode,
};
#[cfg(feature = "visualizer")]
pub use visualizer::{Complex, SpectrumAnalyzer, hann_window, radix2_fft};

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
    #[must_use]
    pub const fn current() -> Self {
        Self {
            rodio: cfg!(feature = "rodio-backend"),
            subprocess: cfg!(feature = "subprocess-backend"),
            visualizer: cfg!(feature = "visualizer"),
        }
    }

    /// Returns `true` if at least one integrated audio backend is compiled in.
    #[must_use]
    pub const fn supports_integrated_playback(self) -> bool {
        self.rodio || self.subprocess
    }
}

/// Computes the recommended remote-state polling interval.
///
/// - Integrated local `librespot` playback uses event-driven push (`None`).
/// - Remote Spotify Connect devices poll adaptively: 4 s while playing, 25 s while paused.
#[must_use]
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
