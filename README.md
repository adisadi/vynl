# vynl

**A cross-platform Rust TUI Spotify client with native terminal cover art.**

[![CI](https://github.com/adisadi/vynl/actions/workflows/ci.yml/badge.svg)](https://github.com/adisadi/vynl/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/adisadi/vynl/badge)](https://scorecard.dev/viewer/?uri=github.com/adisadi/vynl)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPL--3.0--or--later-blue.svg)](LICENSE)

`vynl` brings the full Spotify Desktop experience into your terminal — complete with crisp album art, artist portraits, adaptive cover-color themes, integrated audio playback, and native OS media controls — built with a **cross-compile-first, pure-Rust** architecture.

---

## Highlights

- **Native Terminal Graphics**: Automatic protocol detection via `ratatui-image` (Kitty → iTerm2 → Sixel → 24-bit TrueColor Halfblocks), smart protocol scaling for list thumbnails, and **Modal-Aware Image Occlusion** (zero floating-popup artifacts).
- **Dual Playback Engine**: Built-in Spotify Connect player (`librespot` with `rodio` default backend + pure-Rust `subprocess` fallback) **and** remote control of any Spotify Connect device.
- **Cross-Compile Everywhere**: Pure-Rust TLS (`rustls`), embedded storage (`redb`), fuzzy search (`nucleo`), and D-Bus (`zbus`) — builds cleanly across Linux (`gnu`/`musl`), macOS (`x86_64`/`aarch64`), Windows (`x86_64`/`aarch64`), and FreeBSD.
- **Strictly Spotify-Only**: Zero telemetry, zero third-party cloud APIs (no external lyrics/scrobbling/RPC services).
- **Hybrid UX**: Arrow keys, full mouse support (click, scroll, drag scrubbing), Vim navigation (`h/j/k/l`, `g/G`), and a Command Palette (`Ctrl+P`) active side by side.
- **Modular Themes + Adaptive Cover Colors**: Built-in presets (`vynl-dark`, `catppuccin-mocha`, `tokyo-night`, `gruvbox`, `nord`) plus custom `~/.config/vynl/themes/*.toml` files with WCAG-AA contrast-checked cover accents.
- **Local OS Integration**: MPRIS2 / SMTC / MediaRemote media keys, desktop notifications with cover art, and CLI/IPC scripting (`vynl status --format json`, `vynl play-pause`).

## Terminal Image Compatibility

| Terminal | Protocol | List Thumbnails (Default) | Notes |
| :--- | :--- | :--- | :--- |
| **Kitty** | Kitty Graphics | Enabled | Full pixel clarity & caching |
| **Ghostty** | Kitty Graphics | Enabled | Native Kitty protocol |
| **WezTerm** | Kitty / iTerm2 / Sixel | Enabled | Auto-selects best protocol |
| **iTerm2** | iTerm2 Inline | Enabled | macOS native |
| **Foot / XTerm / Windows Terminal** | Sixel | Disabled (opt-in) | Fast 60 FPS scrolling |
| **Alacritty / SSH / Fallback** | TrueColor Halfblocks | Disabled (opt-in) | Works in any 24-bit terminal |
| **tmux / Zellij** | Passthrough / Halfblocks | Auto | Enable `allow-passthrough on` in `tmux` |

## Quickstart

```bash
# Try the full TUI immediately with the built-in demo catalog & cover art
cargo run -p vynl-tui -- --demo

# Authenticate with your own Spotify Developer Client ID (OAuth 2.0 PKCE)
cargo run -p vynl-tui -- auth --client-id <YOUR_CLIENT_ID>

# Launch live Spotify TUI (optional: --features visualizer for Zen FFT bars)
cargo run -p vynl-tui

# Check environment, detected terminal image protocol, paths & OAuth state
cargo run -p vynl-tui -- doctor

# Control a running instance from status bars or scripts via local IPC
vynl status --json
vynl play-pause
vynl open spotify:album:<ID>

# Run formatting, strict clippy lints, and tests (including insta UI snapshots)
just check
```

## Architecture & Workspace Layout

`vynl` is structured as a modular Cargo workspace:

- `crates/vynl-core` — Domain models, Elm-style state reducer, configuration, and WCAG-checked theme engine
- `crates/vynl-spotify` — Lean `reqwest`+`rustls` Spotify Web API client, PKCE auth state machine, `redb` cache & `nucleo` search
- `crates/vynl-player` — `librespot` playback engine (`rodio` default, `subprocess` / `remote-only` feature flags)
- `crates/vynl-images` — Terminal protocol detection, async L1/L2 image pipeline, and Modal-Aware Image Occlusion
- `crates/vynl-system` — Local OS integrations (MPRIS2 / SMTC / MediaRemote, notifications, CLI/IPC)
- `crates/vynl-tui` — The `vynl` binary crate (Ratatui views, hybrid input loop, CLI)

See [docs/adr/](docs/adr/) for recorded architectural decisions and [CONTRIBUTING.md](CONTRIBUTING.md) to get involved.

## License

Copyright (C) 2026 vynl contributors.

This program is free software: you can redistribute it and/or modify it under the terms of the **GNU General Public License v3.0 or later** (`GPL-3.0-or-later`). See [LICENSE](LICENSE) for full details.
