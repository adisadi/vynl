//! Application configuration models and zero-dependency TOML parser/serializer
//! mirroring `~/.config/vynl/config.toml`.

/// Playback mode selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackMode {
    /// Use the built-in `librespot` Spotify Connect player.
    #[default]
    Integrated,
    /// Control another Spotify Connect device via the Spotify Web API.
    Remote,
}

impl PlaybackMode {
    /// Serializes to the TOML string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Integrated => "integrated",
            Self::Remote => "remote",
        }
    }
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

impl AudioBackendKind {
    /// Serializes to the TOML string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rodio => "rodio",
            Self::Subprocess => "subprocess",
        }
    }
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

impl ImageProtocolPreference {
    /// Serializes to the TOML string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Kitty => "kitty",
            Self::ITerm2 => "iterm2",
            Self::Sixel => "sixel",
            Self::Halfblocks => "halfblocks",
        }
    }
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

impl ListThumbnailPolicy {
    /// Serializes to the TOML string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
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

impl AppConfig {
    /// Parses a TOML configuration document, falling back to safe defaults for omitted keys.
    ///
    /// # Errors
    /// Returns a human-readable line-numbered error when a known key has an invalid value.
    pub fn from_toml(input: &str) -> Result<Self, String> {
        let mut cfg = Self::default();
        let mut section = String::new();

        for (idx, raw_line) in input.lines().enumerate() {
            let line_no = idx + 1;
            let line = strip_toml_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                line[1..line.len() - 1].trim().clone_into(&mut section);
                continue;
            }
            let Some((raw_key, raw_val)) = line.split_once('=') else {
                return Err(format!("line {line_no}: expected `key = value`"));
            };
            let key = raw_key.trim();
            let val = raw_val.trim();
            cfg.apply_key(&section, key, val, line_no)?;
        }

        Ok(cfg)
    }

