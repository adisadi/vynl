# ADR-0001: Cross-Compile-First Pure-Rust Stack

- **Status**: Accepted
- **Date**: 2026-11-25

## Context

`vynl` targets seamless cross-compilation across Linux (`gnu` and static `musl`), macOS (`x86_64`/`aarch64`), Windows (`x86_64`/`aarch64`), and FreeBSD. C system dependencies (OpenSSL, libsqlite3, libsecret) frequently break cross-compilation and static builds.

## Decision

1. Use `reqwest` with **`rustls-tls`** exclusively (`openssl-sys` is banned in `deny.toml`).
2. Use **`redb`** (pure-Rust embedded ACID key-value store) paired with in-memory **`nucleo`** fuzzy matching instead of C SQLite (`libsqlite3-sys` is banned in `deny.toml`).
3. Build a lean custom Spotify Web API client in `vynl-spotify` for full control over `rustls`, ETag/`snapshot_id` caching, `429 Retry-After` backoff, and resilient deserialization.
4. Use pure-Rust D-Bus (`zbus`) for Linux Secret Service keyring & MPRIS2, isolating platform bindings inside `vynl-system` via `cfg(target_os = ...)`.

## Consequences

- Tier-2 targets (`musl`, FreeBSD) compile cleanly with `cargo-zigbuild` without target C sysroots.
- Local library search over tens of thousands of tracks runs in memory via `nucleo` while startup stays under 200 ms.
