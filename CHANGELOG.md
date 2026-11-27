# Changelog

All notable changes to `vynl` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Full 6-crate Rust workspace (`vynl-core`, `vynl-spotify`, `vynl-player`, `vynl-images`, `vynl-system`, `vynl-tui`) enforcing `#[forbid(unsafe_code)]`, strict `clippy::pedantic`, zero `openssl-sys`, and zero `libsqlite3-sys`.
- Live Spotify Web API client (`reqwest` + `rustls-tls`), OAuth 2.0 PKCE CLI login (`vynl auth` / `vynl auth logout`), OS keyring storage (`keyring` with `0600` file fallback), `redb` ACID metadata cache, and `nucleo` local fuzzy search.
- Integrated `librespot` Spotify Connect playback (`rodio-backend` default, pure-Rust `subprocess-backend` for static `musl`/FreeBSD, `--no-default-features` remote-only) with optional real-time radix-2 FFT visualizer (`--features visualizer`).
- Asynchronous cover art pipeline (`image` JPEG/PNG/WebP decoding + `HttpImageSource`), Kitty/iTerm2/Sixel/Halfblocks encoders, L1/L2 caching, PNG cover export for desktop notifications, and **Modal-Aware Image Occlusion**.
- Native `crossterm` cross-platform raw mode & input loop with panic-safe terminal restoration, disk `config.toml` & custom `~/.config/vynl/themes/*.toml` discovery, WCAG-AA adaptive cover accents, interactive `LibraryFilter` sidebar navigation, full `vynl open spotify:<type>:<id>` local IPC routing, and structured file logging (`~/.local/state/vynl/vynl.log`).
- Local OS integrations: pure-Rust `zbus` MPRIS2 / macOS MediaRemote / Windows SMTC (`souvlaki`) and pure-Rust `zbus` Linux desktop notifications (`notify-rust`).
- Deterministic `insta` UI layout snapshot test suite (9 snapshots across all 4 responsive breakpoints, Zen mode, and floating modals), full mdBook user documentation, and auditable SLSA release workflow.
