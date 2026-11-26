//! Audio sink abstractions and pure-Rust PCM output backends.
//!
//! Provides:
//! - [`AudioFormat`] and [`AudioSink`] trait for writing interleaved signed 16-bit PCM.
//! - [`NullAudioSink`] for remote-only builds and deterministic unit tests.
//! - [`SubprocessAudioSink`] (when `subprocess-backend` is enabled) streaming little-endian
//!   signed 16-bit PCM over `stdin` to `pw-play`, `paplay`, `aplay`, or a custom command.
//! - [`RodioAudioSink`] (when `rodio-backend` is enabled) backed by a dedicated `rodio`
//!   hardware audio thread with graceful headless staging fallback.

use crate::error::PlayerError;

#[cfg(feature = "rodio-backend")]
use std::sync::mpsc::{self, SyncSender};
#[cfg(feature = "rodio-backend")]
use std::thread::{self, JoinHandle};

#[cfg(feature = "subprocess-backend")]
use std::io::Write;
#[cfg(feature = "subprocess-backend")]
use std::process::{Child, ChildStdin, Command, Stdio};

/// Interleaved signed 16-bit PCM audio stream parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// Sampling rate in Hz (e.g., `44_100` or `48_000`).
    pub sample_rate: u32,
    /// Number of interleaved audio channels (e.g., `1` for mono, `2` for stereo).
    pub channels: u16,
}

impl AudioFormat {
    /// Standard CD / Spotify stereo format: 44.1 kHz, 2 channels.
    pub const CD_STEREO: Self = Self {
        sample_rate: 44_100,
        channels: 2,
    };

    /// Creates a new [`AudioFormat`] descriptor.
    #[must_use]
    pub const fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
        }
    }

    /// Returns `true` when both `sample_rate` and `channels` are strictly positive.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.sample_rate > 0 && self.channels > 0
    }

    /// Number of bytes per interleaved frame (`channels * 2` for `i16` PCM).
    #[must_use]
    pub const fn bytes_per_frame(self) -> usize {
        (self.channels as usize) * 2
    }

    /// Byte throughput per second for signed 16-bit PCM.
    #[must_use]
    pub const fn bytes_per_second(self) -> u64 {
        (self.sample_rate as u64) * (self.channels as u64) * 2
    }
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self::CD_STEREO
    }
}

/// Output sink receiving interleaved little-endian signed 16-bit PCM samples.
pub trait AudioSink {
    /// Writes a slice of interleaved signed 16-bit PCM samples to the output backend.
    ///
    /// # Errors
    /// Returns [`PlayerError`] if the underlying audio device or pipe fails.
    fn write_pcm(&mut self, samples: &[i16]) -> Result<(), PlayerError>;

    /// Sets output volume as a percentage (`0..=100`; values above 100 are clamped).
    fn set_volume(&mut self, percent: u8);

    /// Pauses audio playback.
    fn pause(&mut self);

    /// Resumes audio playback after pausing.
    fn resume(&mut self);

    /// Flushes any buffered PCM samples immediately.
    fn flush(&mut self);
}

/// Scales a single signed 16-bit PCM sample by a percentage volume (`0..=100`).
#[must_use]
pub fn scale_pcm_volume(sample: i16, volume_percent: u8) -> i16 {
    let clamped = i32::from(volume_percent.min(100));
    if clamped == 100 {
        return sample;
    }
    if clamped == 0 {
        return 0;
    }
    let scaled = (i32::from(sample) * clamped) / 100;
    i16::try_from(scaled).unwrap_or(if scaled < 0 { i16::MIN } else { i16::MAX })
}

/// Scales a single signed 16-bit PCM sample by a linear floating-point gain with saturation.
#[must_use]
pub fn scale_pcm_sample(sample: i16, gain: f32) -> i16 {
    if !gain.is_finite() || gain <= 0.0 {
        return 0;
    }
    let scaled = (f64::from(sample) * f64::from(gain)).round();
    let min = f64::from(i16::MIN);
    let max = f64::from(i16::MAX);
    let clamped = scaled.clamp(min, max);
    #[allow(clippy::cast_possible_truncation)]
    let out = clamped as i16;
    out
}

