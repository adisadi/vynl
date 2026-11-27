# Getting Started

## Prerequisites

- **Rust 1.90+** (Edition 2024) if building from source
- On Linux with the default `rodio-backend`: `pkg-config` and ALSA development headers (`libasound2-dev` on Debian/Ubuntu)
- A **Spotify Developer Client ID** (free to create; avoids shared Development Mode user limits)

## Building from Source

```bash
# Default build (integrated librespot + rodio audio backend)
cargo build --release -p vynl-tui

# Optional Zen-mode real-time FFT audio visualizer
cargo build --release -p vynl-tui --features visualizer

# 100% pure-Rust PCM subprocess backend (ideal for static musl / FreeBSD)
cargo build --release -p vynl-tui --no-default-features --features subprocess-backend

# Remote-only Spotify Connect controller (zero audio dependencies)
cargo build --release -p vynl-tui --no-default-features
```

## Try Demo Mode Immediately (No Account Needed)

```bash
vynl --demo
```

Demo mode launches the full interactive TUI with a built-in catalog, procedural cover art across Kitty/iTerm2/Sixel/Halfblocks, Zen mode, Command Palette (`Ctrl+P`), and adaptive WCAG-AA themes.

## Spotify OAuth 2.0 PKCE Setup

1. Open [developer.spotify.com/dashboard](https://developer.spotify.com/dashboard) and create an app.
2. Under **Redirect URIs**, add:
   ```text
   http://127.0.0.1:8888/callback
   ```
3. Copy your **Client ID** and authenticate via PKCE:
   ```bash
   vynl auth --client-id <YOUR_CLIENT_ID>
   ```
4. Launch `vynl`:
   ```bash
   vynl
   ```

OAuth tokens are stored in your OS keyring (Linux Secret Service via pure-Rust `zbus`, macOS Keychain, Windows Credential Manager) with a `0600` file fallback at `~/.local/state/vynl/tokens.json` in headless environments. To clear saved tokens, run `vynl auth logout`.

## Environment Diagnostics

Run `vynl doctor` to verify detected terminal graphics protocol, compiled audio backends, config/cache/log paths, and OAuth state:

```bash
vynl doctor
```
