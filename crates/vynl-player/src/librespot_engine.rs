//! Integrated `librespot` playback engine and Spotify Connect (`Spirc`) runtime.
//!
//! Compiled whenever an integrated audio backend (`rodio-backend` or `subprocess-backend`)
//! is enabled. Bridges `librespot` 0.8 (`librespot-core`, `librespot-playback`,
//! `librespot-connect`) onto `vynl-player`'s [`AudioSink`] abstraction on a dedicated
//! `tokio` runtime thread so the TUI event loop never blocks.

use std::fmt;
use std::path::{Path, PathBuf};
#[cfg(feature = "visualizer")]
use std::sync::Mutex;
use std::sync::{Arc, mpsc as std_mpsc};
use std::thread::{self, JoinHandle};

use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::authentication::Credentials;
use librespot_core::cache::Cache;
use librespot_core::config::DeviceType;
use librespot_core::{Session, SessionConfig, SpotifyUri};
use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::softmixer::SoftMixer;
use librespot_playback::mixer::{Mixer, MixerConfig};
use librespot_playback::player::{Player, PlayerEvent};
use tokio::runtime::Builder as TokioRuntimeBuilder;
use tokio::sync::mpsc as tokio_mpsc;
use vynl_core::config::{AudioBackendKind, SpotifyConfig};

use crate::CompiledPlayerFeatures;
use crate::error::PlayerError;
use crate::sink::{AudioFormat, AudioSink, NullAudioSink};
use crate::stream_cache::BYTES_PER_MIB;
use crate::sync::resolve_backend;
#[cfg(feature = "visualizer")]
use crate::visualizer::SpectrumAnalyzer;

/// Number of logarithmic FFT spectrum bands matching Zen mode's visualizer.
#[cfg(feature = "visualizer")]
const ZEN_SPECTRUM_BANDS: usize = 16;

/// Configuration for the integrated `librespot` player and Spotify Connect device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegratedPlayerConfig {
    /// Spotify Connect device name advertised to other Spotify clients.
    pub device_name: String,
    /// Streaming audio bitrate in kbps (`96`, `160`, or `320`).
    pub bitrate_kbps: u16,
    /// Directory used for bounded temporary `librespot` audio stream caching.
    pub cache_dir: PathBuf,
    /// Maximum disk stream cache size in MiB (`0` disables on-disk audio caching).
    pub stream_cache_mb: u32,
    /// Preferred local PCM audio backend.
    pub backend: AudioBackendKind,
}

impl IntegratedPlayerConfig {
    /// Constructs an [`IntegratedPlayerConfig`] from a [`SpotifyConfig`] and cache directory path.
    #[must_use]
    pub fn from_spotify_config(config: &SpotifyConfig, cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            device_name: config.device_name.clone(),
            bitrate_kbps: config.bitrate_kbps,
            cache_dir: cache_dir.into(),
            stream_cache_mb: config.stream_cache_mb,
            backend: config.audio_backend,
        }
    }

    /// Maps [`Self::bitrate_kbps`] onto a `librespot` [`Bitrate`] tier (`96`, `160`, or `320` kbps).
    #[must_use]
    pub const fn librespot_bitrate(&self) -> Bitrate {
        match self.bitrate_kbps {
            ..=96 => Bitrate::Bitrate96,
            97..=160 => Bitrate::Bitrate160,
            161.. => Bitrate::Bitrate320,
        }
    }

    /// Builds a `librespot` [`PlayerConfig`] matching this configuration.
    #[must_use]
    pub fn to_player_config(&self) -> PlayerConfig {
        PlayerConfig {
            bitrate: self.librespot_bitrate(),
            ..PlayerConfig::default()
        }
    }

    /// Builds a bounded `librespot` [`Cache`], honoring `stream_cache_mb == 0` as disabled.
    ///
    /// Returns `Ok(None)` when [`Self::stream_cache_mb`] is `0`.
    ///
    /// # Errors
    /// Returns [`PlayerError::CacheError`] if initializing the cache directory fails.
    pub fn to_cache(&self) -> Result<Option<Cache>, PlayerError> {
        if self.stream_cache_mb == 0 {
            return Ok(None);
        }
        let size_limit = u64::from(self.stream_cache_mb).saturating_mul(BYTES_PER_MIB);
        let cache = Cache::new(
            None::<&Path>,
            None,
            Some(self.cache_dir.as_path()),
            Some(size_limit),
        )
        .map_err(|err| PlayerError::CacheError(err.to_string()))?;
        Ok(Some(cache))
    }

    /// Builds a `librespot` [`SessionConfig`].
    #[must_use]
    pub fn to_session_config(&self) -> SessionConfig {
        let _ = &self.device_name;
        SessionConfig::default()
    }

    /// Builds a `librespot` [`ConnectConfig`] advertising this device name as a computer client.
    #[must_use]
    pub fn to_connect_config(&self) -> ConnectConfig {
        let name = if self.device_name.trim().is_empty() {
            String::from("vynl")
        } else {
            self.device_name.clone()
        };
        ConnectConfig {
            name,
            device_type: DeviceType::Computer,
            ..ConnectConfig::default()
        }
    }
}