/// Silent no-op audio sink used for remote-only builds and deterministic unit tests.
#[derive(Debug, Clone)]
pub struct NullAudioSink {
    format: AudioFormat,
    volume_percent: u8,
    paused: bool,
    samples_written: u64,
    flush_count: u64,
    record_samples: bool,
    captured: Vec<i16>,
}

impl NullAudioSink {
    /// Creates a new silent [`NullAudioSink`] that counts written samples without retaining them.
    #[must_use]
    pub const fn new(format: AudioFormat) -> Self {
        Self {
            format,
            volume_percent: 100,
            paused: false,
            samples_written: 0,
            flush_count: 0,
            record_samples: false,
            captured: Vec::new(),
        }
    }

    /// Creates a recording [`NullAudioSink`] that retains volume-scaled PCM samples for inspection.
    #[must_use]
    pub const fn recording(format: AudioFormat) -> Self {
        Self {
            format,
            volume_percent: 100,
            paused: false,
            samples_written: 0,
            flush_count: 0,
            record_samples: true,
            captured: Vec::new(),
        }
    }

    /// Returns the configured [`AudioFormat`].
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// Returns the current volume percentage (`0..=100`).
    #[must_use]
    pub const fn volume_percent(&self) -> u8 {
        self.volume_percent
    }

    /// Returns `true` if the sink is currently paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    /// Returns the total number of samples accepted while unpaused.
    #[must_use]
    pub const fn samples_written(&self) -> u64 {
        self.samples_written
    }

    /// Returns how many times [`AudioSink::flush`] has been invoked.
    #[must_use]
    pub const fn flush_count(&self) -> u64 {
        self.flush_count
    }

    /// Returns the captured volume-scaled PCM samples (populated when created via [`Self::recording`]).
    #[must_use]
    pub fn captured_samples(&self) -> &[i16] {
        &self.captured
    }

    /// Clears captured samples without modifying counters.
    pub fn clear_captured(&mut self) {
        self.captured.clear();
    }
}

impl Default for NullAudioSink {
    fn default() -> Self {
        Self::new(AudioFormat::default())
    }
}

impl AudioSink for NullAudioSink {
    fn write_pcm(&mut self, samples: &[i16]) -> Result<(), PlayerError> {
        if !self.format.is_valid() {
            return Err(PlayerError::InvalidFormat(format!(
                "invalid PCM format: {} Hz, {} ch",
                self.format.sample_rate, self.format.channels
            )));
        }
        if self.paused {
            return Ok(());
        }
        let count = u64::try_from(samples.len()).unwrap_or(u64::MAX);
        self.samples_written = self.samples_written.saturating_add(count);
        if self.record_samples {
            self.captured.reserve(samples.len());
            for &sample in samples {
                self.captured
                    .push(scale_pcm_volume(sample, self.volume_percent));
            }
        }
        Ok(())
    }

    fn set_volume(&mut self, percent: u8) {
        self.volume_percent = percent.min(100);
    }

    fn pause(&mut self) {
        self.paused = true;
    }

    fn resume(&mut self) {
        self.paused = false;
    }

    fn flush(&mut self) {
        self.flush_count = self.flush_count.saturating_add(1);
        self.captured.clear();
    }
}

#[cfg(feature = "rodio-backend")]
#[derive(Debug)]
enum RodioCommand {
    WritePcm(Vec<f32>),
    SetVolume(f32),
    Pause,
    Resume,
    Flush { paused: bool },
    Shutdown,
}

#[cfg(feature = "rodio-backend")]
#[derive(Debug)]
struct RodioWorker {
    tx: Option<SyncSender<RodioCommand>>,
    thread: Option<JoinHandle<()>>,
}

#[cfg(feature = "rodio-backend")]
impl RodioWorker {
    const CHANNEL_CAPACITY: usize = 64;

