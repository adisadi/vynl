# AGENTS.md — Guidelines for AI Coding Agents

This file defines binding architectural invariants, build/test commands, and conventions for AI coding agents working on `vynl`.

## Project Overview

`vynl` is a cross-platform Rust TUI Spotify client with native terminal cover art, licensed under **GPL-3.0-or-later** (Rust Edition 2024, Rolling MSRV).

## Non-Negotiable Architectural Invariants

1. **Cross-Compile-First (Pure Rust Preferred)**:
   - **Never** add `openssl-sys` or `libsqlite3-sys`. Use `reqwest` with `rustls-tls` and `redb` + `nucleo`.
   - Keep `vynl-player` audio backends behind Cargo features: `rodio-backend` (default), `subprocess-backend` (pure Rust fallback), and `remote-only` (`--no-default-features`).
   - Isolate OS integrations (`MPRIS2`, `SMTC`, `MediaRemote`, notifications, IPC) inside `vynl-system` using target-specific dependencies.
2. **Strictly Spotify-Only**:
   - Do **not** add external third-party cloud services (no LRCLIB/lyrics APIs, no Last.fm/ListenBrainz, no Discord RPC).
   - Handle restricted Spotify Web API endpoints gracefully on HTTP `403` without crashing or nagging the user.
3. **Single-Process + Local IPC**:
   - `vynl` runs as a single process; CLI subcommands communicate with the running instance over local IPC. Do not introduce a separate background daemon architecture.
4. **Terminal Graphics Hygiene**:
   - Never block the main UI thread on image downloads, decoding, or resizing.
   - Always route floating modal overlays through **Modal-Aware Image Occlusion** (`vynl-images`) so Kitty/iTerm2/Sixel regions do not bleed over popups.
   - Preserve the panic hook that restores terminal state (raw mode, alternate screen, mouse capture).
5. **English-Only & Modular Themes**:
   - All UI text, CLI help, comments, and docs must be in English.
   - Adaptive cover colors must only override theme accent slots and must satisfy **WCAG-AA** contrast ratios against the theme background.
6. **Strict Lints**:
   - `unsafe_code` is forbidden workspace-wide.
   - `unwrap()`, `expect()`, `panic!()`, and `dbg!()` are denied in non-test production code. Handle errors explicitly.

## Workspace Crates

- `crates/vynl-core` — Pure state, Elm reducer (`reduce`), models, config, theme/WCAG math (must stay free of I/O).
- `crates/vynl-spotify` — Lean Spotify Web API client, OAuth PKCE flow, rate limiting, `redb` cache & `nucleo` search.
- `crates/vynl-player` — `librespot` playback engine, adaptive sync policy, bounded stream cache.
- `crates/vynl-images` — Terminal protocol detection, async image cache/worker, modal occlusion math.
- `crates/vynl-system` — Local OS media controls, notifications, IPC protocol.
- `crates/vynl-tui` — Binary entry point (`vynl`), Ratatui rendering, input handling, CLI.

## Required Verification Commands

Before considering any task complete, run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

When adding or modifying UI layouts, update and verify `insta` snapshots. When adding Cargo features, verify `--no-default-features` and `--all-features`.
