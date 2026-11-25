//! Core Spotify domain models shared across `vynl` crates.

/// Reference to a remote cover or avatar artwork served by Spotify's CDN.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef {
    /// Canonical HTTPS image URL.
    pub url: String,
    /// Optional width in pixels as reported by Spotify.
    pub width: Option<u32>,
    /// Optional height in pixels as reported by Spotify.
    pub height: Option<u32>,
}

/// Playback repeat mode mirroring the Spotify Desktop client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    /// Repeat disabled.
    #[default]
    Off,
    /// Repeat the current album, playlist, or artist context.
    Context,
    /// Repeat the currently playing track.
    Track,
}

impl RepeatMode {
    /// Cycles `Off -> Context -> Track -> Off`.
    #[must_use]
    pub const fn cycle(self) -> Self {
        match self {
            Self::Off => Self::Context,
            Self::Context => Self::Track,
            Self::Track => Self::Off,
        }
    }
}

/// Minimal track summary displayed in lists, queue, and Now Playing bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSummary {
    /// Spotify track ID.
    pub id: String,
    /// Track title.
    pub title: String,
    /// Comma-formatted or primary artist display names.
    pub artists: Vec<String>,
    /// Album title.
    pub album: String,
    /// Track duration in milliseconds.
    pub duration_ms: u64,
    /// Whether the track is marked explicit.
    pub explicit: bool,
    /// Whether the track is saved in Liked Songs.
    pub is_liked: bool,
    /// Optional cover artwork reference.
    pub cover: Option<ImageRef>,
}

/// Active Spotify Connect device descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectDevice {
    /// Spotify device identifier.
    pub id: String,
    /// Human-readable device name (e.g., `vynl`).
    pub name: String,
    /// Device category (e.g., `Computer`, `Smartphone`, `Speaker`).
    pub device_type: String,
    /// Whether this device is currently active.
    pub is_active: bool,
    /// Volume percentage (`0..=100`).
    pub volume_percent: u8,
}

/// Snapshot of current playback state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaybackState {
    /// Currently loaded track, if any.
    pub current_track: Option<TrackSummary>,
    /// Whether audio is actively playing.
    pub is_playing: bool,
    /// Current playback position in milliseconds.
    pub position_ms: u64,
    /// Shuffle toggle.
    pub shuffle: bool,
    /// Repeat mode.
    pub repeat: RepeatMode,
    /// Volume percentage (`0..=100`).
    pub volume_percent: u8,
    /// Whether output is muted.
    pub muted: bool,
    /// Active Spotify Connect device.
    pub active_device: Option<ConnectDevice>,
}

#[cfg(test)]
mod tests {
    use super::RepeatMode;

    #[test]
    fn repeat_mode_cycles_in_desktop_order() {
        assert_eq!(RepeatMode::Off.cycle(), RepeatMode::Context);
        assert_eq!(RepeatMode::Context.cycle(), RepeatMode::Track);
        assert_eq!(RepeatMode::Track.cycle(), RepeatMode::Off);
    }
}
