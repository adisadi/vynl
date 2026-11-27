# Configuration & Modular Themes

`vynl` stores its configuration at `~/.config/vynl/config.toml` (honoring `$XDG_CONFIG_HOME` on Unix and `%APPDATA%\vynl` on Windows).

## Complete `config.toml` Reference

```toml
[spotify]
client_id = "your-spotify-developer-client-id"
playback_mode = "integrated"   # "integrated" | "remote"
audio_backend = "rodio"        # "rodio" | "subprocess"
device_name = "vynl"
bitrate_kbps = 320             # 96 | 160 | 320
stream_cache_mb = 256          # 0 disables on-disk stream cache

[images]
enabled = true
protocol = "auto"              # "auto" | "kitty" | "iterm2" | "sixel" | "halfblocks"
list_thumbnails = "auto"       # "auto" | "always" | "never"
max_memory_cache_items = 64
max_disk_cache_mb = 250

[ui]
theme = "vynl-dark"            # built-in preset or custom theme filename stem
adaptive_cover_colors = true   # WCAG-AA contrast-checked accent extraction
mouse_enabled = true

[integrations]
media_controls = true          # MPRIS2 (zbus) / macOS MediaRemote / Windows SMTC
notifications = true           # Desktop track-change notifications with PNG cover art
ipc_socket = true              # Local Unix socket / Windows named pipe for CLI control
```

## Built-In & Custom TOML Themes

Built-in themes:
- `vynl-dark`
- `catppuccin-mocha`
- `tokyo-night`
- `gruvbox`
- `nord`

### Creating a Custom Theme

Drop any `.toml` file into `~/.config/vynl/themes/<name>.toml` (for example, `~/.config/vynl/themes/cyberpunk.toml`):

```toml
background = "#0d0f18"
surface = "#161926"
text_primary = "#edf2ff"
text_muted = "#8b95b3"
accent = "#ff4fd8"
border = "#2c324a"
```

Custom themes are automatically discovered on startup and appear in the **Command Palette (`Ctrl+P`)**. Switching themes persists your selection to `config.toml`.

### Adaptive Cover Colors (WCAG-AA)

When `adaptive_cover_colors = true`, `vynl` extracts dominant colors from the active album cover on a background worker and overrides **only** the `accent` slot after verifying a contrast ratio of at least `4.5:1` (WCAG-AA) against both `background` and `surface`.
