//! Synchronization coordinator, `ReplayGain` / crossfade DSP math, and high-level [`PlaybackEngine`].

use std::f32::consts::{FRAC_PI_2, PI};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vynl_core::config::{AudioBackendKind, PlaybackMode, SpotifyConfig};
use vynl_core::model::PlaybackState;

use crate::CompiledPlayerFeatures;
use crate::error::PlayerError;
use crate::remote_poll_interval;
use crate::sink::{AudioSink, scale_pcm_sample};
use crate::stream_cache::StreamCache;

/// Coordinates local playback progress interpolation, adaptive Connect polling,
/// and HTTP `429` rate-limit pause windows.
#[derive(Debug, Clone)]
pub struct SyncCoordinator {
    mode: PlaybackMode,
    anchor_position_ms: u64,
    duration_ms: u64,
    is_playing: bool,
    speed_percent: u16,
    anchor_instant: Option<Instant>,
    last_poll_at: Option<Instant>,
    rate_limited_until: Option<Instant>,
}

impl SyncCoordinator {
    /// Creates a new [`SyncCoordinator`] for the given [`PlaybackMode`].
    #[must_use]
    pub const fn new(mode: PlaybackMode) -> Self {
        Self {
            mode,
            anchor_position_ms: 0,
            duration_ms: 0,
            is_playing: false,
            speed_percent: 100,
            anchor_instant: None,
            last_poll_at: None,
            rate_limited_until: None,
        }
    }

    /// Returns the active [`PlaybackMode`].
    #[must_use]
    pub const fn mode(&self) -> PlaybackMode {
        self.mode
    }

    /// Updates the active [`PlaybackMode`].
    pub const fn set_mode(&mut self, mode: PlaybackMode) {
        self.mode = mode;
    }

    /// Returns `true` if the last known playback snapshot was actively playing.
    #[must_use]
    pub const fn is_playing(&self) -> bool {
        self.is_playing
    }

    /// Returns the last known track duration in milliseconds.
    #[must_use]
    pub const fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// Records a playback position anchor at an explicit timestamp `now`.
    pub const fn apply_snapshot_at(
        &mut self,
        position_ms: u64,
        duration_ms: u64,
        is_playing: bool,
        speed_percent: u16,
        now: Instant,
    ) {
        self.duration_ms = duration_ms;
        self.anchor_position_ms = clamp_to_duration(position_ms, duration_ms);
        self.is_playing = is_playing;
        self.speed_percent = if speed_percent == 0 {
            100
        } else {
            speed_percent
        };
        self.anchor_instant = Some(now);
    }

    /// Records a [`PlaybackState`] snapshot at an explicit timestamp `now`.
    pub fn apply_playback_state_at(&mut self, state: &PlaybackState, now: Instant) {
        let duration_ms = state.current_track.as_ref().map_or(0, |t| t.duration_ms);
        self.apply_snapshot_at(
            state.position_ms,
            duration_ms,
            state.is_playing,
            state.speed_percent,
            now,
        );
    }

    /// Records a [`PlaybackState`] snapshot using [`Instant::now`].
    pub fn apply_playback_state(&mut self, state: &PlaybackState) {
        self.apply_playback_state_at(state, Instant::now());
    }

    /// Computes the smoothly interpolated playback position in milliseconds at `now`.
    #[must_use]
    pub fn interpolated_position_ms_at(&self, now: Instant) -> u64 {
        if !self.is_playing {
            return clamp_to_duration(self.anchor_position_ms, self.duration_ms);
        }
        let Some(anchor) = self.anchor_instant else {
            return clamp_to_duration(self.anchor_position_ms, self.duration_ms);
        };
        let elapsed_ms = now.saturating_duration_since(anchor).as_millis();
        let speed = u128::from(if self.speed_percent == 0 {
            100
        } else {
            self.speed_percent
        });
        let scaled_elapsed = elapsed_ms.saturating_mul(speed) / 100;
        let advanced = u128::from(self.anchor_position_ms).saturating_add(scaled_elapsed);
        let position = u64::try_from(advanced).unwrap_or(u64::MAX);
        clamp_to_duration(position, self.duration_ms)
    }

