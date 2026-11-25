# Contributing to `vynl`

Thank you for your interest in contributing to `vynl`! We welcome bug reports, feature discussions, themes, documentation improvements, and code contributions.

## 5-Minute Development Setup

Choose whichever workflow suits you best:

1. **GitHub Codespaces / VS Code Devcontainer**: Open the repository in a Devcontainer — Rust, `just`, `cargo-nextest`, `cargo-deny`, `cargo-insta`, and Linux audio headers are pre-installed.
2. **Nix Flake**: Run `nix develop` (or `direnv allow`).
3. **Local Rust Toolchain**: Install Rust (see `rust-toolchain.toml`) and optionally `just`, `cargo-nextest`, `cargo-deny`, and `cargo-insta`.

Run the full check suite locally before opening a PR:

```bash
just check
```

## Collaboration Workflow

- **Discussions vs. Issues**: Use **GitHub Discussions** for ideas, Q&A, custom themes (*Show & Tell*), and RFCs. Use **GitHub Issues** for confirmed bugs and actionable tasks.
- **RFCs (`docs/rfcs/`)**: Significant UX or architectural changes should start with a short RFC discussion before writing large amounts of code.
- **ADRs (`docs/adr/`)**: Permanent architectural decisions are documented as Architecture Decision Records.
- **Trunk-Based Development**: Branch from `main`, keep PRs focused and small, and ensure all CI checks pass for the GitHub Merge Queue.
- **Conventional Commits**: PR titles and squash commits must follow [Conventional Commits](https://www.conventionalcommits.org/) (e.g., `feat(images): add modal occlusion clipping`, `fix(spotify): respect Retry-After header`).

## Coding Standards & Invariants

- **Language**: All UI strings, CLI output, code comments, and docs are **English-only**.
- **Strict Lints**: `#[forbid(unsafe_code)]`, `clippy::pedantic`, `clippy::unwrap_used = "deny"`, `clippy::expect_used = "deny"`, and `clippy::panic = "deny"` are enforced workspace-wide.
- **Cross-Compile First**: Never introduce OpenSSL or C SQLite dependencies (`deny.toml` enforces this). Prefer pure-Rust crates and gate OS-specific integrations cleanly behind `cfg(target_os = ...)` and Cargo features.
- **Strictly Spotify-Only**: Do not add third-party cloud APIs (no external lyrics providers, Last.fm, or Discord RPC).
- **Modal-Aware Image Occlusion**: Floating modals must suppress overlapping terminal image regions via `vynl-images`.

## AI-Assisted Contributions Policy

AI-assisted contributions are welcome provided that:
1. You have personally reviewed, understood, and tested every line you submit.
2. You disclose AI assistance in the PR checklist.
3. The contribution adheres strictly to `AGENTS.md` and passes `just check`.

## Licensing

By submitting a contribution, you agree that your work is licensed under **`GPL-3.0-or-later`** (GitHub Inbound = Outbound).