/// Callback receiving interleaved `i16` PCM slices forwarded through [`VynlLibrespotSink`].
#[cfg(feature = "visualizer")]
pub type PcmTapCallback = Box<dyn FnMut(&[i16]) + Send>;

/// Adapter implementing `librespot`'s [`Sink`] trait on top of any [`AudioSink`].
///
/// Allows `RodioAudioSink`, `SubprocessAudioSink`, or [`NullAudioSink`] to be driven directly
/// by `librespot`'s decoder and playback thread.
pub struct VynlLibrespotSink {
    inner: Box<dyn AudioSink + Send>,
    samples_forwarded: u64,
    #[cfg(feature = "visualizer")]
    pcm_tap: Option<PcmTapCallback>,
}

impl VynlLibrespotSink {
    /// Wraps an [`AudioSink`] for consumption by `librespot`.
    #[must_use]
    pub fn new(inner: Box<dyn AudioSink + Send>) -> Self {
        Self {
            inner,
            samples_forwarded: 0,
            #[cfg(feature = "visualizer")]
            pcm_tap: None,
        }
    }

    /// Attaches an optional PCM sample tap invoked with every interleaved `i16` buffer written.
    #[cfg(feature = "visualizer")]
    #[must_use]
    pub fn with_pcm_tap(mut self, tap: PcmTapCallback) -> Self {
        self.pcm_tap = Some(tap);
        self
    }

    /// Returns the total number of `i16` PCM samples forwarded to the wrapped [`AudioSink`].
    #[must_use]
    pub const fn samples_forwarded(&self) -> u64 {
        self.samples_forwarded
    }

    /// Consumes this adapter and returns the wrapped [`AudioSink`].
    #[must_use]
    pub fn into_inner(self) -> Box<dyn AudioSink + Send> {
        self.inner
    }
}

impl fmt::Debug for VynlLibrespotSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VynlLibrespotSink")
            .field("samples_forwarded", &self.samples_forwarded)
            .finish_non_exhaustive()
    }
}

impl Sink for VynlLibrespotSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.resume();
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.inner.pause();
        self.inner.flush();
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        match packet {
            AudioPacket::Samples(samples) => {
                let pcm = converter.f64_to_s16(&samples);
                self.inner
                    .write_pcm(&pcm)
                    .map_err(|err| SinkError::OnWrite(err.to_string()))?;
                let count = u64::try_from(pcm.len()).unwrap_or(u64::MAX);
                self.samples_forwarded = self.samples_forwarded.saturating_add(count);
                #[cfg(feature = "visualizer")]
                if let Some(tap) = self.pcm_tap.as_mut() {
                    tap(&pcm);
                }
                Ok(())
            }
            AudioPacket::Raw(_) => Err(SinkError::InvalidParams(String::from(
                "raw passthrough packets are not supported by VynlLibrespotSink",
            ))),
        }
    }
}

/// Control commands accepted by [`IntegratedPlayerHandle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegratedCommand {
    /// Loads a Spotify track or episode URI at `position_ms`, optionally starting playback.
    LoadTrack {
        /// Canonical Spotify URI (e.g. `spotify:track:<base62>`).
        spotify_uri: String,
        /// Whether playback should begin immediately after loading.
        start_playing: bool,
        /// Initial playback offset in milliseconds.
        position_ms: u32,
    },
    /// Resumes local playback.
    Play,
    /// Pauses local playback.
    Pause,
    /// Seeks within the active track to the given offset in milliseconds.
    Seek(u32),
    /// Sets software output volume percentage (`0..=100`).
    SetVolume(u8),
    /// Stops playback and clears the active track.
    Stop,
    /// Disconnects Spotify Connect and terminates the background runtime thread.
    Shutdown,
}

