//! Application configuration models mirroring `~/.config/vynl/config.toml`.

/// Playback mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackMode {
    /// Use the built-in `librespot` Spotify Connect player.
    #[default]
    Integrated,
    /// Control another Spotify Connect device via the Spotify Web API.
    Remote,
}

/// Audio backend selection for the integrated player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioBackendKind {
    /// Cross-platform `rodio`/`cpal` backend (default on Linux, macOS, Windows).
    #[default]
    Rodio,
    /// Pure-Rust PCM pipe subprocess fallback for static `musl`, FreeBSD, and exotic targets.
    Subprocess,
}

/// Terminal image protocol preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageProtocolPreference {
    /// Automatically pick the highest-fidelity protocol supported by the terminal.
    #[default]
    Auto,
    /// Force Kitty Graphics Protocol.
    Kitty,
    /// Force `iTerm2` Inline Images.
    ITerm2,
    /// Force Sixel graphics.
    Sixel,
    /// Force 24-bit `TrueColor` Unicode halfblocks.
    Halfblocks,
}

/// Policy controlling small thumbnails inside scrolling track and search lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListThumbnailPolicy {
    /// Enabled automatically on Kitty/`iTerm2`, disabled on Sixel/Halfblocks for 60 FPS scrolling.
    #[default]
    Auto,
    /// Always render list thumbnails regardless of protocol.
    Always,
    /// Never render list thumbnails (keep hero/header/Now Playing covers only).
    Never,
}

/// `[spotify]` configuration section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyConfig {
    /// User-supplied Spotify Developer `client_id` (BYO PKCE flow).
    pub client_id: Option<String>,
    /// Playback mode (`integrated` or `remote`).
    pub playback_mode: PlaybackMode,
    /// Audio backend for integrated playback.
    pub audio_backend: AudioBackendKind,
    /// Spotify Connect device name advertised by `vynl`.
    pub device_name: String,
    /// Audio streaming bitrate in kbps (`96`, `160`, or `320`).
    pub bitrate_kbps: u16,
    /// Bounded temporary `librespot` stream cache size in MiB (`0` disables disk stream cache).
    pub stream_cache_mb: u32,
}

impl Default for SpotifyConfig {
    fn default() -> Self {
        Self {
            client_id: None,
            playback_mode: PlaybackMode::Integrated,
            audio_backend: AudioBackendKind::Rodio,
            device_name: String::from("vynl"),
            bitrate_kbps: 320,
            stream_cache_mb: 256,
        }
    }
}

/// `[images]` configuration section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagesConfig {
    /// Master switch for terminal image rendering.
    pub enabled: bool,
    /// Terminal image protocol preference.
    pub protocol: ImageProtocolPreference,
    /// List thumbnail policy.
    pub list_thumbnails: ListThumbnailPolicy,
    /// L1 in-memory decoded image LRU capacity.
    pub max_memory_cache_items: usize,
    /// L2 on-disk image cache limit in MiB.
    pub max_disk_cache_mb: u32,
}

impl Default for ImagesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            protocol: ImageProtocolPreference::Auto,
            list_thumbnails: ListThumbnailPolicy::Auto,
            max_memory_cache_items: 64,
            max_disk_cache_mb: 250,
        }
    }
}

/// `[ui]` configuration section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiConfig {
    /// Active theme name (built-in preset or file under `~/.config/vynl/themes/`).
    pub theme: String,
    /// Override theme accent slots with WCAG-AA-checked cover colors.
    pub adaptive_cover_colors: bool,
    /// Enable mouse clicks, scrolling, and drag scrubbing.
    pub mouse_enabled: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: String::from("vynl-dark"),
            adaptive_cover_colors: true,
            mouse_enabled: true,
        }
    }
}

/// `[integrations]` configuration section (strictly local OS integrations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationsConfig {
    /// Enable local OS media controls (`MPRIS2` / `SMTC` / `MediaRemote`).
    pub media_controls: bool,
    /// Enable local OS track-change notifications.
    pub notifications: bool,
    /// Enable local IPC socket / named pipe for CLI scripting.
    pub ipc_socket: bool,
}

impl Default for IntegrationsConfig {
    fn default() -> Self {
        Self {
            media_controls: true,
            notifications: true,
            ipc_socket: true,
        }
    }
}

/// Top-level `vynl` configuration structure (`~/.config/vynl/config.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppConfig {
    /// Spotify playback and authentication settings.
    pub spotify: SpotifyConfig,
    /// Terminal image rendering and cache settings.
    pub images: ImagesConfig,
    /// UI appearance and input settings.
    pub ui: UiConfig,
    /// Local OS integration settings.
    pub integrations: IntegrationsConfig,
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, ListThumbnailPolicy, PlaybackMode};

    #[test]
    fn default_config_matches_spec_decisions() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.spotify.device_name, "vynl");
        assert_eq!(cfg.spotify.playback_mode, PlaybackMode::Integrated);
        assert_eq!(cfg.spotify.bitrate_kbps, 320);
        assert_eq!(cfg.spotify.stream_cache_mb, 256);
        assert_eq!(cfg.images.list_thumbnails, ListThumbnailPolicy::Auto);
        assert!(cfg.ui.adaptive_cover_colors);
        assert!(cfg.ui.mouse_enabled);
        assert!(cfg.integrations.media_controls);
        assert!(cfg.integrations.ipc_socket);
    }
}
