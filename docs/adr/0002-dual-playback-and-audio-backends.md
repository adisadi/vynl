# ADR-0002: Dual Playback Engine, Audio Backends & Single-Process Model

- **Status**: Accepted
- **Date**: 2026-11-25

## Context

Desktop parity requires built-in audio playback without running the official Spotify Desktop app, while preserving cross-compilation portability and simple IPC control.

## Decision

1. **Single-Process TUI + Local IPC**: `vynl` runs as a single process; CLI commands (`vynl status`, `vynl play-pause`) talk to the running instance over a Unix socket / Windows named pipe.
2. **Dual Playback**: Combine integrated `librespot` playback with Spotify Connect remote control.
3. **Feature-Gated Audio Backends**:
   - `rodio-backend` (default): single `cpal`-based backend for Linux, macOS, and Windows.
   - `subprocess-backend`: 100% pure-Rust PCM pipe backend for static `musl`, FreeBSD, and exotic targets.
   - `--no-default-features`: remote-only Connect controller with zero audio dependencies.
4. **Auth & Sync**: BYO Spotify Developer `client_id` setup wizard + OAuth 2.0 PKCE + `librespot` OAuth token bridge with Zeroconf fallback. Event-driven state push locally; adaptive polling (3–5 s active, 20–30 s paused) for remote devices; bounded 256 MB LRU temporary stream cache.

## Consequences

- Full desktop playback experience on Tier-1 platforms without sacrificing Tier-2 portability.
- Avoids Spotify's 25-user Development Mode cap by guiding users through their own `client_id`.