/// Asynchronous playback and connection events emitted by [`IntegratedPlayerHandle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegratedEvent {
    /// Spotify session and Connect (`Spirc`) device registered successfully.
    Connected {
        /// Active Spotify Connect device name.
        device_name: String,
    },
    /// Local playback is actively playing at `position_ms`.
    Playing {
        /// Canonical Spotify track URI.
        track_id: String,
        /// Playback position in milliseconds.
        position_ms: u32,
    },
    /// Local playback is paused at `position_ms`.
    Paused {
        /// Canonical Spotify track URI.
        track_id: String,
        /// Playback position in milliseconds.
        position_ms: u32,
    },
    /// Playback stopped or the `Spirc` loop exited.
    Stopped,
    /// Active track finished playing to the end.
    TrackEnded {
        /// Canonical Spotify track URI.
        track_id: String,
    },
    /// Recoverable or fatal playback/session error message.
    Error(String),
}

/// Non-blocking handle to the background `librespot` playback and `Spirc` runtime thread.
#[derive(Debug)]
pub struct IntegratedPlayerHandle {
    cmd_tx: tokio_mpsc::UnboundedSender<IntegratedCommand>,
    event_rx: std_mpsc::Receiver<IntegratedEvent>,
    worker: Option<JoinHandle<()>>,
    #[cfg(feature = "visualizer")]
    spectrum: Arc<Mutex<SpectrumAnalyzer>>,
}

impl IntegratedPlayerHandle {
    /// Spawns a dedicated `tokio` runtime thread that connects to Spotify using `access_token`,
    /// initializes `librespot`'s [`Player`] with [`VynlLibrespotSink`], and runs [`Spirc`].
    ///
    /// # Errors
    /// Returns [`PlayerError::InvalidState`] if `access_token` is blank,
    /// [`PlayerError::CacheError`] if initializing the bounded stream cache fails,
    /// or [`PlayerError::Io`] if building the `tokio` runtime or spawning the OS thread fails.
    pub fn spawn_with_access_token(
        config: IntegratedPlayerConfig,
        access_token: String,
    ) -> Result<Self, PlayerError> {
        if access_token.trim().is_empty() {
            return Err(PlayerError::InvalidState(String::from(
                "Spotify access token must not be empty",
            )));
        }

        let cache = config.to_cache()?;
        let runtime = TokioRuntimeBuilder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("vynl-librespot-io")
            .build()?;

        let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<IntegratedCommand>();
        let (event_tx, event_rx) = std_mpsc::channel::<IntegratedEvent>();

        #[cfg(feature = "visualizer")]
        let spectrum = Arc::new(Mutex::new(SpectrumAnalyzer::new(
            SpectrumAnalyzer::DEFAULT_FFT_SIZE,
            ZEN_SPECTRUM_BANDS,
            AudioFormat::CD_STEREO.sample_rate,
            AudioFormat::CD_STEREO.channels,
        )?));
        #[cfg(feature = "visualizer")]
        let spectrum_worker = Arc::clone(&spectrum);

        let worker = thread::Builder::new()
            .name(String::from("vynl-librespot-main"))
            .spawn(move || {
                runtime.block_on(run_integrated_session(
                    config,
                    access_token,
                    cache,
                    cmd_rx,
                    event_tx,
                    #[cfg(feature = "visualizer")]
                    spectrum_worker,
                ));
            })?;

        Ok(Self {
            cmd_tx,
            event_rx,
            worker: Some(worker),
            #[cfg(feature = "visualizer")]
            spectrum,
        })
    }

    /// Enqueues an [`IntegratedCommand`] for the background `librespot` engine without blocking.
    ///
    /// # Errors
    /// Returns [`PlayerError::InvalidState`] if the background player runtime has already stopped.
    pub fn send(&self, cmd: IntegratedCommand) -> Result<(), PlayerError> {
        self.cmd_tx.send(cmd).map_err(|_| {
            PlayerError::InvalidState(String::from("integrated player runtime has stopped"))
        })
    }

