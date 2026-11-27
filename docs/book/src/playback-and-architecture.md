# Playback Engine & Architecture

## Dual Playback Architecture

`vynl` combines an integrated Spotify Connect player (`librespot`) with full remote control of any Spotify Connect device on your account:

- **`rodio-backend` (default)**: Cross-platform `cpal`/`rodio` audio output on Linux, macOS, and Windows with integrated `librespot` playback.
- **`subprocess-backend`**: 100% pure-Rust PCM pipe (`pw-play`, `paplay`, `aplay`, or custom command) with zero C audio libraries — built for static `musl` binaries and FreeBSD.
- **`--no-default-features` (remote-only)**: Controls remote Spotify Connect devices via Web API with zero audio or `librespot` dependencies.
- **`visualizer` (optional feature)**: Non-blocking `try_lock` PCM tap feeding a pure-Rust radix-2 FFT 16-band spectrum analyzer into Zen mode.

## Pure-Rust Cross-Compile Stack

Every crate enforces `#[forbid(unsafe_code)]` and strict `clippy::pedantic` lints (`unwrap_used`, `expect_used`, `panic`, `dbg_macro` denied):

- **TLS**: `reqwest` + `librespot` configured with `rustls` (`openssl-sys` is banned in `deny.toml`).
- **Storage & Search**: `redb` embedded ACID KV store (`~/.cache/vynl/metadata.redb`) + `nucleo` fuzzy search (`libsqlite3-sys` is banned in `deny.toml`).
- **Linux OS Integration**: Pure-Rust `zbus` for MPRIS2 (`souvlaki`), Secret Service (`keyring`), and desktop notifications (`notify-rust`) — zero `libdbus-sys`.
- **Diagnostic Logs**: Append-only structured logs at `~/.local/state/vynl/vynl.log` (automatically rotated at 2 MiB).

## Strictly Spotify-Only Policy

`vynl` never contacts external third-party cloud APIs (no LRCLIB/external lyrics, no Last.fm/ListenBrainz, no Discord RPC). Restricted Spotify Web API endpoints returning HTTP `403` degrade silently without interrupting playback or showing error popups.