    /// Computes the interpolated playback position using [`Instant::now`].
    #[must_use]
    pub fn interpolated_position_ms(&self) -> u64 {
        self.interpolated_position_ms_at(Instant::now())
    }

    /// Returns the adaptive polling interval for the current mode and playback state.
    ///
    /// - `PlaybackMode::Integrated`: `None` (event-driven push).
    /// - `PlaybackMode::Remote` playing: `Some(4 s)`.
    /// - `PlaybackMode::Remote` paused: `Some(25 s)`.
    #[must_use]
    pub const fn poll_interval(&self) -> Option<Duration> {
        remote_poll_interval(self.mode, self.is_playing)
    }

    /// Registers an HTTP `429 Too Many Requests` pause window starting at `now`.
    pub fn register_rate_limit_at(&mut self, retry_after: Duration, now: Instant) {
        let candidate = now + retry_after;
        self.rate_limited_until = Some(match self.rate_limited_until {
            Some(existing) if existing > candidate => existing,
            _ => candidate,
        });
    }

    /// Registers an HTTP `429` pause window using [`Instant::now`].
    pub fn register_rate_limit(&mut self, retry_after: Duration) {
        self.register_rate_limit_at(retry_after, Instant::now());
    }

    /// Returns `true` if polling is paused due to an active HTTP `429` window at `now`.
    #[must_use]
    pub fn is_rate_limited_at(&self, now: Instant) -> bool {
        self.rate_limited_until.is_some_and(|until| now < until)
    }

    /// Returns the remaining duration of any active HTTP `429` backoff window at `now`.
    #[must_use]
    pub fn rate_limit_remaining_at(&self, now: Instant) -> Option<Duration> {
        self.rate_limited_until
            .and_then(|until| until.checked_duration_since(now))
            .filter(|remaining| !remaining.is_zero())
    }

    /// Returns `true` if a remote Connect state poll should be issued at `now`.
    #[must_use]
    pub fn should_poll_at(&self, now: Instant) -> bool {
        let Some(interval) = self.poll_interval() else {
            return false;
        };
        if self.is_rate_limited_at(now) {
            return false;
        }
        let Some(last) = self.last_poll_at else {
            return true;
        };
        now.saturating_duration_since(last) >= interval
    }

    /// Returns the delay until the next remote poll is permitted, respecting both
    /// the adaptive interval and any active HTTP `429` pause window.
    ///
    /// Returns `None` in [`PlaybackMode::Integrated`].
    #[must_use]
    pub fn next_poll_delay_at(&self, now: Instant) -> Option<Duration> {
        let interval = self.poll_interval()?;
        let base_delay = self.last_poll_at.map_or(Duration::ZERO, |last| {
            interval.saturating_sub(now.saturating_duration_since(last))
        });
        let rate_delay = self.rate_limit_remaining_at(now).unwrap_or(Duration::ZERO);
        Some(base_delay.max(rate_delay))
    }

    /// Records that a remote Connect poll completed at `now`.
    pub const fn mark_polled_at(&mut self, now: Instant) {
        self.last_poll_at = Some(now);
    }

    /// Records that a remote Connect poll completed at [`Instant::now`].
    pub fn mark_polled(&mut self) {
        self.mark_polled_at(Instant::now());
    }
}

const fn clamp_to_duration(position_ms: u64, duration_ms: u64) -> u64 {
    if duration_ms > 0 && position_ms > duration_ms {
        duration_ms
    } else {
        position_ms
    }
}