    fn spawn(format: AudioFormat) -> Result<Self, PlayerError> {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<RodioCommand>(Self::CHANNEL_CAPACITY);
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        let handle = thread::Builder::new()
            .name(String::from("vynl-rodio-sink"))
            .spawn(move || {
                let mut stream = match rodio::OutputStreamBuilder::open_default_stream() {
                    Ok(stream) => stream,
                    Err(err) => {
                        let _ = init_tx.send(Err(err.to_string()));
                        return;
                    }
                };
                // Suppress stderr drop diagnostics so the TUI viewport stays clean.
                stream.log_on_drop(false);
                let sink = rodio::Sink::connect_new(stream.mixer());

                if init_tx.send(Ok(())).is_err() {
                    sink.stop();
                    return;
                }

                while let Ok(cmd) = cmd_rx.recv() {
                    match cmd {
                        RodioCommand::WritePcm(samples) => {
                            if !samples.is_empty() {
                                let source = rodio::buffer::SamplesBuffer::new(
                                    format.channels,
                                    format.sample_rate,
                                    samples,
                                );
                                sink.append(source);
                            }
                        }
                        RodioCommand::SetVolume(volume) => {
                            sink.set_volume(volume);
                        }
                        RodioCommand::Pause => {
                            sink.pause();
                        }
                        RodioCommand::Resume => {
                            sink.play();
                        }
                        RodioCommand::Flush { paused } => {
                            sink.clear();
                            if !paused {
                                sink.play();
                            }
                        }
                        RodioCommand::Shutdown => {
                            sink.stop();
                            break;
                        }
                    }
                }
            })
            .map_err(|err| {
                PlayerError::BackendUnavailable(format!("failed to spawn rodio thread: {err}"))
            })?;

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx: Some(cmd_tx),
                thread: Some(handle),
            }),
            Ok(Err(reason)) => {
                let _ = handle.join();
                Err(PlayerError::BackendUnavailable(format!(
                    "failed to open default rodio audio stream: {reason}"
                )))
            }
            Err(_) => {
                let _ = handle.join();
                Err(PlayerError::BackendUnavailable(String::from(
                    "rodio audio thread terminated during initialization",
                )))
            }
        }
    }

    fn send(&self, cmd: RodioCommand) -> Result<(), PlayerError> {
        let Some(tx) = self.tx.as_ref() else {
            return Err(PlayerError::BackendUnavailable(String::from(
                "rodio audio worker channel closed",
            )));
        };
        tx.send(cmd).map_err(|_| {
            PlayerError::BackendUnavailable(String::from("rodio audio worker thread terminated"))
        })
    }
}

#[cfg(feature = "rodio-backend")]
impl Drop for RodioWorker {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.try_send(RodioCommand::Shutdown);
            drop(tx);
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(feature = "rodio-backend")]
fn validate_rodio_format(format: AudioFormat) -> Result<(), PlayerError> {
    if !format.is_valid() {
        return Err(PlayerError::InvalidFormat(format!(
            "invalid PCM format: {} Hz, {} ch",
            format.sample_rate, format.channels
        )));
    }
    Ok(())
}

