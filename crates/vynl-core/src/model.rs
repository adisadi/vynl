//! Core Spotify domain models shared across all `vynl` crates.

/// Reference to a remote cover or avatar artwork served by Spotify's CDN.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageRef {
    /// Canonical HTTPS image URL (or `vynl://demo/<id>` for offline demo covers).
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

    /// Returns the Spotify Web API query parameter string.
    #[must_use]
    pub const fn as_api_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Context => "context",
            Self::Track => "track",
        }
    }
}

/// Authenticated Spotify user profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserProfile {
    /// Spotify user ID.
    pub id: String,
    /// Display name shown in the top bar.
    pub display_name: String,
    /// Subscription tier (`premium` or `free`).
    pub product: String,
    /// Optional avatar artwork.
    pub avatar: Option<ImageRef>,
}

/// Minimal track summary displayed in lists, queue, and Now Playing bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSummary {
    /// Spotify track ID.
    pub id: String,
    /// Track title.
    pub title: String,
    /// Artist display names.
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

impl TrackSummary {
    /// Formats `duration_ms` as `MM:SS`.
    #[must_use]
    pub fn formatted_duration(&self) -> String {
        format_duration_ms(self.duration_ms)
    }

    /// Formats the joined artist list.
    #[must_use]
    pub fn artist_line(&self) -> String {
        if self.artists.is_empty() {
            String::from("Unknown Artist")
        } else {
            self.artists.join(", ")
        }
    }
}

/// Formats milliseconds into `MM:SS` (or `H:MM:SS` for podcasts/audiobooks >= 1 hour).
#[must_use]
pub fn format_duration_ms(ms: u64) -> String {
    let total_secs = ms / 1_000;
    let hours = total_secs / 3_600;
    let mins = (total_secs % 3_600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

/// Album summary for Home cards, artist discography, and library views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumSummary {
    /// Spotify album ID.
    pub id: String,
    /// Album title.
    pub title: String,
    /// Primary artist names.
    pub artists: Vec<String>,
    /// Release year (e.g., `2013`).
    pub release_year: String,
    /// Album type (`album`, `single`, `compilation`).
    pub album_type: String,
    /// Total track count.
    pub total_tracks: u32,
    /// Optional album cover artwork.
    pub cover: Option<ImageRef>,
}

/// Artist summary for search, library following, and artist header views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistSummary {
    /// Spotify artist ID.
    pub id: String,
    /// Artist display name.
    pub name: String,
    /// Follower count.
    pub followers: u64,
    /// Genres associated with the artist.
    pub genres: Vec<String>,
    /// Whether the current user follows this artist.
    pub following: bool,
    /// Optional artist portrait artwork.
    pub avatar: Option<ImageRef>,
}

/// Playlist summary for sidebar, library, and playlist detail header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSummary {
    /// Spotify playlist ID.
    pub id: String,
    /// Playlist title.
    pub name: String,
    /// Playlist description.
    pub description: String,
    /// Owner display name.
    pub owner: String,
    /// Number of tracks.
    pub track_count: u32,
    /// Spotify `snapshot_id` for cache invalidation.
    pub snapshot_id: String,
    /// Whether pinned to the top of the library sidebar.
    pub pinned: bool,
    /// Optional virtual folder name stored locally.
    pub folder: Option<String>,
    /// Optional playlist cover artwork.
    pub cover: Option<ImageRef>,
}

/// Podcast or audiobook episode summary with resume position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeSummary {
    /// Spotify episode ID.
    pub id: String,
    /// Episode title.
    pub title: String,
    /// Show or audiobook name.
    pub show_name: String,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Resume position in milliseconds.
    pub resume_position_ms: u64,
    /// Whether fully played.
    pub fully_played: bool,
    /// Optional show/episode cover artwork.
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
    /// Playback speed in hundredths (`100` = 1.0x, `150` = 1.5x for podcasts).
    pub speed_percent: u16,
    /// Active Spotify Connect device.
    pub active_device: Option<ConnectDevice>,
}

