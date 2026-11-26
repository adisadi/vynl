//! OS media-control bridge abstractions (MPRIS2 / SMTC / `MediaRemote`) for `vynl`.
//!
//! Provides a platform-neutral [`MediaControlBridge`] trait and an in-memory
//! [`LocalMediaBridge`] tracker ready to be wired to native D-Bus, Windows SMTC,
//! or macOS `MediaRemote` handles.

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

#[cfg(test)]
mod tests {
    use vynl_core::model::{ImageRef, PlaybackState, TrackSummary};

    use super::{
        LocalMediaBridge, MediaControlBridge, MediaMetadata, MediaPlaybackStatus,
        media_control_backend_name,
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
}