#[cfg(feature = "rodio-backend")]
fn i16_to_f32_pcm(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

/// Cross-platform `rodio`/`cpal` audio sink backed by a dedicated audio worker thread.
///
/// Compiled when the `rodio-backend` feature is enabled. The underlying `rodio::OutputStream`
/// and `rodio::Sink` live on a dedicated OS thread so [`RodioAudioSink`] remains `Send` on
/// every platform.
#[cfg(feature = "rodio-backend")]
#[derive(Debug)]
pub struct RodioAudioSink {
    format: AudioFormat,
    volume_percent: u8,
    paused: bool,
    samples_written: u64,
    buffer: Vec<i16>,
    max_buffer_samples: usize,
    worker: Option<RodioWorker>,
}

#[cfg(feature = "rodio-backend")]
impl RodioAudioSink {
    /// Default staged buffer capacity (roughly 100 ms at 44.1 kHz stereo).
    pub const DEFAULT_BUFFER_SAMPLES: usize = 8_820;

    /// Opens a real hardware-backed `rodio` output stream on the system's default audio device.
    ///
    /// # Errors
    /// Returns [`PlayerError::InvalidFormat`] if `format` has a zero sample rate or channel count,
    /// or [`PlayerError::BackendUnavailable`] if no audio output device is available.
    pub fn open_default(format: AudioFormat) -> Result<Self, PlayerError> {
        validate_rodio_format(format)?;
        let worker = RodioWorker::spawn(format)?;
        Ok(Self {
            format,
            volume_percent: 100,
            paused: false,
            samples_written: 0,
            buffer: Vec::with_capacity(Self::DEFAULT_BUFFER_SAMPLES),
            max_buffer_samples: Self::DEFAULT_BUFFER_SAMPLES,
            worker: Some(worker),
        })
    }

    /// Creates a new [`RodioAudioSink`] for the given [`AudioFormat`].
    ///
    /// Attempts to open the system's default `rodio` hardware output stream and
    /// gracefully falls back to an in-memory staging buffer when running in a
    /// headless environment without an audio output device.
    ///
    /// # Errors
    /// Returns [`PlayerError::InvalidFormat`] if `format` has a zero sample rate or channel count.
    pub fn new(format: AudioFormat) -> Result<Self, PlayerError> {
        validate_rodio_format(format)?;
        match Self::open_default(format) {
            Ok(sink) => Ok(sink),
            Err(PlayerError::BackendUnavailable(_)) => Ok(Self {
                format,
                volume_percent: 100,
                paused: false,
                samples_written: 0,
                buffer: Vec::with_capacity(Self::DEFAULT_BUFFER_SAMPLES),
                max_buffer_samples: Self::DEFAULT_BUFFER_SAMPLES,
                worker: None,
            }),
            Err(err) => Err(err),
        }
    }

    /// Returns `true` when backed by an active `rodio` hardware output stream.
    #[must_use]
    pub const fn is_hardware_active(&self) -> bool {
        self.worker.is_some()
    }

    /// Returns the configured [`AudioFormat`].
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// Returns the current volume percentage (`0..=100`).
    #[must_use]
    pub const fn volume_percent(&self) -> u8 {
        self.volume_percent
    }

    /// Returns `true` when paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    /// Returns total samples processed while unpaused.
    #[must_use]
    pub const fn samples_written(&self) -> u64 {
        self.samples_written
    }

    /// Returns currently staged samples awaiting hardware drain.
    #[must_use]
    pub fn staged_samples(&self) -> &[i16] {
        &self.buffer
    }
}

#[cfg(feature = "rodio-backend")]
impl AudioSink for RodioAudioSink {
    fn write_pcm(&mut self, samples: &[i16]) -> Result<(), PlayerError> {
        if self.paused {
            return Ok(());
        }
        let count = u64::try_from(samples.len()).unwrap_or(u64::MAX);
        self.samples_written = self.samples_written.saturating_add(count);

        self.buffer.clear();
        let take_from = samples.len().saturating_sub(self.max_buffer_samples);
        for &sample in &samples[take_from..] {
            self.buffer
                .push(scale_pcm_volume(sample, self.volume_percent));
        }

        if let Some(worker) = self.worker.as_ref()
            && !samples.is_empty()
        {
            let samples_f32: Vec<f32> = samples.iter().copied().map(i16_to_f32_pcm).collect();
            worker.send(RodioCommand::WritePcm(samples_f32))?;
        }
        Ok(())
    }

    fn set_volume(&mut self, percent: u8) {
        self.volume_percent = percent.min(100);
        if let Some(worker) = self.worker.as_ref() {
            let linear = f32::from(self.volume_percent) / 100.0;
            let _ = worker.send(RodioCommand::SetVolume(linear));
        }
    }

    fn pause(&mut self) {
        self.paused = true;
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.send(RodioCommand::Pause);
        }
    }

    fn resume(&mut self) {
        self.paused = false;
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.send(RodioCommand::Resume);
        }
    }

    fn flush(&mut self) {
        self.buffer.clear();
        if let Some(worker) = self.worker.as_ref() {
            let _ = worker.send(RodioCommand::Flush {
                paused: self.paused,
            });
        }
    }
}

/// Configured external PCM player command for [`SubprocessAudioSink`].
#[cfg(feature = "subprocess-backend")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubprocessCommand {
    /// `PipeWire` raw PCM playback via `pw-play`.
    PipeWire,
    /// `PulseAudio` raw PCM playback via `paplay`.
    PulseAudio,
    /// Linux ALSA raw PCM playback via `aplay`.
    Alsa,
    /// Custom command and arguments accepting raw little-endian signed 16-bit PCM on `stdin`.
    Custom {
        /// Executable name or path.
        program: String,
        /// Command-line arguments passed to the executable.
        args: Vec<String>,
    },
}