    /// Polls the next available [`IntegratedEvent`] without blocking the caller.
    #[must_use]
    pub fn try_recv_event(&self) -> Option<IntegratedEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Returns the latest normalized FFT spectrum bars (`0.0..=1.0`) from the decoded PCM tap.
    ///
    /// Returns an empty vector when built without the `visualizer` feature or if the
    /// analyzer lock is unavailable.
    #[must_use]
    #[cfg_attr(not(feature = "visualizer"), allow(clippy::unused_self))]
    pub fn spectrum_bars(&self) -> Vec<f32> {
        #[cfg(feature = "visualizer")]
        {
            self.spectrum
                .lock()
                .map(|analyzer| analyzer.bars().to_vec())
                .unwrap_or_default()
        }
        #[cfg(not(feature = "visualizer"))]
        {
            Vec::new()
        }
    }

    /// Sends [`IntegratedCommand::Shutdown`] and waits for the background runtime thread to exit.
    pub fn shutdown(mut self) {
        let _ = self.cmd_tx.send(IntegratedCommand::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for IntegratedPlayerHandle {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(IntegratedCommand::Shutdown);
    }
}

#[cfg(feature = "visualizer")]
fn build_spectrum_tap(analyzer: Arc<Mutex<SpectrumAnalyzer>>) -> PcmTapCallback {
    Box::new(move |pcm| {
        if let Ok(mut guard) = analyzer.try_lock() {
            guard.push_interleaved_pcm(pcm);
        }
    })
}

async fn run_integrated_session(
    config: IntegratedPlayerConfig,
    access_token: String,
    cache: Option<Cache>,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<IntegratedCommand>,
    event_tx: std_mpsc::Sender<IntegratedEvent>,
    #[cfg(feature = "visualizer")] spectrum: Arc<Mutex<SpectrumAnalyzer>>,
) {
    let session_config = config.to_session_config();
    let player_config = config.to_player_config();
    let connect_config = config.to_connect_config();
    let device_name = connect_config.name.clone();
    let credentials = Credentials::with_access_token(access_token);

    let session = Session::new(session_config, cache);

    let mixer: Arc<dyn Mixer> = match SoftMixer::open(MixerConfig::default()) {
        Ok(mixer) => Arc::new(mixer),
        Err(err) => {
            let _ = event_tx.send(IntegratedEvent::Error(format!(
                "failed to initialize soft mixer: {err}"
            )));
            return;
        }
    };

    let backend = config.backend;
    let player = Player::new(
        player_config,
        session.clone(),
        mixer.get_soft_volume(),
        move || {
            let sink = VynlLibrespotSink::new(build_audio_sink(backend));
            #[cfg(feature = "visualizer")]
            let sink = sink.with_pcm_tap(build_spectrum_tap(Arc::clone(&spectrum)));
            Box::new(sink)
        },
    );

    let mut player_events = player.get_player_event_channel();

    let (spirc, spirc_task) = match Spirc::new(
        connect_config,
        session,
        credentials,
        Arc::clone(&player),
        Arc::clone(&mixer),
    )
    .await
    {
        Ok(pair) => pair,
        Err(err) => {
            let _ = event_tx.send(IntegratedEvent::Error(format!(
                "librespot session/connect failed: {err}"
            )));
            player.stop();
            return;
        }
    };

    let _ = event_tx.send(IntegratedEvent::Connected { device_name });

    let event_tx_player = event_tx.clone();
    let events_task = tokio::spawn(async move {
        while let Some(event) = player_events.recv().await {
            if let Some(mapped) = map_player_event(&event)
                && event_tx_player.send(mapped).is_err()
            {
                break;
            }
        }
    });

    let event_tx_cmd = event_tx.clone();
    let player_cmd = Arc::clone(&player);
    let mixer_cmd = Arc::clone(&mixer);
    let cmd_task = tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            if apply_command(cmd, &player_cmd, &spirc, mixer_cmd.as_ref(), &event_tx_cmd) {
                break;
            }
        }
        player_cmd.stop();
        let _ = spirc.shutdown();
    });

    spirc_task.await;

    events_task.abort();
    cmd_task.abort();
    player.stop();
    let _ = event_tx.send(IntegratedEvent::Stopped);
}

