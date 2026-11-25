# RFC-0000: Title

- **Author(s)**: @github-handle
- **Status**: Draft | In Discussion | Accepted | Rejected
- **Discussion Link**: (GitHub Discussions URL)

## Summary

One-paragraph explanation of the proposed change.

## Motivation

Why should `vynl` add or change this? What user workflow or architectural issue does it solve?

## Detailed Design

Explain crate boundaries, state/event changes, config schema additions, and cross-compile/platform implications.

## Invariants Check

- [ ] Preserves Cross-Compile-First / Pure-Rust constraints (no `openssl-sys`, no `libsqlite3-sys`)
- [ ] Preserves Strictly Spotify-Only policy (no external third-party APIs)
- [ ] Preserves Modal-Aware Image Occlusion & non-blocking UI rendering

## Alternatives Considered

What other approaches were evaluated and why were they rejected?