#[cfg(feature = "subprocess-backend")]
impl SubprocessCommand {
    /// Returns the executable program name.
    #[must_use]
    pub fn program(&self) -> &str {
        match self {
            Self::PipeWire => "pw-play",
            Self::PulseAudio => "paplay",
            Self::Alsa => "aplay",
            Self::Custom { program, .. } => program.as_str(),
        }
    }

    /// Builds the command-line arguments for the requested [`AudioFormat`].
    #[must_use]
    pub fn args_for_format(&self, format: AudioFormat) -> Vec<String> {
        match self {
            Self::PipeWire => vec![
                String::from("--raw"),
                String::from("--format=s16"),
                format!("--rate={}", format.sample_rate),
                format!("--channels={}", format.channels),
                String::from("-"),
            ],
            Self::PulseAudio => vec![
                String::from("--raw"),
                String::from("--format=s16le"),
                format!("--rate={}", format.sample_rate),
                format!("--channels={}", format.channels),
            ],
            Self::Alsa => vec![
                String::from("-q"),
                String::from("-t"),
                String::from("raw"),
                String::from("-f"),
                String::from("S16_LE"),
                String::from("-r"),
                format.sample_rate.to_string(),
                String::from("-c"),
                format.channels.to_string(),
            ],
            Self::Custom { args, .. } => args.clone(),
        }
    }
}

/// Pure-Rust PCM pipe audio sink spawning an external player command.
///
/// Streams little-endian signed 16-bit PCM bytes into the child process's `stdin`
/// using zero `unsafe` code, making static `musl`, FreeBSD, and exotic targets
/// compile without native audio C libraries.
#[cfg(feature = "subprocess-backend")]
#[derive(Debug)]
pub struct SubprocessAudioSink {
    command_label: String,
    format: AudioFormat,
    child: Child,
    stdin: Option<ChildStdin>,
    volume_percent: u8,
    paused: bool,
    byte_buf: Vec<u8>,
}

#[cfg(feature = "subprocess-backend")]
impl SubprocessAudioSink {
    /// Spawns the configured subprocess with `Stdio::piped()` standard input.
    ///
    /// # Errors
    /// Returns [`PlayerError::InvalidFormat`] if `format` is invalid,
    /// [`PlayerError::BackendUnavailable`] if the child does not expose `stdin`,
    /// or [`PlayerError::Io`] if spawning the command fails.
    pub fn spawn(command: &SubprocessCommand, format: AudioFormat) -> Result<Self, PlayerError> {
        if !format.is_valid() {
            return Err(PlayerError::InvalidFormat(format!(
                "invalid PCM format: {} Hz, {} ch",
                format.sample_rate, format.channels
            )));
        }

        let program = command.program();
        if program.trim().is_empty() {
            return Err(PlayerError::BackendUnavailable(String::from(
                "subprocess program name must not be empty",
            )));
        }

        let args = command.args_for_format(format);
        let mut child = Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PlayerError::BackendUnavailable(format!(
                "failed to attach stdin pipe for `{program}`"
            )));
        };

        Ok(Self {
            command_label: program.to_owned(),
            format,
            child,
            stdin: Some(stdin),
            volume_percent: 100,
            paused: false,
            byte_buf: Vec::with_capacity(4096),
        })
    }

    /// Returns the configured [`AudioFormat`].
    #[must_use]
    pub const fn format(&self) -> AudioFormat {
        self.format
    }

    /// Returns the spawned executable name.
    #[must_use]
    pub fn command_label(&self) -> &str {
        &self.command_label
    }

    /// Returns the current software volume percentage (`0..=100`).
    #[must_use]
    pub const fn volume_percent(&self) -> u8 {
        self.volume_percent
    }

    /// Returns `true` when paused.
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    /// Closes the PCM pipe cleanly and waits for the child process to exit.
    ///
    /// # Errors
    /// Returns [`PlayerError::Io`] if waiting on the child process fails.
    pub fn shutdown(mut self) -> Result<Option<i32>, PlayerError> {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.flush();
        }
        let status = self.child.wait()?;
        Ok(status.code())
    }
}

