# ADR-0003: Strictly Spotify-Only Policy

- **Status**: Accepted
- **Date**: 2026-11-25

## Context

Spotify's public Web API does not expose lyrics and restricts several recommendation endpoints for newer developer apps. Third-party clients often pull in external services (LRCLIB, Last.fm, Discord RPC).

## Decision

`vynl` is **strictly Spotify-only**:
- No third-party cloud APIs (no LRCLIB/external lyrics, no Last.fm/ListenBrainz, no Discord Rich Presence).
- Restricted Spotify Web API endpoints are handled via graceful `403` detection and cleanly hidden without error dialogs.
- All desktop integrations (MPRIS2/SMTC/MediaRemote, notifications, IPC) operate strictly locally on the user's machine.

## Consequences

- Strong privacy guarantees, smaller dependency footprint, and zero external third-party API breakage.
