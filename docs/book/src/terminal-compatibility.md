# Terminal Image Compatibility

`vynl` automatically detects the highest-fidelity graphics protocol supported by your terminal emulator:

1. **Kitty Graphics Protocol** — Kitty, Ghostty, WezTerm
2. **iTerm2 Inline Images** — iTerm2, WezTerm, Konsole
3. **Sixel** — Foot, Contour, XTerm, Windows Terminal
4. **TrueColor Halfblocks (`▀` / `▄`)** — Alacritty, SSH, and any 24-bit terminal

## Using `vynl` inside `tmux`

To enable Kitty/Sixel passthrough inside `tmux`, add the following to `~/.tmux.conf`:

```tmux
set -g allow-passthrough on
```

Run `vynl doctor` inside your terminal to verify detected protocol capabilities.
