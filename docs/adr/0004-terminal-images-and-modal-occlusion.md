# ADR-0004: Terminal Image Pipeline, Modal-Aware Occlusion & Modular Themes

- **Status**: Accepted
- **Date**: 2026-11-25

## Context

Rendering high-resolution album covers in terminal emulators introduces two classic failure modes: scroll jank on slower protocols (Sixel/Halfblocks) and Z-order visual corruption when floating popups overlap pixel graphics.

## Decision

1. **Smart Protocol Scaling**: Hero images (Home grid, headers, Now Playing, Zen mode, player bar) always render; list thumbnails default to enabled on Kitty/iTerm2 and disabled (opt-in) on Sixel/Halfblocks.
2. **Modal-Aware Image Occlusion**: When a floating modal (Command Palette, Device Picker, Help) is open, overlapping image rects are cleanly suppressed and restored instantaneously from the L1 RAM LRU cache on dismissal.
3. **Modular TOML Themes + Adaptive Cover Accents**: Built-in presets (`vynl-dark`, `catppuccin-mocha`, `tokyo-night`, `gruvbox`, `nord`) plus `~/.config/vynl/themes/*.toml`. Adaptive cover colors override only accent slots after verifying WCAG-AA contrast against the theme background.

## Consequences

- Smooth 60 FPS scrolling across all terminals and zero modal overlap artifacts in Kitty, Sixel, iTerm2, and `tmux`.
