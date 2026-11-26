//! OS media-control bridge abstractions (MPRIS2 / SMTC / `MediaRemote`) for `vynl`.
//!
//! Provides a platform-neutral [`MediaControlBridge`] trait, an in-memory
//! [`LocalMediaBridge`] tracker, and a live [`SystemMediaBridge`] wired to native
//! OS media controls via `souvlaki` (MPRIS2 `zbus` on Linux, `MediaRemote` on macOS,
//! and graceful in-memory degradation when a native session is unavailable).

use std::fmt;
use std::sync::mpsc;

use vynl_core::model::{PlaybackState, TrackSummary};

/// Platform media-control backend compiled for the current target OS.
#[must_use]
pub const fn media_control_backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "MPRIS2 (D-Bus / zbus)"
    } else if cfg!(target_os = "macos") {
        "macOS MediaRemote"
    } else if cfg!(target_os = "windows") {
        "Windows SMTC"
    } else {
        "none (unsupported OS)"
    }
}

/// Playback status exposed to OS media controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MediaPlaybackStatus {
    /// No track loaded or playback stopped.
    #[default]
    Stopped,
    /// Audio is actively playing.
    Playing,
    /// Track is loaded and paused.
    Paused,
}

impl MediaPlaybackStatus {
    /// Returns the MPRIS2 `PlaybackStatus` property value.
    #[must_use]
    pub const fn as_mpris_str(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
        }
    }
}

/// Track metadata exported to MPRIS2, Windows SMTC, or macOS `MediaRemote`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaMetadata {
    /// Spotify track identifier (used for MPRIS2 `mpris:trackid`).
    pub track_id: String,
    /// Track display title.
    pub title: String,
    /// Artist display names.
    pub artists: Vec<String>,
    /// Album title.
    pub album: String,
    /// Track duration in milliseconds.
    pub duration_ms: u64,
    /// Optional artwork URL or local file URI.
    pub cover_url: Option<String>,
}

impl MediaMetadata {
    /// Builds [`MediaMetadata`] from a core [`TrackSummary`].
    #[must_use]
    pub fn from_track(track: &TrackSummary) -> Self {
        Self {
            track_id: track.id.clone(),
            title: track.title.clone(),
            artists: track.artists.clone(),
            album: track.album.clone(),
            duration_ms: track.duration_ms,
            cover_url: track.cover.as_ref().map(|cover| cover.url.clone()),
        }
    }
}

/// Media key or OS transport command received from the host platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaControlEvent {
    /// Start or resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Toggle play/pause.
    PlayPause,
    /// Skip to the next track.
    Next,
    /// Skip to the previous track.
    Previous,
    /// Stop playback.
    Stop,
    /// Relative seek offset in milliseconds.
    SeekByMs(i64),
    /// Absolute volume percentage (`0..=100`).
    SetVolume(u8),
}

/// Trait implemented by OS media-control bridges (MPRIS2, SMTC, `MediaRemote`).
pub trait MediaControlBridge {
    /// Human-readable name of the active OS media-control backend.
    fn backend_name(&self) -> &'static str;

    /// Updates the currently exposed track metadata (`None` clears metadata).
    fn update_metadata(&mut self, metadata: Option<MediaMetadata>);

    /// Updates the OS-visible playback status and track position in milliseconds.
    fn update_playback_state(&mut self, status: MediaPlaybackStatus, position_ms: u64);
}

/// In-memory OS media-control state bridge ready to bind D-Bus / SMTC / `MediaRemote` handles.
///
/// Tracks metadata, playback status, position, volume, navigation capabilities, and a
/// monotonically increasing `revision` counter so native OS adapters only emit property
/// change signals when state actually changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMediaBridge {
    backend: &'static str,
    metadata: Option<MediaMetadata>,
    status: MediaPlaybackStatus,
    position_ms: u64,
    volume_percent: u8,
    can_go_next: bool,
    can_go_previous: bool,
    revision: u64,
}