    fn apply_key(
        &mut self,
        section: &str,
        key: &str,
        val: &str,
        line_no: usize,
    ) -> Result<(), String> {
        match (section, key) {
            ("spotify", "client_id") => {
                let s = parse_toml_string(val, line_no)?;
                self.spotify.client_id = if s.is_empty() { None } else { Some(s) };
            }
            ("spotify", "playback_mode") => {
                self.spotify.playback_mode = match parse_toml_string(val, line_no)?.as_str() {
                    "integrated" => PlaybackMode::Integrated,
                    "remote" => PlaybackMode::Remote,
                    other => {
                        return Err(format!("line {line_no}: invalid playback_mode `{other}`"));
                    }
                };
            }
            ("spotify", "audio_backend") => {
                self.spotify.audio_backend = match parse_toml_string(val, line_no)?.as_str() {
                    "rodio" => AudioBackendKind::Rodio,
                    "subprocess" => AudioBackendKind::Subprocess,
                    other => {
                        return Err(format!("line {line_no}: invalid audio_backend `{other}`"));
                    }
                };
            }
            ("spotify", "device_name") => {
                self.spotify.device_name = parse_toml_string(val, line_no)?;
            }
            ("spotify", "bitrate" | "bitrate_kbps") => {
                let kbps: u16 = val
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid integer `{val}`"))?;
                if !matches!(kbps, 96 | 160 | 320) {
                    return Err(format!(
                        "line {line_no}: bitrate must be 96, 160, or 320 (got {kbps})"
                    ));
                }
                self.spotify.bitrate_kbps = kbps;
            }
            ("spotify", "stream_cache_mb") => {
                self.spotify.stream_cache_mb = val
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid u32 `{val}`"))?;
            }
            ("images", "enabled") => self.images.enabled = parse_toml_bool(val, line_no)?,
            ("images", "protocol") => {
                self.images.protocol = match parse_toml_string(val, line_no)?.as_str() {
                    "auto" => ImageProtocolPreference::Auto,
                    "kitty" => ImageProtocolPreference::Kitty,
                    "iterm2" => ImageProtocolPreference::ITerm2,
                    "sixel" => ImageProtocolPreference::Sixel,
                    "halfblocks" => ImageProtocolPreference::Halfblocks,
                    other => return Err(format!("line {line_no}: invalid protocol `{other}`")),
                };
            }
            ("images", "list_thumbnails") => {
                self.images.list_thumbnails = match parse_toml_string(val, line_no)?.as_str() {
                    "auto" => ListThumbnailPolicy::Auto,
                    "always" => ListThumbnailPolicy::Always,
                    "never" => ListThumbnailPolicy::Never,
                    other => {
                        return Err(format!("line {line_no}: invalid list_thumbnails `{other}`"));
                    }
                };
            }
            ("images", "max_memory_cache_items") => {
                self.images.max_memory_cache_items = val
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid usize `{val}`"))?;
            }
            ("images", "max_disk_cache_mb") => {
                self.images.max_disk_cache_mb = val
                    .parse()
                    .map_err(|_| format!("line {line_no}: invalid u32 `{val}`"))?;
            }
            ("ui", "theme") => self.ui.theme = parse_toml_string(val, line_no)?,
            ("ui", "adaptive_cover_colors") => {
                self.ui.adaptive_cover_colors = parse_toml_bool(val, line_no)?;
            }
            ("ui", "mouse_enabled") => self.ui.mouse_enabled = parse_toml_bool(val, line_no)?,
            ("integrations", "media_controls") => {
                self.integrations.media_controls = parse_toml_bool(val, line_no)?;
            }
            ("integrations", "notifications") => {
                self.integrations.notifications = parse_toml_bool(val, line_no)?;
            }
            ("integrations", "ipc_socket") => {
                self.integrations.ipc_socket = parse_toml_bool(val, line_no)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Serializes this configuration into canonical `config.toml` format.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let client_id = self.spotify.client_id.as_deref().unwrap_or("");
        format!(
            "[spotify]\n\
             client_id = \"{client_id}\"\n\
             playback_mode = \"{}\"\n\
             audio_backend = \"{}\"\n\
             device_name = \"{}\"\n\
             bitrate = {}\n\
             stream_cache_mb = {}\n\n\
             [images]\n\
             enabled = {}\n\
             protocol = \"{}\"\n\
             list_thumbnails = \"{}\"\n\
             max_memory_cache_items = {}\n\
             max_disk_cache_mb = {}\n\n\
             [ui]\n\
             theme = \"{}\"\n\
             adaptive_cover_colors = {}\n\
             mouse_enabled = {}\n\n\
             [integrations]\n\
             media_controls = {}\n\
             notifications = {}\n\
             ipc_socket = {}\n",
            self.spotify.playback_mode.as_str(),
            self.spotify.audio_backend.as_str(),
            self.spotify.device_name,
            self.spotify.bitrate_kbps,
            self.spotify.stream_cache_mb,
            self.images.enabled,
            self.images.protocol.as_str(),
            self.images.list_thumbnails.as_str(),
            self.images.max_memory_cache_items,
            self.images.max_disk_cache_mb,
            self.ui.theme,
            self.ui.adaptive_cover_colors,
            self.ui.mouse_enabled,
            self.integrations.media_controls,
            self.integrations.notifications,
            self.integrations.ipc_socket,
        )
    }
}

pub(crate) fn strip_toml_comment(line: &str) -> &str {
    let mut in_quotes = false;
    for (idx, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => return &line[..idx],
            _ => {}
        }
    }
    line
}

pub(crate) fn parse_toml_string(val: &str, line_no: usize) -> Result<String, String> {
    if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
        Ok(val[1..val.len() - 1].replace("\\\"", "\""))
    } else {
        Err(format!(
            "line {line_no}: expected quoted string, got `{val}`"
        ))
    }
}

fn parse_toml_bool(val: &str, line_no: usize) -> Result<bool, String> {
    match val {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("line {line_no}: expected boolean, got `{val}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, AudioBackendKind, ImageProtocolPreference, ListThumbnailPolicy};

    #[test]
    fn round_trips_config_toml() -> Result<(), String> {
        let mut cfg = AppConfig::default();
        cfg.spotify.client_id = Some(String::from("abc123clientid"));
        cfg.spotify.audio_backend = AudioBackendKind::Subprocess;
        cfg.images.protocol = ImageProtocolPreference::Kitty;
        cfg.images.list_thumbnails = ListThumbnailPolicy::Always;
        cfg.ui.theme = String::from("catppuccin-mocha");

        let serialized = cfg.to_toml();
        let parsed = AppConfig::from_toml(&serialized)?;
        assert_eq!(parsed, cfg);
        Ok(())
    }

    #[test]
    fn rejects_invalid_bitrate() {
        let bad = "[spotify]\nbitrate = 128\n";
        assert!(AppConfig::from_toml(bad).is_err());
    }
}