/// Playback queue and recently played history.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueState {
    /// Explicitly queued tracks ("Next in queue").
    pub up_next: Vec<TrackSummary>,
    /// Recently played tracks.
    pub recently_played: Vec<TrackSummary>,
}

/// Category filter tabs on the Search view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchCategory {
    /// Combined top hits across all entities.
    #[default]
    All,
    /// Tracks only.
    Tracks,
    /// Albums only.
    Albums,
    /// Artists only.
    Artists,
    /// Playlists only.
    Playlists,
    /// Podcasts & episodes.
    Podcasts,
}

impl SearchCategory {
    /// Returns all search tabs in display order.
    #[must_use]
    pub const fn all_tabs() -> &'static [Self] {
        &[
            Self::All,
            Self::Tracks,
            Self::Albums,
            Self::Artists,
            Self::Playlists,
            Self::Podcasts,
        ]
    }

    /// Human-readable English tab label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Tracks => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
            Self::Podcasts => "Podcasts",
        }
    }
}

/// Grouped search results returned by Spotify Web API or local fuzzy index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchResults {
    /// Query that produced these results.
    pub query: String,
    /// Matching tracks.
    pub tracks: Vec<TrackSummary>,
    /// Matching albums.
    pub albums: Vec<AlbumSummary>,
    /// Matching artists.
    pub artists: Vec<ArtistSummary>,
    /// Matching playlists.
    pub playlists: Vec<PlaylistSummary>,
    /// Matching podcast episodes.
    pub episodes: Vec<EpisodeSummary>,
}

/// Sidebar library filter pills.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LibraryFilter {
    /// Show all library items.
    #[default]
    All,
    /// Filter to playlists.
    Playlists,
    /// Filter to saved albums.
    Albums,
    /// Filter to followed artists.
    Artists,
    /// Filter to podcasts & audiobooks.
    Podcasts,
}

impl LibraryFilter {
    /// Cycles through sidebar filter pills.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::All => Self::Playlists,
            Self::Playlists => Self::Albums,
            Self::Albums => Self::Artists,
            Self::Artists => Self::Podcasts,
            Self::Podcasts => Self::All,
        }
    }

    /// English pill label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Playlists => "Playlists",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Podcasts => "Podcasts",
        }
    }
}

/// Cached user library collections shown in Home, Sidebar, and Detail views.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LibraryState {
    /// User's saved Liked Songs.
    pub liked_tracks: Vec<TrackSummary>,
    /// User's owned and followed playlists.
    pub playlists: Vec<PlaylistSummary>,
    /// Saved albums.
    pub albums: Vec<AlbumSummary>,
    /// Followed artists.
    pub artists: Vec<ArtistSummary>,
    /// Saved podcast episodes.
    pub episodes: Vec<EpisodeSummary>,
}

/// Severity of a non-intrusive status toast in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    /// Informational feedback (e.g., "Added to queue").
    Info,
    /// Recoverable warning (e.g., HTTP 429 rate-limit backoff).
    Warning,
    /// Action failure.
    Error,
}

/// Transient toast notification rendered in the status chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastMessage {
    /// Severity level.
    pub level: ToastLevel,
    /// Short English message.
    pub text: String,
    /// Remaining ticks until auto-dismissal.
    pub ttl_ticks: u16,
}

#[cfg(test)]
mod tests {
    use super::{LibraryFilter, RepeatMode, format_duration_ms};

    #[test]
    fn repeat_mode_cycles_in_desktop_order() {
        assert_eq!(RepeatMode::Off.cycle(), RepeatMode::Context);
        assert_eq!(RepeatMode::Context.cycle(), RepeatMode::Track);
        assert_eq!(RepeatMode::Track.cycle(), RepeatMode::Off);
        assert_eq!(RepeatMode::Context.as_api_str(), "context");
    }

    #[test]
    fn formats_track_and_podcast_durations() {
        assert_eq!(format_duration_ms(337_000), "05:37");
        assert_eq!(format_duration_ms(3_665_000), "1:01:05");
        assert_eq!(LibraryFilter::All.next(), LibraryFilter::Playlists);
    }
}
