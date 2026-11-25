## Summary

<!-- Briefly describe what this PR changes and why. Link related issues/discussions: Closes #123 -->

## Type of Change

- [ ] Bug fix (`fix:`)
- [ ] New feature (`feat:`)
- [ ] Performance improvement (`perf:`)
- [ ] Refactoring (`refactor:`)
- [ ] Documentation / Theme (`docs:`)
- [ ] CI / Build / Tooling (`chore:` / `ci:`)

## Visual Preview (if UI or Image Rendering changed)

<!-- Attach a screenshot, terminal recording, or VHS GIF if applicable. -->

## Checklist

- [ ] PR title follows [Conventional Commits](https://www.conventionalcommits.org/).
- [ ] `just check` passes locally (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --workspace`).
- [ ] Pure-Rust / Cross-Compile invariants preserved (no `openssl-sys`, no `libsqlite3-sys`, platform integrations properly `cfg`-gated).
- [ ] Strictly Spotify-Only invariant preserved (no external third-party APIs).
- [ ] UI snapshots updated and verified (if layout changed).
- [ ] **AI Disclosure**: If AI coding tools assisted with this PR, I have personally reviewed, understood, and tested all submitted code (`[ ] Yes, AI-assisted` / `[ ] No AI used`).
