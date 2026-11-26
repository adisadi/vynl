# Keybindings, Mouse & CLI/IPC

`vynl` runs a hybrid UX: Arrow keys, Vim motions, full SGR mouse controls, and the Command Palette (`Ctrl+P`) are active simultaneously.

## Keyboard Shortcuts

| Keys | Action |
| :--- | :--- |
| `↑` / `↓` or `k` / `j` | Move selection up / down |
| `←` / `→` or `h` / `l` | Focus Library sidebar / Main panel |
| `g` / `G` or `Home` / `End` | Jump to top / bottom |
| `Tab` / `Shift+Tab` | Cycle panel focus |
| `Enter` | Open selected playlist/album/artist or play track |
| `f` (in Sidebar) | Cycle Library filter (`All` → `Playlists` → `Albums` → `Artists` → `Podcasts`) |
| `f` (in Main) | Open inline fuzzy list filter |
| `Space` | Play / Pause |
| `n` / `p` (or `>` / `<`) | Next / Previous track |
| `Shift+Left` / `Shift+Right` | Seek `-5s` / `+5s` |
| `+` / `-` / `m` | Volume `+5%` / `-5%` / Mute |
| `s` / `r` | Toggle Shuffle / Cycle Repeat (`Off` → `Context` → `Track`) |
| `L` / `a` / `P` / `x` | Like track / Enqueue / Add to Playlist / Multi-Select |
| `/` or `Ctrl+F` | Instant Search (local `nucleo` + live Spotify search) |
| `Ctrl+P` or `:` | Command Palette (actions + built-in & custom themes) |
| `d` / `q` / `z` | Device Picker / Queue Panel / Zen Fullscreen Now Playing |
| `Backspace` / `Alt+Left` | Navigate back |
| `?` | Toggle Help modal |
| `Esc` | Dismiss active modal or filter |
| `Q` or `Ctrl+C` | Quit cleanly (restores terminal state) |

## Mouse Controls

- **Click or drag** on the progress bar to scrub track position.
- **Click or drag** on the volume bar to set exact volume.
- **Click** any sidebar entry to open it immediately; **scroll wheel** navigates lists.
- **Right-click** opens the Command Palette.

## Local CLI & IPC Scripting

`vynl` runs as a single process and exposes a local Unix domain socket (`$XDG_RUNTIME_DIR/vynl.sock`) for status bars (Waybar, Polybar, tmux, SketchyBar) and shell scripts:

```bash
vynl status --json
vynl play-pause
vynl next
vynl prev
vynl volume +5
vynl volume -10
vynl open spotify:track:<id>
vynl open spotify:playlist:<id>
vynl open spotify:album:<id>
vynl open spotify:artist:<id>
```