#[cfg(feature = "subprocess-backend")]
impl AudioSink for SubprocessAudioSink {
    fn write_pcm(&mut self, samples: &[i16]) -> Result<(), PlayerError> {
        if self.paused || samples.is_empty() {
            return Ok(());
        }

        let Some(stdin) = self.stdin.as_mut() else {
            return Err(PlayerError::SubprocessExited {
                command: self.command_label.clone(),
                status: None,
            });
        };

        self.byte_buf.clear();
        self.byte_buf.reserve(samples.len().saturating_mul(2));
        for &sample in samples {
            let scaled = scale_pcm_volume(sample, self.volume_percent);
            self.byte_buf.extend_from_slice(&scaled.to_le_bytes());
        }

        if let Err(err) = stdin.write_all(&self.byte_buf) {
            if let Ok(Some(exit_status)) = self.child.try_wait() {
                return Err(PlayerError::SubprocessExited {
                    command: self.command_label.clone(),
                    status: exit_status.code(),
                });
            }
            return Err(PlayerError::Io(err));
        }

        Ok(())
    }

    fn set_volume(&mut self, percent: u8) {
        self.volume_percent = percent.min(100);
    }

    fn pause(&mut self) {
        self.paused = true;
    }

    fn resume(&mut self) {
        self.paused = false;
    }

    fn flush(&mut self) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.flush();
        }
    }
}

#[cfg(feature = "subprocess-backend")]
impl Drop for SubprocessAudioSink {
    fn drop(&mut self) {
        // Drop stdin first so well-behaved audio tools exit cleanly on EOF.
        drop(self.stdin.take());
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioFormat, AudioSink, NullAudioSink, scale_pcm_sample, scale_pcm_volume};

    #[test]
    fn audio_format_calculates_frame_and_byte_rates() {
        let fmt = AudioFormat::CD_STEREO;
        assert!(fmt.is_valid());
        assert_eq!(fmt.bytes_per_frame(), 4);
        assert_eq!(fmt.bytes_per_second(), 176_400);
        assert!(!AudioFormat::new(0, 2).is_valid());
        assert!(!AudioFormat::new(44_100, 0).is_valid());
    }

    #[test]
    fn null_audio_sink_tracks_volume_pause_and_flush() -> Result<(), String> {
        let mut sink = NullAudioSink::recording(AudioFormat::CD_STEREO);
        assert_eq!(sink.volume_percent(), 100);
        assert!(!sink.is_paused());

        sink.set_volume(50);
        assert_eq!(sink.volume_percent(), 50);
        sink.write_pcm(&[1_000, -2_000, 400, -400])
            .map_err(|e| e.to_string())?;
        assert_eq!(sink.samples_written(), 4);
        assert_eq!(sink.captured_samples(), &[500, -1_000, 200, -200]);

        sink.pause();
        assert!(sink.is_paused());
        sink.write_pcm(&[10_000, 10_000])
            .map_err(|e| e.to_string())?;
        assert_eq!(sink.samples_written(), 4);

        sink.resume();
        sink.set_volume(150);
        assert_eq!(sink.volume_percent(), 100);
        sink.flush();
        assert_eq!(sink.flush_count(), 1);
        assert_eq!(sink.captured_samples(), &[] as &[i16]);
        Ok(())
    }

    #[test]
    fn null_audio_sink_rejects_invalid_format() {
        let mut sink = NullAudioSink::new(AudioFormat::new(0, 2));
        assert!(sink.write_pcm(&[1, 2]).is_err());
    }

    #[test]
    fn pcm_scaling_clamps_without_overflow() {
        assert_eq!(scale_pcm_volume(i16::MAX, 100), i16::MAX);
        assert_eq!(scale_pcm_volume(i16::MIN, 0), 0);
        assert_eq!(scale_pcm_volume(1_000, 25), 250);
        assert_eq!(scale_pcm_sample(20_000, 3.0), i16::MAX);
        assert_eq!(scale_pcm_sample(-20_000, 3.0), i16::MIN);
        assert_eq!(scale_pcm_sample(10_000, f32::NAN), 0);
    }