fn apply_command(
    cmd: IntegratedCommand,
    player: &Player,
    spirc: &Spirc,
    mixer: &dyn Mixer,
    event_tx: &std_mpsc::Sender<IntegratedEvent>,
) -> bool {
    match cmd {
        IntegratedCommand::LoadTrack {
            spotify_uri,
            start_playing,
            position_ms,
        } => match SpotifyUri::from_uri(&spotify_uri) {
            Ok(uri) => player.load(uri, start_playing, position_ms),
            Err(err) => {
                let _ = event_tx.send(IntegratedEvent::Error(format!(
                    "invalid Spotify URI `{spotify_uri}`: {err}"
                )));
            }
        },
        IntegratedCommand::Play => player.play(),
        IntegratedCommand::Pause => player.pause(),
        IntegratedCommand::Seek(position_ms) => player.seek(position_ms),
        IntegratedCommand::SetVolume(percent) => {
            let volume = percent_to_librespot_volume(percent);
            mixer.set_volume(volume);
            player.emit_volume_changed_event(volume);
            let _ = spirc.set_volume(volume);
        }
        IntegratedCommand::Stop => player.stop(),
        IntegratedCommand::Shutdown => {
            player.stop();
            let _ = spirc.shutdown();
            return true;
        }
    }
    false
}

fn map_player_event(event: &PlayerEvent) -> Option<IntegratedEvent> {
    match event {
        PlayerEvent::Playing {
            track_id,
            position_ms,
            ..
        } => Some(IntegratedEvent::Playing {
            track_id: track_id.to_string(),
            position_ms: *position_ms,
        }),
        PlayerEvent::Paused {
            track_id,
            position_ms,
            ..
        } => Some(IntegratedEvent::Paused {
            track_id: track_id.to_string(),
            position_ms: *position_ms,
        }),
        PlayerEvent::Stopped { .. } => Some(IntegratedEvent::Stopped),
        PlayerEvent::EndOfTrack { track_id, .. } => Some(IntegratedEvent::TrackEnded {
            track_id: track_id.to_string(),
        }),
        PlayerEvent::Unavailable { track_id, .. } => Some(IntegratedEvent::Error(format!(
            "track unavailable: {track_id}"
        ))),
        _ => None,
    }
}

fn percent_to_librespot_volume(percent: u8) -> u16 {
    let clamped = u32::from(percent.min(100));
    let scaled = (clamped * u32::from(u16::MAX)) / 100;
    u16::try_from(scaled).unwrap_or(u16::MAX)
}

fn build_audio_sink(backend: AudioBackendKind) -> Box<dyn AudioSink + Send> {
    let format = AudioFormat::CD_STEREO;
    let resolved = resolve_backend(backend, CompiledPlayerFeatures::current());

    #[cfg(feature = "rodio-backend")]
    if resolved == Some(AudioBackendKind::Rodio)
        && let Ok(sink) = crate::sink::RodioAudioSink::new(format)
    {
        return Box::new(sink);
    }

    #[cfg(feature = "subprocess-backend")]
    if resolved == Some(AudioBackendKind::Subprocess)
        && let Some(sink) = try_spawn_default_subprocess(format)
    {
        return Box::new(sink);
    }

    Box::new(NullAudioSink::new(format))
}