impl Default for LocalMediaBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalMediaBridge {
    /// Creates a new [`LocalMediaBridge`] initialized for the current target OS.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            backend: media_control_backend_name(),
            metadata: None,
            status: MediaPlaybackStatus::Stopped,
            position_ms: 0,
            volume_percent: 100,
            can_go_next: true,
            can_go_previous: true,
            revision: 0,
        }
    }

    /// Returns the currently exported track metadata, if any.
    #[must_use]
    pub const fn metadata(&self) -> Option<&MediaMetadata> {
        self.metadata.as_ref()
    }

    /// Returns the current playback status.
    #[must_use]
    pub const fn status(&self) -> MediaPlaybackStatus {
        self.status
    }

    /// Returns the last reported playback position in milliseconds.
    #[must_use]
    pub const fn position_ms(&self) -> u64 {
        self.position_ms
    }

    /// Returns the last reported volume percentage (`0..=100`).
    #[must_use]
    pub const fn volume_percent(&self) -> u8 {
        self.volume_percent
    }

    /// Returns whether next-track navigation is enabled.
    #[must_use]
    pub const fn can_go_next(&self) -> bool {
        self.can_go_next
    }

    /// Returns whether previous-track navigation is enabled.
    #[must_use]
    pub const fn can_go_previous(&self) -> bool {
        self.can_go_previous
    }

    /// Returns the monotonic state revision incremented on every state mutation.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Updates track navigation capabilities reported to the OS.
    pub fn set_navigation_capabilities(&mut self, can_go_next: bool, can_go_previous: bool) {
        if self.can_go_next != can_go_next || self.can_go_previous != can_go_previous {
            self.can_go_next = can_go_next;
            self.can_go_previous = can_go_previous;
            self.bump_revision();
        }
    }

    /// Updates the OS-visible volume percentage, clamping values above `100`.
    pub fn set_volume_percent(&mut self, volume_percent: u8) {
        let clamped = volume_percent.min(100);
        if self.volume_percent != clamped {
            self.volume_percent = clamped;
            self.bump_revision();
        }
    }

    /// Synchronizes metadata, playback status, position, and volume from [`PlaybackState`].
    pub fn sync_from_playback_state(&mut self, playback: &PlaybackState) {
        let metadata = playback
            .current_track
            .as_ref()
            .map(MediaMetadata::from_track);
        self.update_metadata(metadata);

        let status = if playback.current_track.is_none() {
            MediaPlaybackStatus::Stopped
        } else if playback.is_playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        };
        self.update_playback_state(status, playback.position_ms);

        let effective_volume = if playback.muted {
            0
        } else {
            playback.volume_percent
        };
        self.set_volume_percent(effective_volume);
    }

    const fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

impl MediaControlBridge for LocalMediaBridge {
    fn backend_name(&self) -> &'static str {
        self.backend
    }

    fn update_metadata(&mut self, metadata: Option<MediaMetadata>) {
        if self.metadata != metadata {
            self.metadata = metadata;
            self.bump_revision();
        }
    }

    fn update_playback_state(&mut self, status: MediaPlaybackStatus, position_ms: u64) {
        if self.status != status || self.position_ms != position_ms {
            self.status = status;
            self.position_ms = position_ms;
            self.bump_revision();
        }
    }
}

/// Live OS media-control bridge wrapping [`LocalMediaBridge`] and an optional native
/// `souvlaki` backend running on a dedicated worker thread.
///
/// Running the native `souvlaki::MediaControls` controller on a background thread
/// guarantees [`SystemMediaBridge`] is [`Send`] on all targets without `unsafe` code.
/// When native OS media controls are unavailable (such as headless Linux without a
/// D-Bus session bus, Windows without an `HWND`, or unsupported platforms), the bridge
/// transparently degrades to in-memory [`LocalMediaBridge`] tracking.
pub struct SystemMediaBridge {
    local: LocalMediaBridge,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    native: Option<NativeMediaWorker>,
}

impl fmt::Debug for SystemMediaBridge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemMediaBridge")
            .field("local", &self.local)
            .field("native_active", &self.is_native_active())
            .finish_non_exhaustive()
    }
}