/// Converts a decibel gain offset into a linear amplitude multiplier (`10^(gain_db / 20)`).
///
/// Non-finite inputs are handled safely: `NaN` returns `1.0`, `-inf` returns `0.0`,
/// and `+inf` returns `1.0`.
#[must_use]
pub fn gain_db_to_linear(gain_db: f32) -> f32 {
    if gain_db.is_nan() || gain_db == f32::INFINITY {
        return 1.0;
    }
    if gain_db == f32::NEG_INFINITY {
        return 0.0;
    }
    10.0_f32.powf(gain_db / 20.0)
}

/// Computes a `ReplayGain` linear scale factor with optional peak clipping protection.
///
/// Combines `track_gain_db` and `preamp_db`, converts to linear amplitude via
/// [`gain_db_to_linear`], and—when `prevent_clipping` is enabled and `peak_amplitude > 0.0`—
/// limits the scale so `scale * peak_amplitude <= 1.0`.
#[must_use]
pub fn replay_gain_scale(
    track_gain_db: f32,
    preamp_db: f32,
    peak_amplitude: Option<f32>,
    prevent_clipping: bool,
) -> f32 {
    let combined_db = if track_gain_db.is_finite() && preamp_db.is_finite() {
        track_gain_db + preamp_db
    } else {
        0.0
    };
    let mut scale = gain_db_to_linear(combined_db);
    if !scale.is_finite() || scale < 0.0 {
        return 1.0;
    }
    if prevent_clipping
        && let Some(peak) = peak_amplitude
        && peak.is_finite()
        && peak > 0.0
        && scale * peak > 1.0
    {
        scale = 1.0 / peak;
    }
    scale
}

/// Mathematical fade curve shape used when crossfading between consecutive tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CrossfadeCurve {
    /// Straight linear ramp (`outgoing = 1 - t`, `incoming = t`).
    Linear,
    /// Constant-energy trigonometric curve (`outgoing = cos(t * pi/2)`, `incoming = sin(t * pi/2)`).
    #[default]
    EqualPower,
    /// Smooth cosine S-curve ease-in/ease-out.
    SCurve,
}

/// Pair of linear amplitude gains (`0.0..=1.0`) applied to outgoing and incoming audio streams.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrossfadeGains {
    /// Gain applied to the outgoing (fading-out) track.
    pub outgoing: f32,
    /// Gain applied to the incoming (fading-in) track.
    pub incoming: f32,
}