    #[cfg(feature = "subprocess-backend")]
    #[test]
    fn subprocess_command_builds_expected_preset_args() {
        use super::SubprocessCommand;

        let fmt = AudioFormat::new(48_000, 2);
        assert_eq!(SubprocessCommand::PipeWire.program(), "pw-play");
        assert_eq!(
            SubprocessCommand::PipeWire.args_for_format(fmt),
            vec!["--raw", "--format=s16", "--rate=48000", "--channels=2", "-"]
        );
        assert_eq!(SubprocessCommand::PulseAudio.program(), "paplay");
        assert_eq!(
            SubprocessCommand::PulseAudio.args_for_format(fmt),
            vec!["--raw", "--format=s16le", "--rate=48000", "--channels=2"]
        );
        assert_eq!(SubprocessCommand::Alsa.program(), "aplay");
        assert_eq!(
            SubprocessCommand::Alsa.args_for_format(fmt),
            vec!["-q", "-t", "raw", "-f", "S16_LE", "-r", "48000", "-c", "2"]
        );
    }

    #[cfg(all(feature = "subprocess-backend", unix))]
    #[test]
    fn subprocess_audio_sink_streams_little_endian_pcm() -> Result<(), String> {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        use super::{SubprocessAudioSink, SubprocessCommand};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let out_path = std::env::temp_dir().join(format!(
            "vynl_subproc_sink_{}_{stamp}.pcm",
            std::process::id()
        ));
        let cmd = SubprocessCommand::Custom {
            program: String::from("sh"),
            args: vec![
                String::from("-c"),
                format!("cat > \"{}\"", out_path.display()),
            ],
        };

        let mut sink =
            SubprocessAudioSink::spawn(&cmd, AudioFormat::CD_STEREO).map_err(|e| e.to_string())?;
        sink.set_volume(50);
        sink.write_pcm(&[2_000, -4_000, 0, 1_000])
            .map_err(|e| e.to_string())?;
        sink.pause();
        sink.write_pcm(&[30_000, -30_000])
            .map_err(|e| e.to_string())?;
        sink.resume();
        sink.flush();

        let code = sink.shutdown().map_err(|e| e.to_string())?;
        assert_eq!(code, Some(0));

        let bytes = fs::read(&out_path).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&out_path);

        let expected: Vec<u8> = [1_000_i16, -2_000_i16, 0_i16, 500_i16]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        assert_eq!(bytes, expected);
        Ok(())
    }

    #[cfg(feature = "rodio-backend")]
    #[test]
    fn rodio_audio_sink_is_send_and_handles_lifecycle() -> Result<(), String> {
        use super::RodioAudioSink;
        use crate::error::PlayerError;

        fn assert_send<T: Send>() {}
        assert_send::<RodioAudioSink>();

        assert!(matches!(
            RodioAudioSink::new(AudioFormat::new(0, 2)),
            Err(PlayerError::InvalidFormat(_))
        ));
        assert!(matches!(
            RodioAudioSink::open_default(AudioFormat::new(44_100, 0)),
            Err(PlayerError::InvalidFormat(_))
        ));

        let mut sink = RodioAudioSink::new(AudioFormat::CD_STEREO).map_err(|e| e.to_string())?;
        assert_eq!(sink.format(), AudioFormat::CD_STEREO);
        assert_eq!(sink.volume_percent(), 100);
        assert!(!sink.is_paused());
        let _hardware = sink.is_hardware_active();

        sink.set_volume(50);
        assert_eq!(sink.volume_percent(), 50);
        sink.write_pcm(&[1_000, -2_000, 400, -400])
            .map_err(|e| e.to_string())?;
        assert_eq!(sink.samples_written(), 4);
        assert_eq!(sink.staged_samples(), &[500, -1_000, 200, -200]);

        sink.pause();
        assert!(sink.is_paused());
        sink.write_pcm(&[8_000, -8_000])
            .map_err(|e| e.to_string())?;
        assert_eq!(sink.samples_written(), 4);

        sink.resume();
        assert!(!sink.is_paused());
        sink.flush();
        assert_eq!(sink.staged_samples(), &[] as &[i16]);
        Ok(())
    }
}