impl SystemMediaBridge {
    /// Connects to native OS media controls when available and returns the bridge along
    /// with a receiver for incoming OS transport [`MediaControlEvent`]s.
    ///
    /// Never fails or panics when the host desktop media session is unavailable; in that
    /// case [`Self::is_native_active`] returns `false` and state tracking continues in memory.
    #[must_use]
    pub fn connect() -> (Self, mpsc::Receiver<MediaControlEvent>) {
        let (event_tx, event_rx) = mpsc::channel();

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let native = NativeMediaWorker::start(event_tx);

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = event_tx;

        let bridge = Self {
            local: LocalMediaBridge::new(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            native,
        };
        (bridge, event_rx)
    }

    /// Creates an in-memory [`SystemMediaBridge`] without attempting native OS registration.
    ///
    /// Intended for tests and configurations where OS media controls are explicitly disabled.
    #[must_use]
    pub fn in_memory() -> (Self, mpsc::Receiver<MediaControlEvent>) {
        let (_event_tx, event_rx) = mpsc::channel();
        let bridge = Self {
            local: LocalMediaBridge::new(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            native: None,
        };
        (bridge, event_rx)
    }

    /// Returns `true` if a live native OS media-control handle is currently attached.
    #[must_use]
    pub const fn is_native_active(&self) -> bool {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.native.is_some()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            false
        }
    }

    /// Returns a shared reference to the underlying deduplicated [`LocalMediaBridge`].
    #[must_use]
    pub const fn local(&self) -> &LocalMediaBridge {
        &self.local
    }

    /// Updates track navigation capabilities in the local tracker.
    pub fn set_navigation_capabilities(&mut self, can_go_next: bool, can_go_previous: bool) {
        self.local
            .set_navigation_capabilities(can_go_next, can_go_previous);
    }

    /// Updates the OS-visible volume percentage (`0..=100`), clamping values above `100`.
    pub fn set_volume_percent(&mut self, volume_percent: u8) {
        let before = self.local.revision();
        self.local.set_volume_percent(volume_percent);
        if self.local.revision() != before {
            #[cfg(target_os = "linux")]
            self.send_native(NativeCommand::Volume(self.local.volume_percent()));
        }
    }

    /// Synchronizes metadata, playback status, position, and volume from [`PlaybackState`].
    pub fn sync_from_playback_state(&mut self, playback: &PlaybackState) {
        let metadata = playback
            .current_track
            .as_ref()
            .map(MediaMetadata::from_track);
        self.update_metadata(metadata);

        let status = if playback.current_track.is_none() {
            MediaPlaybackStatus::Stopped
        } else if playback.is_playing {
            MediaPlaybackStatus::Playing
        } else {
            MediaPlaybackStatus::Paused
        };
        self.update_playback_state(status, playback.position_ms);

        let effective_volume = if playback.muted {
            0
        } else {
            playback.volume_percent
        };
        self.set_volume_percent(effective_volume);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn send_native(&mut self, command: NativeCommand) {
        if let Some(worker) = self.native.as_ref()
            && !worker.send(command)
        {
            self.native = None;
        }
    }
}

impl MediaControlBridge for SystemMediaBridge {
    fn backend_name(&self) -> &'static str {
        self.local.backend_name()
    }

    fn update_metadata(&mut self, metadata: Option<MediaMetadata>) {
        let before = self.local.revision();
        self.local.update_metadata(metadata);
        if self.local.revision() != before {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            self.send_native(NativeCommand::Metadata(self.local.metadata().cloned()));
        }
    }

    fn update_playback_state(&mut self, status: MediaPlaybackStatus, position_ms: u64) {
        let before = self.local.revision();
        self.local.update_playback_state(status, position_ms);
        if self.local.revision() != before {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            self.send_native(NativeCommand::Playback {
                status,
                position_ms,
            });
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
enum NativeCommand {
    Metadata(Option<MediaMetadata>),
    Playback {
        status: MediaPlaybackStatus,
        position_ms: u64,
    },
    #[cfg(target_os = "linux")]
    Volume(u8),
    Shutdown,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct NativeMediaWorker {
    tx: mpsc::Sender<NativeCommand>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl NativeMediaWorker {
    fn start(event_tx: mpsc::Sender<MediaControlEvent>) -> Option<Self> {
        #[cfg(target_os = "linux")]
        if !has_linux_session_bus() {
            return None;
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<NativeCommand>();
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();

        let handle = std::thread::Builder::new()
            .name(String::from("vynl-media-controls"))
            .spawn(move || {
                run_native_worker(event_tx, &cmd_rx, &ready_tx);
            })
            .ok()?;

        if let Ok(true) = ready_rx.recv() {
            Some(Self {
                tx: cmd_tx,
                handle: Some(handle),
            })
        } else {
            let _ = handle.join();
            None
        }
    }

    fn send(&self, command: NativeCommand) -> bool {
        self.tx.send(command).is_ok()
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for NativeMediaWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(NativeCommand::Shutdown);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_native_worker(
    event_tx: mpsc::Sender<MediaControlEvent>,
    cmd_rx: &mpsc::Receiver<NativeCommand>,
    ready_tx: &mpsc::Sender<bool>,
) {
    let config = souvlaki::PlatformConfig {
        dbus_name: "vynl",
        display_name: "vynl",
        hwnd: None,
    };

    let Ok(mut controls) = souvlaki::MediaControls::new(config) else {
        let _ = ready_tx.send(false);
        return;
    };

    let attach_result = controls.attach(move |event| {
        if let Some(translated) = translate_souvlaki_event(&event) {
            let _ = event_tx.send(translated);
        }
    });

    if attach_result.is_err() {
        let _ = ready_tx.send(false);
        return;
    }

    let _ = ready_tx.send(true);

    while let Ok(command) = cmd_rx.recv() {
        match command {
            NativeCommand::Metadata(metadata) => {
                apply_souvlaki_metadata(&mut controls, metadata.as_ref());
            }
            NativeCommand::Playback {
                status,
                position_ms,
            } => {
                let playback = to_souvlaki_playback(status, position_ms);
                let _ = controls.set_playback(playback);
            }
            #[cfg(target_os = "linux")]
            NativeCommand::Volume(volume_percent) => {
                let _ = controls.set_volume(f64::from(volume_percent) / 100.0);
            }
            NativeCommand::Shutdown => break,
        }
    }

    let _ = controls.detach();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn apply_souvlaki_metadata(
    controls: &mut souvlaki::MediaControls,
    metadata: Option<&MediaMetadata>,
) {
    use std::time::Duration;

    if let Some(meta) = metadata {
        let joined_artists = if meta.artists.is_empty() {
            None
        } else {
            Some(meta.artists.join(", "))
        };
        let native_meta = souvlaki::MediaMetadata {
            title: Some(meta.title.as_str()),
            album: Some(meta.album.as_str()),
            artist: joined_artists.as_deref(),
            cover_url: meta.cover_url.as_deref(),
            duration: Some(Duration::from_millis(meta.duration_ms)),
        };
        let _ = controls.set_metadata(native_meta);
    } else {
        let _ = controls.set_metadata(souvlaki::MediaMetadata::default());
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const fn to_souvlaki_playback(
    status: MediaPlaybackStatus,
    position_ms: u64,
) -> souvlaki::MediaPlayback {
    use std::time::Duration;

    let progress = Some(souvlaki::MediaPosition(Duration::from_millis(position_ms)));
    match status {
        MediaPlaybackStatus::Stopped => souvlaki::MediaPlayback::Stopped,
        MediaPlaybackStatus::Playing => souvlaki::MediaPlayback::Playing { progress },
        MediaPlaybackStatus::Paused => souvlaki::MediaPlayback::Paused { progress },
    }
}

#[cfg(target_os = "linux")]
fn has_linux_session_bus() -> bool {
    use std::os::unix::net::UnixStream;
    use std::path::Path;

    if let Ok(address) = std::env::var("DBUS_SESSION_BUS_ADDRESS")
        && !address.is_empty()
    {
        if let Some(rest) = address.strip_prefix("unix:path=") {
            let socket_path = rest.split([',', ';']).next().unwrap_or("");
            if !socket_path.is_empty() && !socket_path.contains('%') {
                return UnixStream::connect(socket_path).is_ok();
            }
        }
        return true;
    }

    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR")
        && !runtime_dir.is_empty()
    {
        return UnixStream::connect(Path::new(&runtime_dir).join("bus")).is_ok();
    }

    false
}

/// Translates a native `souvlaki::MediaControlEvent` into a `vynl` [`MediaControlEvent`].
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn translate_souvlaki_event(event: &souvlaki::MediaControlEvent) -> Option<MediaControlEvent> {
    match event {
        souvlaki::MediaControlEvent::Play => Some(MediaControlEvent::Play),
        souvlaki::MediaControlEvent::Pause => Some(MediaControlEvent::Pause),
        souvlaki::MediaControlEvent::Toggle => Some(MediaControlEvent::PlayPause),
        souvlaki::MediaControlEvent::Next => Some(MediaControlEvent::Next),
        souvlaki::MediaControlEvent::Previous => Some(MediaControlEvent::Previous),
        souvlaki::MediaControlEvent::Stop => Some(MediaControlEvent::Stop),
        souvlaki::MediaControlEvent::SeekBy(direction, duration) => {
            let ms = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
            let offset = match direction {
                souvlaki::SeekDirection::Forward => ms,
                souvlaki::SeekDirection::Backward => ms.saturating_neg(),
            };
            Some(MediaControlEvent::SeekByMs(offset))
        }
        souvlaki::MediaControlEvent::SetVolume(volume) => {
            volume_f64_to_percent(*volume).map(MediaControlEvent::SetVolume)
        }
        _ => None,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[cfg_attr(target_os = "windows", allow(dead_code))]
fn volume_f64_to_percent(volume: f64) -> Option<u8> {
    if !volume.is_finite() {
        return None;
    }
    // `scaled` is finite and clamped to [0.0, 100.0] before casting to `u8`.
    let scaled = (volume.clamp(0.0, 1.0) * 100.0).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = scaled as u8;
    Some(percent.min(100))
}

#[cfg(test)]
mod tests {
    use vynl_core::model::{ImageRef, PlaybackState, TrackSummary};

    use super::{
        LocalMediaBridge, MediaControlBridge, MediaMetadata, MediaPlaybackStatus,
        SystemMediaBridge, media_control_backend_name,
    };

    fn sample_track() -> TrackSummary {
        TrackSummary {
            id: String::from("4cOdK2wGLETKBW3PvgPWqT"),
            title: String::from("Giorgio by Moroder"),
            artists: vec![String::from("Daft Punk")],
            album: String::from("Random Access Memories"),
            duration_ms: 544_000,
            explicit: false,
            is_liked: true,
            cover: Some(ImageRef {
                url: String::from("https://i.scdn.co/image/ram"),
                width: Some(640),
                height: Some(640),
            }),
        }
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn tracks_metadata_and_playback_revisions() {
        let mut bridge = LocalMediaBridge::new();
        assert_eq!(bridge.backend_name(), media_control_backend_name());
        assert_eq!(bridge.status(), MediaPlaybackStatus::Stopped);
        assert_eq!(bridge.status().as_mpris_str(), "Stopped");
        assert_eq!(bridge.revision(), 0);

        let metadata = MediaMetadata::from_track(&sample_track());
        bridge.update_metadata(Some(metadata.clone()));
        assert_eq!(bridge.revision(), 1);
        assert_eq!(bridge.metadata(), Some(&metadata));

        // Identical updates do not increment revision.
        bridge.update_metadata(Some(metadata));
        assert_eq!(bridge.revision(), 1);

        bridge.update_playback_state(MediaPlaybackStatus::Playing, 42_000);
        assert_eq!(bridge.status(), MediaPlaybackStatus::Playing);
        assert_eq!(bridge.status().as_mpris_str(), "Playing");
        assert_eq!(bridge.position_ms(), 42_000);
        assert_eq!(bridge.revision(), 2);
    }

    #[test]
    fn syncs_from_core_playback_state() {
        let mut bridge = LocalMediaBridge::new();
        let state = PlaybackState {
            current_track: Some(sample_track()),
            is_playing: false,
            position_ms: 15_500,
            volume_percent: 72,
            muted: false,
            ..PlaybackState::default()
        };

        bridge.sync_from_playback_state(&state);
        assert_eq!(bridge.status(), MediaPlaybackStatus::Paused);
        assert_eq!(bridge.position_ms(), 15_500);
        assert_eq!(bridge.volume_percent(), 72);
        assert_eq!(
            bridge.metadata().map(|meta| meta.title.as_str()),
            Some("Giorgio by Moroder")
        );

        let muted_state = PlaybackState {
            muted: true,
            ..state
        };
        bridge.sync_from_playback_state(&muted_state);
        assert_eq!(bridge.volume_percent(), 0);
    }

    #[test]
    fn system_media_bridge_is_send_and_supports_in_memory_sync() {
        assert_send::<SystemMediaBridge>();

        let (mut bridge, _rx) = SystemMediaBridge::in_memory();
        assert!(!bridge.is_native_active());
        assert_eq!(bridge.backend_name(), media_control_backend_name());
        assert_eq!(bridge.local().revision(), 0);

        let state = PlaybackState {
            current_track: Some(sample_track()),
            is_playing: true,
            position_ms: 90_000,
            volume_percent: 65,
            muted: false,
            ..PlaybackState::default()
        };
        bridge.sync_from_playback_state(&state);

        assert_eq!(bridge.local().status(), MediaPlaybackStatus::Playing);
        assert_eq!(bridge.local().position_ms(), 90_000);
        assert_eq!(bridge.local().volume_percent(), 65);
        assert_eq!(
            bridge.local().metadata().map(|meta| meta.title.as_str()),
            Some("Giorgio by Moroder")
        );
        let rev_after_first = bridge.local().revision();
        assert!(rev_after_first >= 3);

        // Identical sync is deduplicated.
        bridge.sync_from_playback_state(&state);
        assert_eq!(bridge.local().revision(), rev_after_first);

        // Clearing track transitions to Stopped.
        let stopped = PlaybackState::default();
        bridge.sync_from_playback_state(&stopped);
        assert_eq!(bridge.local().status(), MediaPlaybackStatus::Stopped);
        assert!(bridge.local().metadata().is_none());
    }

    #[test]
    fn system_media_bridge_connect_never_panics_in_headless_env() {
        let (mut bridge, _rx) = SystemMediaBridge::connect();
        assert_eq!(bridge.backend_name(), media_control_backend_name());

        let metadata = MediaMetadata::from_track(&sample_track());
        bridge.update_metadata(Some(metadata));
        bridge.update_playback_state(MediaPlaybackStatus::Playing, 12_345);
        bridge.set_volume_percent(80);
        bridge.set_navigation_capabilities(true, false);

        assert_eq!(bridge.local().status(), MediaPlaybackStatus::Playing);
        assert_eq!(bridge.local().position_ms(), 12_345);
        assert_eq!(bridge.local().volume_percent(), 80);
        assert!(bridge.local().can_go_next());
        assert!(!bridge.local().can_go_previous());
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    #[test]
    fn translates_souvlaki_events_accurately() {
        use std::time::Duration;

        use super::{MediaControlEvent, translate_souvlaki_event};

        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Play),
            Some(MediaControlEvent::Play)
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Pause),
            Some(MediaControlEvent::Pause)
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Toggle),
            Some(MediaControlEvent::PlayPause)
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Next),
            Some(MediaControlEvent::Next)
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Previous),
            Some(MediaControlEvent::Previous)
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Stop),
            Some(MediaControlEvent::Stop)
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::SeekBy(
                souvlaki::SeekDirection::Forward,
                Duration::from_millis(5_000),
            )),
            Some(MediaControlEvent::SeekByMs(5_000))
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::SeekBy(
                souvlaki::SeekDirection::Backward,
                Duration::from_millis(3_500),
            )),
            Some(MediaControlEvent::SeekByMs(-3_500))
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::SetVolume(0.75)),
            Some(MediaControlEvent::SetVolume(75))
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::SetVolume(1.5)),
            Some(MediaControlEvent::SetVolume(100))
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::SetVolume(-0.2)),
            Some(MediaControlEvent::SetVolume(0))
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::SetVolume(f64::NAN)),
            None
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Raise),
            None
        );
        assert_eq!(
            translate_souvlaki_event(&souvlaki::MediaControlEvent::Quit),
            None
        );
    }
}