/// Computes normalized crossfade progress in `0.0..=1.0` from elapsed time and total duration.
///
/// Returns `1.0` immediately when `duration` is zero.
#[must_use]
pub fn crossfade_progress(elapsed: Duration, duration: Duration) -> f32 {
    if duration.is_zero() {
        return 1.0;
    }
    (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

/// Computes [`CrossfadeGains`] at normalized `progress` (`0.0..=1.0`) for the chosen [`CrossfadeCurve`].
#[must_use]
pub fn crossfade_gains(progress: f32, curve: CrossfadeCurve) -> CrossfadeGains {
    let t = if progress.is_nan() {
        0.0
    } else {
        progress.clamp(0.0, 1.0)
    };
    match curve {
        CrossfadeCurve::Linear => CrossfadeGains {
            outgoing: 1.0 - t,
            incoming: t,
        },
        CrossfadeCurve::EqualPower => {
            let angle = t * FRAC_PI_2;
            CrossfadeGains {
                outgoing: angle.cos().clamp(0.0, 1.0),
                incoming: angle.sin().clamp(0.0, 1.0),
            }
        }
        CrossfadeCurve::SCurve => {
            let incoming = (0.5 - 0.5 * (t * PI).cos()).clamp(0.0, 1.0);
            CrossfadeGains {
                outgoing: (1.0 - incoming).clamp(0.0, 1.0),
                incoming,
            }
        }
    }
}

/// Mixes two PCM sample buffers using [`CrossfadeGains`] with saturation-safe clamping.
///
/// Output length equals `outgoing.len().max(incoming.len())`; missing tail samples are treated as `0`.
#[must_use]
pub fn mix_crossfade_pcm(outgoing: &[i16], incoming: &[i16], gains: CrossfadeGains) -> Vec<i16> {
    let len = outgoing.len().max(incoming.len());
    let mut mixed = Vec::with_capacity(len);
    let min = f64::from(i16::MIN);
    let max = f64::from(i16::MAX);
    let out_gain = f64::from(if gains.outgoing.is_finite() {
        gains.outgoing.max(0.0)
    } else {
        0.0
    });
    let in_gain = f64::from(if gains.incoming.is_finite() {
        gains.incoming.max(0.0)
    } else {
        0.0
    });

    for i in 0..len {
        let a = f64::from(outgoing.get(i).copied().unwrap_or(0));
        let b = f64::from(incoming.get(i).copied().unwrap_or(0));
        let sum = (a * out_gain + b * in_gain).round().clamp(min, max);
        #[allow(clippy::cast_possible_truncation)]
        let sample = sum as i16;
        mixed.push(sample);
    }
    mixed
}

/// Resolves the effective [`AudioBackendKind`] against compile-time feature flags.
///
/// Prefers `preferred` if compiled in, otherwise falls back to whichever integrated
/// backend is compiled, or returns `None` in remote-only builds.
#[must_use]
pub const fn resolve_backend(
    preferred: AudioBackendKind,
    features: CompiledPlayerFeatures,
) -> Option<AudioBackendKind> {
    match preferred {
        AudioBackendKind::Rodio if features.rodio => Some(AudioBackendKind::Rodio),
        AudioBackendKind::Subprocess | AudioBackendKind::Rodio if features.subprocess => {
            Some(AudioBackendKind::Subprocess)
        }
        AudioBackendKind::Subprocess if features.rodio => Some(AudioBackendKind::Rodio),
        _ => None,
    }
}

/// Resolves the effective [`PlaybackMode`] given compile-time audio backend availability.
///
/// Falls back to [`PlaybackMode::Remote`] when no integrated backend is compiled in.
#[must_use]
pub const fn resolve_mode(
    preferred: PlaybackMode,
    features: CompiledPlayerFeatures,
) -> PlaybackMode {
    match preferred {
        PlaybackMode::Integrated if features.supports_integrated_playback() => {
            PlaybackMode::Integrated
        }
        PlaybackMode::Integrated | PlaybackMode::Remote => PlaybackMode::Remote,
    }
}

/// High-level dual playback engine coordinator combining mode/backend selection,
/// bounded [`StreamCache`], [`SyncCoordinator`], `ReplayGain`, and crossfade processing.
#[derive(Debug)]
pub struct PlaybackEngine {
    requested_mode: PlaybackMode,
    requested_backend: AudioBackendKind,
    compiled_features: CompiledPlayerFeatures,
    stream_cache: StreamCache,
    sync: SyncCoordinator,
    replay_gain_preamp_db: f32,
    prevent_clipping: bool,
    crossfade_duration: Duration,
    crossfade_curve: CrossfadeCurve,
    volume_percent: u8,
    muted: bool,
    state: PlaybackState,
}

impl PlaybackEngine {
    /// Constructs a [`PlaybackEngine`] from [`SpotifyConfig`] and a stream cache directory.
    ///
    /// # Errors
    /// Returns [`PlayerError`] if opening the disk [`StreamCache`] fails.
    pub fn from_config(
        config: &SpotifyConfig,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<Self, PlayerError> {
        let cache = StreamCache::from_mib(cache_dir, config.stream_cache_mb)?;
        Ok(Self::new(config.playback_mode, config.audio_backend, cache))
    }

    /// Constructs a [`PlaybackEngine`] from explicit mode, backend, and [`StreamCache`].
    #[must_use]
    pub fn new(
        requested_mode: PlaybackMode,
        requested_backend: AudioBackendKind,
        stream_cache: StreamCache,
    ) -> Self {
        let compiled_features = CompiledPlayerFeatures::current();
        let effective_mode = resolve_mode(requested_mode, compiled_features);
        Self {
            requested_mode,
            requested_backend,
            compiled_features,
            stream_cache,
            sync: SyncCoordinator::new(effective_mode),
            replay_gain_preamp_db: 0.0,
            prevent_clipping: true,
            crossfade_duration: Duration::ZERO,
            crossfade_curve: CrossfadeCurve::EqualPower,
            volume_percent: 100,
            muted: false,
            state: PlaybackState::default(),
        }
    }

    /// Returns the requested [`PlaybackMode`].
    #[must_use]
    pub const fn requested_mode(&self) -> PlaybackMode {
        self.requested_mode
    }

    /// Returns the effective [`PlaybackMode`] after applying compile-time backend capabilities.
    #[must_use]
    pub const fn effective_mode(&self) -> PlaybackMode {
        resolve_mode(self.requested_mode, self.compiled_features)
    }

    /// Returns the requested [`AudioBackendKind`].
    #[must_use]
    pub const fn requested_backend(&self) -> AudioBackendKind {
        self.requested_backend
    }

    /// Returns the effective [`AudioBackendKind`], or `None` in remote-only builds.
    #[must_use]
    pub const fn effective_backend(&self) -> Option<AudioBackendKind> {
        resolve_backend(self.requested_backend, self.compiled_features)
    }

    /// Returns compile-time player feature flags.
    #[must_use]
    pub const fn compiled_features(&self) -> CompiledPlayerFeatures {
        self.compiled_features
    }

    /// Updates the requested [`PlaybackMode`] and synchronizes the [`SyncCoordinator`].
    pub const fn set_mode(&mut self, mode: PlaybackMode) {
        self.requested_mode = mode;
        let effective = resolve_mode(mode, self.compiled_features);
        self.sync.set_mode(effective);
    }

    /// Updates the requested [`AudioBackendKind`].
    pub const fn set_backend(&mut self, backend: AudioBackendKind) {
        self.requested_backend = backend;
    }

    /// Configures `ReplayGain` preamp offset and clipping protection.
    pub const fn set_replay_gain(&mut self, preamp_db: f32, prevent_clipping: bool) {
        self.replay_gain_preamp_db = preamp_db;
        self.prevent_clipping = prevent_clipping;
    }

    /// Configures crossfade duration and ramp curve.
    pub const fn set_crossfade(&mut self, duration: Duration, curve: CrossfadeCurve) {
        self.crossfade_duration = duration;
        self.crossfade_curve = curve;
    }

    /// Returns the configured crossfade duration.
    #[must_use]
    pub const fn crossfade_duration(&self) -> Duration {
        self.crossfade_duration
    }

    /// Returns the configured [`CrossfadeCurve`].
    #[must_use]
    pub const fn crossfade_curve(&self) -> CrossfadeCurve {
        self.crossfade_curve
    }

    /// Sets output volume (`0..=100`, clamped).
    pub fn set_volume(&mut self, percent: u8) {
        self.volume_percent = percent.min(100);
        self.state.volume_percent = self.volume_percent;
    }

    /// Sets mute state.
    pub const fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.state.muted = muted;
    }

    /// Returns the effective volume percentage (`0` when muted).
    #[must_use]
    pub const fn effective_volume_percent(&self) -> u8 {
        if self.muted { 0 } else { self.volume_percent }
    }

    /// Immutable reference to the bounded [`StreamCache`].
    #[must_use]
    pub const fn stream_cache(&self) -> &StreamCache {
        &self.stream_cache
    }

    /// Mutable reference to the bounded [`StreamCache`].
    pub const fn stream_cache_mut(&mut self) -> &mut StreamCache {
        &mut self.stream_cache
    }

    /// Immutable reference to the [`SyncCoordinator`].
    #[must_use]
    pub const fn sync(&self) -> &SyncCoordinator {
        &self.sync
    }

    /// Mutable reference to the [`SyncCoordinator`].
    pub const fn sync_mut(&mut self) -> &mut SyncCoordinator {
        &mut self.sync
    }

    /// Returns the most recently applied [`PlaybackState`].
    #[must_use]
    pub const fn state(&self) -> &PlaybackState {
        &self.state
    }

    /// Applies an authoritative [`PlaybackState`] snapshot at timestamp `now`.
    pub fn update_state_at(&mut self, state: PlaybackState, now: Instant) {
        self.sync.apply_playback_state_at(&state, now);
        self.volume_percent = state.volume_percent.min(100);
        self.muted = state.muted;
        self.state = state;
    }

    /// Applies an authoritative [`PlaybackState`] snapshot using [`Instant::now`].
    pub fn update_state(&mut self, state: PlaybackState) {
        self.update_state_at(state, Instant::now());
    }

    /// Computes the effective linear `ReplayGain` scale for a track.
    #[must_use]
    pub fn compute_track_gain(&self, track_gain_db: f32, track_peak: Option<f32>) -> f32 {
        replay_gain_scale(
            track_gain_db,
            self.replay_gain_preamp_db,
            track_peak,
            self.prevent_clipping,
        )
    }

    /// Computes [`CrossfadeGains`] at `elapsed` into the configured crossfade window.
    #[must_use]
    pub fn compute_crossfade(&self, elapsed: Duration) -> CrossfadeGains {
        let progress = crossfade_progress(elapsed, self.crossfade_duration);
        crossfade_gains(progress, self.crossfade_curve)
    }

    /// Applies `ReplayGain` normalization and volume/mute scaling, then writes PCM to `sink`.
    ///
    /// # Errors
    /// Returns [`PlayerError`] if `sink.write_pcm` fails.
    pub fn push_pcm_to_sink(
        &self,
        sink: &mut dyn AudioSink,
        samples: &[i16],
        track_gain_db: f32,
        track_peak: Option<f32>,
    ) -> Result<(), PlayerError> {
        sink.set_volume(self.effective_volume_percent());
        let gain = self.compute_track_gain(track_gain_db, track_peak);
        if (gain - 1.0).abs() < 1e-5 {
            return sink.write_pcm(samples);
        }
        let normalized: Vec<i16> = samples
            .iter()
            .map(|&sample| scale_pcm_sample(sample, gain))
            .collect();
        sink.write_pcm(&normalized)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use vynl_core::config::{AudioBackendKind, PlaybackMode, SpotifyConfig};
    use vynl_core::model::{PlaybackState, TrackSummary};

    use super::{
        CrossfadeCurve, PlaybackEngine, SyncCoordinator, crossfade_gains, crossfade_progress,
        gain_db_to_linear, mix_crossfade_pcm, replay_gain_scale, resolve_backend, resolve_mode,
    };
    use crate::CompiledPlayerFeatures;
    use crate::sink::{AudioFormat, NullAudioSink};

    static ENGINE_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn sync_coordinator_interpolates_and_clamps_progress() {
        let mut sync = SyncCoordinator::new(PlaybackMode::Remote);
        let t0 = Instant::now();
        sync.apply_snapshot_at(10_000, 15_000, true, 100, t0);

        assert_eq!(sync.interpolated_position_ms_at(t0), 10_000);
        assert_eq!(
            sync.interpolated_position_ms_at(t0 + Duration::from_millis(2_500)),
            12_500
        );
        // Clamped to track duration (15_000 ms).
        assert_eq!(
            sync.interpolated_position_ms_at(t0 + Duration::from_secs(10)),
            15_000
        );

        // Podcast 1.5x speed interpolation.
        sync.apply_snapshot_at(1_000, 60_000, true, 150, t0);
        assert_eq!(
            sync.interpolated_position_ms_at(t0 + Duration::from_secs(2)),
            4_000
        );

        // Paused playback does not advance position.
        sync.apply_snapshot_at(7_500, 60_000, false, 100, t0);
        assert_eq!(
            sync.interpolated_position_ms_at(t0 + Duration::from_secs(30)),
            7_500
        );
    }

    #[test]
    fn sync_coordinator_adaptive_polling_and_429_pause_window() {
        let t0 = Instant::now();

        // Integrated player never polls.
        let integrated = SyncCoordinator::new(PlaybackMode::Integrated);
        assert_eq!(integrated.poll_interval(), None);
        assert!(!integrated.should_poll_at(t0));
        assert_eq!(integrated.next_poll_delay_at(t0), None);

        // Remote player polls immediately, then every 4s while playing and 25s while paused.
        let mut remote = SyncCoordinator::new(PlaybackMode::Remote);
        remote.apply_snapshot_at(0, 180_000, true, 100, t0);
        assert_eq!(remote.poll_interval(), Some(Duration::from_secs(4)));
        assert!(remote.should_poll_at(t0));

        remote.mark_polled_at(t0);
        assert!(!remote.should_poll_at(t0 + Duration::from_secs(3)));
        assert_eq!(
            remote.next_poll_delay_at(t0 + Duration::from_secs(3)),
            Some(Duration::from_secs(1))
        );
        assert!(remote.should_poll_at(t0 + Duration::from_secs(4)));

        // Switch to paused -> 25s polling interval.
        remote.apply_snapshot_at(4_000, 180_000, false, 100, t0 + Duration::from_secs(4));
        remote.mark_polled_at(t0 + Duration::from_secs(4));
        assert_eq!(remote.poll_interval(), Some(Duration::from_secs(25)));
        assert!(!remote.should_poll_at(t0 + Duration::from_secs(20)));
        assert!(remote.should_poll_at(t0 + Duration::from_secs(29)));

        // Register HTTP 429 retry window for 40s.
        let t_rl = t0 + Duration::from_secs(29);
        remote.register_rate_limit_at(Duration::from_secs(40), t_rl);
        assert!(remote.is_rate_limited_at(t_rl + Duration::from_secs(10)));
        assert!(!remote.should_poll_at(t_rl + Duration::from_secs(10)));
        assert_eq!(
            remote.next_poll_delay_at(t_rl + Duration::from_secs(10)),
            Some(Duration::from_secs(30))
        );
        assert!(!remote.is_rate_limited_at(t_rl + Duration::from_secs(40)));
        assert!(remote.should_poll_at(t_rl + Duration::from_secs(40)));
    }

    #[test]
    fn replay_gain_and_clipping_protection_math() {
        assert!((gain_db_to_linear(0.0) - 1.0).abs() < 1e-6);
        assert!((gain_db_to_linear(-6.0206) - 0.5).abs() < 1e-3);
        assert!((gain_db_to_linear(6.0206) - 2.0).abs() < 1e-3);
        assert!((gain_db_to_linear(f32::NEG_INFINITY) - 0.0).abs() < 1e-6);
        assert!((gain_db_to_linear(f32::NAN) - 1.0).abs() < 1e-6);

        // +6 dB (~2.0x) on a track peaking at 0.8 would clip (1.6 > 1.0), so limit to 1/0.8 = 1.25.
        let safe = replay_gain_scale(6.0206, 0.0, Some(0.8), true);
        assert!((safe - 1.25).abs() < 1e-4);

        // Without clipping protection, full ~2.0x scale is returned.
        let raw = replay_gain_scale(6.0206, 0.0, Some(0.8), false);
        assert!((raw - 2.0).abs() < 1e-3);
    }

    #[test]
    fn crossfade_curves_and_pcm_mixing() {
        assert!(
            (crossfade_progress(Duration::from_secs(2), Duration::from_secs(4)) - 0.5).abs() < 1e-6
        );
        assert!((crossfade_progress(Duration::from_secs(1), Duration::ZERO) - 1.0).abs() < 1e-6);

        // Linear midpoint: 0.5 / 0.5.
        let lin = crossfade_gains(0.5, CrossfadeCurve::Linear);
        assert!((lin.outgoing - 0.5).abs() < 1e-6);
        assert!((lin.incoming - 0.5).abs() < 1e-6);

        // Equal-power preserves constant energy: outgoing^2 + incoming^2 == 1.0.
        for step in 0..=10 {
            let t = f32::from(u8::try_from(step).unwrap_or(0)) / 10.0;
            let eq = crossfade_gains(t, CrossfadeCurve::EqualPower);
            let power = eq.outgoing * eq.outgoing + eq.incoming * eq.incoming;
            assert!((power - 1.0).abs() < 1e-5);
        }

        // S-curve endpoints and midpoint.
        let s0 = crossfade_gains(0.0, CrossfadeCurve::SCurve);
        let s5 = crossfade_gains(0.5, CrossfadeCurve::SCurve);
        let s1 = crossfade_gains(1.0, CrossfadeCurve::SCurve);
        assert!((s0.outgoing - 1.0).abs() < 1e-6);
        assert!((s0.incoming - 0.0).abs() < 1e-6);
        assert!((s5.outgoing - 0.5).abs() < 1e-6);
        assert!((s5.incoming - 0.5).abs() < 1e-6);
        assert!((s1.outgoing - 0.0).abs() < 1e-6);
        assert!((s1.incoming - 1.0).abs() < 1e-6);

        let mixed = mix_crossfade_pcm(&[1_000, -2_000], &[3_000, 2_000, 4_000], lin);
        assert_eq!(mixed, vec![2_000, 0, 2_000]);
    }

    #[test]
    fn backend_and_mode_resolution_respects_compiled_features() {
        let remote_only = CompiledPlayerFeatures {
            rodio: false,
            subprocess: false,
            visualizer: false,
        };
        assert_eq!(resolve_backend(AudioBackendKind::Rodio, remote_only), None);
        assert_eq!(
            resolve_mode(PlaybackMode::Integrated, remote_only),
            PlaybackMode::Remote
        );

        let subproc_only = CompiledPlayerFeatures {
            rodio: false,
            subprocess: true,
            visualizer: false,
        };
        assert_eq!(
            resolve_backend(AudioBackendKind::Rodio, subproc_only),
            Some(AudioBackendKind::Subprocess)
        );
        assert_eq!(
            resolve_mode(PlaybackMode::Integrated, subproc_only),
            PlaybackMode::Integrated
        );
    }

    #[test]
    fn playback_engine_integrates_cache_sync_and_sink() -> Result<(), String> {
        let count = ENGINE_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "vynl_engine_{}_{count}_{stamp}",
            std::process::id()
        ));

        let cfg = SpotifyConfig {
            stream_cache_mb: 1,
            ..SpotifyConfig::default()
        };
        let mut engine = PlaybackEngine::from_config(&cfg, &dir).map_err(|e| e.to_string())?;
        engine.set_crossfade(Duration::from_secs(4), CrossfadeCurve::EqualPower);
        engine.set_replay_gain(0.0, true);
        engine.set_volume(100);

        let now = Instant::now();
        let state = PlaybackState {
            current_track: Some(TrackSummary {
                id: String::from("trk1"),
                title: String::from("Test Track"),
                artists: vec![String::from("Artist")],
                album: String::from("Album"),
                duration_ms: 120_000,
                explicit: false,
                is_liked: true,
                cover: None,
            }),
            is_playing: true,
            position_ms: 5_000,
            volume_percent: 100,
            speed_percent: 100,
            ..PlaybackState::default()
        };
        engine.update_state_at(state, now);
        assert_eq!(
            engine
                .sync()
                .interpolated_position_ms_at(now + Duration::from_secs(3)),
            8_000
        );

        let mut sink = NullAudioSink::recording(AudioFormat::CD_STEREO);
        engine
            .push_pcm_to_sink(&mut sink, &[2_000, -2_000], -6.0206, Some(0.5))
            .map_err(|e| e.to_string())?;
        let captured = sink.captured_samples();
        assert_eq!(captured.len(), 2);
        assert!((i32::from(captured[0]) - 1_000).abs() <= 2);
        assert!((i32::from(captured[1]) + 1_000).abs() <= 2);

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}