#[cfg(feature = "subprocess-backend")]
fn try_spawn_default_subprocess(format: AudioFormat) -> Option<crate::sink::SubprocessAudioSink> {
    use crate::sink::{SubprocessAudioSink, SubprocessCommand};

    for preset in [
        SubprocessCommand::PipeWire,
        SubprocessCommand::PulseAudio,
        SubprocessCommand::Alsa,
    ] {
        if let Ok(sink) = SubprocessAudioSink::spawn(&preset, format) {
            return Some(sink);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use librespot_core::config::DeviceType;
    use librespot_playback::audio_backend::Sink;
    use librespot_playback::config::Bitrate;
    use librespot_playback::convert::Converter;
    use librespot_playback::decoder::AudioPacket;
    use vynl_core::config::{AudioBackendKind, SpotifyConfig};

    use super::{
        IntegratedPlayerConfig, IntegratedPlayerHandle, VynlLibrespotSink,
        percent_to_librespot_volume,
    };
    use crate::error::PlayerError;
    use crate::sink::{AudioFormat, NullAudioSink};

    fn unique_temp_dir(label: &str) -> Result<PathBuf, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| err.to_string())?
            .as_nanos();
        Ok(std::env::temp_dir().join(format!("vynl_{label}_{}_{stamp}", std::process::id())))
    }

    #[test]
    fn config_maps_bitrates_connect_and_session() {
        let mut spotify = SpotifyConfig {
            device_name: String::from("vynl-studio"),
            bitrate_kbps: 96,
            stream_cache_mb: 0,
            audio_backend: AudioBackendKind::Subprocess,
            ..SpotifyConfig::default()
        };

        let cfg96 = IntegratedPlayerConfig::from_spotify_config(&spotify, "/tmp/vynl-cache");
        assert_eq!(cfg96.librespot_bitrate(), Bitrate::Bitrate96);
        assert_eq!(cfg96.to_player_config().bitrate, Bitrate::Bitrate96);
        assert_eq!(cfg96.backend, AudioBackendKind::Subprocess);

        let connect = cfg96.to_connect_config();
        assert_eq!(connect.name, "vynl-studio");
        assert_eq!(connect.device_type, DeviceType::Computer);

        let session_cfg = cfg96.to_session_config();
        assert_ne!(session_cfg.device_id, "");

        spotify.bitrate_kbps = 160;
        let cfg160 = IntegratedPlayerConfig::from_spotify_config(&spotify, "/tmp/vynl-cache");
        assert_eq!(cfg160.librespot_bitrate(), Bitrate::Bitrate160);

        spotify.bitrate_kbps = 320;
        spotify.device_name = String::from("   ");
        let cfg320 = IntegratedPlayerConfig::from_spotify_config(&spotify, "/tmp/vynl-cache");
        assert_eq!(cfg320.librespot_bitrate(), Bitrate::Bitrate320);
        assert_eq!(cfg320.to_connect_config().name, "vynl");
    }

    #[test]
    fn config_cache_honors_disabled_and_enabled_modes() -> Result<(), String> {
        let dir = unique_temp_dir("librespot_cache")?;

        let disabled = IntegratedPlayerConfig {
            device_name: String::from("vynl"),
            bitrate_kbps: 320,
            cache_dir: dir.clone(),
            stream_cache_mb: 0,
            backend: AudioBackendKind::Rodio,
        };
        assert!(
            disabled
                .to_cache()
                .map_err(|err| err.to_string())?
                .is_none()
        );
        assert!(!dir.exists());

        let enabled = IntegratedPlayerConfig {
            stream_cache_mb: 32,
            ..disabled
        };
        let cache = enabled.to_cache().map_err(|err| err.to_string())?;
        assert!(cache.is_some());
        assert!(dir.is_dir());

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn librespot_sink_forwards_audio_packet_samples_to_null_sink() -> Result<(), String> {
        use std::sync::{Arc, Mutex};

        struct SpySink {
            inner: NullAudioSink,
            out: Arc<Mutex<Vec<i16>>>,
        }

        impl crate::sink::AudioSink for SpySink {
            fn write_pcm(&mut self, samples: &[i16]) -> Result<(), PlayerError> {
                self.inner.write_pcm(samples)?;
                if let Ok(mut guard) = self.out.lock() {
                    guard.extend_from_slice(self.inner.captured_samples());
                }
                self.inner.clear_captured();
                Ok(())
            }

            fn set_volume(&mut self, percent: u8) {
                self.inner.set_volume(percent);
            }

            fn pause(&mut self) {
                self.inner.pause();
            }

            fn resume(&mut self) {
                self.inner.resume();
            }

            fn flush(&mut self) {
                self.inner.flush();
            }
        }

        let shared = Arc::new(Mutex::new(Vec::<i16>::new()));
        let recorded = Arc::clone(&shared);

        let spy = SpySink {
            inner: NullAudioSink::recording(AudioFormat::CD_STEREO),
            out: recorded,
        };
        let mut sink = VynlLibrespotSink::new(Box::new(spy));

        #[cfg(feature = "visualizer")]
        let tapped = Arc::new(Mutex::new(Vec::<i16>::new()));
        #[cfg(feature = "visualizer")]
        {
            let tapped_clone = Arc::clone(&tapped);
            sink = sink.with_pcm_tap(Box::new(move |pcm| {
                if let Ok(mut guard) = tapped_clone.lock() {
                    guard.extend_from_slice(pcm);
                }
            }));
        }

        let mut converter = Converter::new(None);
        sink.start().map_err(|err| err.to_string())?;
        sink.write(
            AudioPacket::Samples(vec![0.0, 0.5, -0.5, 1.0]),
            &mut converter,
        )
        .map_err(|err| err.to_string())?;
        assert_eq!(sink.samples_forwarded(), 4);

        let samples = shared.lock().map_err(|err| err.to_string())?.clone();
        assert_eq!(samples, vec![0_i16, 16_384, -16_384, i16::MAX]);

        #[cfg(feature = "visualizer")]
        {
            let tapped_samples = tapped.lock().map_err(|err| err.to_string())?.clone();
            assert_eq!(tapped_samples, samples);
        }

        let raw_err = sink.write(AudioPacket::Raw(vec![1, 2, 3, 4]), &mut converter);
        assert!(raw_err.is_err());

        sink.stop().map_err(|err| err.to_string())?;
        let _inner = sink.into_inner();
        Ok(())
    }

    #[test]
    fn volume_conversion_and_empty_token_rejection() {
        assert_eq!(percent_to_librespot_volume(0), 0);
        assert_eq!(percent_to_librespot_volume(100), u16::MAX);
        assert_eq!(percent_to_librespot_volume(200), u16::MAX);

        let cfg = IntegratedPlayerConfig::from_spotify_config(
            &SpotifyConfig {
                stream_cache_mb: 0,
                ..SpotifyConfig::default()
            },
            "/tmp/vynl-unused",
        );
        assert!(matches!(
            IntegratedPlayerHandle::spawn_with_access_token(cfg, String::from("   ")),
            Err(PlayerError::InvalidState(_))
        ));
    }

    #[cfg(feature = "visualizer")]
    #[test]
    fn pcm_tap_feeds_440hz_sine_into_spectrum_analyzer() -> Result<(), String> {
        use std::f64::consts::PI;
        use std::sync::{Arc, Mutex};

        use super::{ZEN_SPECTRUM_BANDS, build_spectrum_tap};
        use crate::visualizer::SpectrumAnalyzer;

        let format = AudioFormat::CD_STEREO;
        let fft_size = SpectrumAnalyzer::DEFAULT_FFT_SIZE;
        let analyzer = Arc::new(Mutex::new(
            SpectrumAnalyzer::new(
                fft_size,
                ZEN_SPECTRUM_BANDS,
                format.sample_rate,
                format.channels,
            )
            .map_err(|err| err.to_string())?,
        ));

        let mut sink = VynlLibrespotSink::new(Box::new(NullAudioSink::new(format)))
            .with_pcm_tap(build_spectrum_tap(Arc::clone(&analyzer)));

        let mut stereo_samples = Vec::with_capacity(fft_size * usize::from(format.channels));
        for frame in 0..fft_size {
            #[allow(clippy::cast_precision_loss)]
            let t = (frame as f64) / f64::from(format.sample_rate);
            let value = (2.0 * PI * 440.0 * t).sin() * 0.85;
            stereo_samples.push(value);
            stereo_samples.push(value);
        }

        let mut converter = Converter::new(None);
        sink.start().map_err(|err| err.to_string())?;
        sink.write(AudioPacket::Samples(stereo_samples), &mut converter)
            .map_err(|err| err.to_string())?;
        sink.stop().map_err(|err| err.to_string())?;

        let guard = analyzer.lock().map_err(|err| err.to_string())?;
        let bars = guard.bars();
        assert_eq!(bars.len(), ZEN_SPECTRUM_BANDS);
        assert!(bars.iter().all(|&bar| (0.0..=1.0).contains(&bar)));
        assert!(
            bars.iter().any(|&bar| bar > 0.5),
            "expected non-zero spectrum bar from 440 Hz sine wave, got {bars:?}"
        );

        let bin_width_hz = f64::from(format.sample_rate) / 1024.0;
        let peak_hz = guard.peak_frequency_hz();
        assert!(
            (peak_hz - 440.0).abs() <= bin_width_hz,
            "expected peak near 440 Hz, got {peak_hz} Hz"
        );
        Ok(())
    }
}
