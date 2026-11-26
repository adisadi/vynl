//! Zero-dependency 24-bit `TrueColor` terminal cell buffer and diff-based ANSI renderer.
//!
//! Computes minimal cell diffs between consecutive frames to eliminate flicker
//! and exports plain-text snapshots for deterministic headless UI testing.

use std::fmt::Write as _;

use vynl_core::theme::Rgb;

/// Terminal rectangle in 0-indexed character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left column (0-indexed).
    pub x: u16,
    /// Top row (0-indexed).
    pub y: u16,
    /// Width in cells.
    pub width: u16,
    /// Height in cells.
    pub height: u16,
}

impl Rect {
    /// Constructs a new rectangle.
    #[must_use]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if `(col, row)` falls inside this rectangle.
    #[must_use]
    pub const fn contains(self, col: u16, row: u16) -> bool {
        col >= self.x
            && col < self.x.saturating_add(self.width)
            && row >= self.y
            && row < self.y.saturating_add(self.height)
    }

    /// Returns an inner rectangle inset by 1 cell on every side (inside a box border).
    #[must_use]
    pub const fn inner(self) -> Self {
        Self {
            x: self.x.saturating_add(1),
            y: self.y.saturating_add(1),
            width: self.width.saturating_sub(2),
            height: self.height.saturating_sub(2),
        }
    }

    /// Right-most exclusive X coordinate.
    #[must_use]
    pub const fn right(self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Bottom-most exclusive Y coordinate.
    #[must_use]
    pub const fn bottom(self) -> u16 {
        self.y.saturating_add(self.height)
    }
}

/// Visual styling attributes for a terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    /// Foreground color.
    pub fg: Option<Rgb>,
    /// Background color.
    pub bg: Option<Rgb>,
    /// Bold text weight.
    pub bold: bool,
    /// Dimmed text weight.
    pub dim: bool,
}

impl CellStyle {
    /// Creates a style with foreground and background colors.
    #[must_use]
    pub const fn fg_bg(fg: Rgb, bg: Rgb) -> Self {
        Self {
            fg: Some(fg),
            bg: Some(bg),
            bold: false,
            dim: false,
        }
    }

    /// Returns a bold variant of this style.
    #[must_use]
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// Returns a dimmed variant of this style.
    #[must_use]
    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }
}

/// Single character cell inside a [`Buffer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// Displayed Unicode scalar value.
    pub ch: char,
    /// Style attributes.
    pub style: CellStyle,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: CellStyle::default(),
        }
    }
}

/// 2D character cell grid backing a rendered TUI frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    /// Frame width in cells.
    pub width: u16,
    /// Frame height in cells.
    pub height: u16,
    cells: Vec<Cell>,
    /// Pre-encoded terminal graphic escape sequences positioned at `(x, y)` to
    /// emit after the text diff pass (used for Kitty / `iTerm2` / Sixel).
    graphics_overlays: Vec<(u16, u16, String)>,
}

impl Buffer {
    /// Allocates a blank frame buffer filled with `bg`.
    #[must_use]
    pub fn filled(width: u16, height: u16, fg: Rgb, bg: Rgb) -> Self {
        let len = usize::from(width) * usize::from(height);
        let cell = Cell {
            ch: ' ',
            style: CellStyle::fg_bg(fg, bg),
        };
        Self {
            width,
            height,
            cells: vec![cell; len],
            graphics_overlays: Vec::new(),
        }
    }

    /// Returns the cell at `(x, y)` if in bounds.
    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = usize::from(y) * usize::from(self.width) + usize::from(x);
        self.cells.get(idx)
    }

    /// Sets a single cell if `(x, y)` is in bounds.
    pub fn set(&mut self, x: u16, y: u16, ch: char, style: CellStyle) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = usize::from(y) * usize::from(self.width) + usize::from(x);
        if let Some(cell) = self.cells.get_mut(idx) {
            cell.ch = ch;
            cell.style = style;
        }
    }

    /// Fills `rect` with spaces using `style`.
    pub fn fill_rect(&mut self, rect: Rect, style: CellStyle) {
        for row in rect.y..rect.bottom().min(self.height) {
            for col in rect.x..rect.right().min(self.width) {
                self.set(col, row, ' ', style);
            }
        }
    }

    /// Draws a rounded Unicode box border with an optional title.
    pub fn draw_box(&mut self, rect: Rect, title: &str, border_style: CellStyle, fill: CellStyle) {
        if rect.width < 2 || rect.height < 2 {
            return;
        }
        self.fill_rect(rect, fill);
        let right = rect.right().saturating_sub(1);
        let bottom = rect.bottom().saturating_sub(1);

        self.set(rect.x, rect.y, '╭', border_style);
        self.set(right, rect.y, '╮', border_style);
        self.set(rect.x, bottom, '╰', border_style);
        self.set(right, bottom, '╯', border_style);

        for x in (rect.x + 1)..right {
            self.set(x, rect.y, '─', border_style);
            self.set(x, bottom, '─', border_style);
        }
        for y in (rect.y + 1)..bottom {
            self.set(rect.x, y, '│', border_style);
            self.set(right, y, '│', border_style);
        }

        if !title.is_empty() && rect.width > 4 {
            let max_w = rect.width.saturating_sub(4);
            let label = format!(" {title} ");
            self.put_str_clipped(rect.x + 2, rect.y, &label, max_w, border_style.bold());
        }
    }

    /// Writes `text` starting at `(x, y)`, truncating with `…` if it exceeds `max_width`.
    pub fn put_str_clipped(
        &mut self,
        x: u16,
        y: u16,
        text: &str,
        max_width: u16,
        style: CellStyle,
    ) {
        if max_width == 0 || y >= self.height {
            return;
        }
        let chars: Vec<char> = text.chars().collect();
        let max_len = usize::from(max_width);
        let truncated = chars.len() > max_len;
        let emit_len = if truncated {
            max_len.saturating_sub(1)
        } else {
            chars.len()
        };

        for (i, &ch) in chars.iter().take(emit_len).enumerate() {
            let Ok(offset) = u16::try_from(i) else {
                break;
            };
            self.set(x.saturating_add(offset), y, ch, style);
        }
        if truncated && let Ok(offset) = u16::try_from(emit_len) {
            self.set(x.saturating_add(offset), y, '…', style);
        }
    }

    /// Registers a raw terminal graphics escape sequence (Kitty / `iTerm2` / Sixel)
    /// anchored at cell `(x, y)` to be emitted after the cell diff pass.
    pub fn push_graphics_overlay(&mut self, x: u16, y: u16, payload: String) {
        self.graphics_overlays.push((x, y, payload));
    }

    /// Returns all queued graphics overlays for this frame.
    #[must_use]
    pub fn graphics_overlays(&self) -> &[(u16, u16, String)] {
        &self.graphics_overlays
    }

    /// Renders a diff-based ANSI byte stream transitioning from `previous` to `self`.
    ///
    /// When `previous` is `None` or has different dimensions, emits a full redraw.
    #[must_use]
    pub fn diff_ansi(&self, previous: Option<&Self>) -> String {
        let full_redraw = previous.is_none_or(|p| p.width != self.width || p.height != self.height);
        let mut out = String::with_capacity(usize::from(self.width) * usize::from(self.height) * 4);
        if full_redraw {
            out.push_str("\x1b[2J\x1b[H");
        }

        let mut last_style: Option<CellStyle> = None;
        let mut cursor_pos: Option<(u16, u16)> = None;

        for y in 0..self.height {
            for x in 0..self.width {
                let Some(&cell) = self.get(x, y) else {
                    continue;
                };
                if !full_redraw && previous.and_then(|p| p.get(x, y)).copied() == Some(cell) {
                    continue;
                }

                if cursor_pos != Some((x, y)) {
                    let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
                }
                if last_style != Some(cell.style) {
                    write_sgr(&mut out, cell.style);
                    last_style = Some(cell.style);
                }
                out.push(cell.ch);
                cursor_pos = Some((x.saturating_add(1), y));
            }
        }

        out.push_str("\x1b[0m");

        for (x, y, payload) in &self.graphics_overlays {
            let _ = write!(out, "\x1b[{};{}H{}", y + 1, x + 1, payload);
        }

        out
    }

    /// Exports the frame as plain text lines for deterministic `TestBackend` snapshot testing.
    #[must_use]
    pub fn to_plain_text(&self) -> String {
        let mut out = String::with_capacity(usize::from(self.width + 1) * usize::from(self.height));
        for y in 0..self.height {
            for x in 0..self.width {
                let ch = self.get(x, y).map_or(' ', |c| c.ch);
                out.push(ch);
            }
            if y + 1 < self.height {
                out.push('\n');
            }
        }
        out
    }
}

fn write_sgr(out: &mut String, style: CellStyle) {
    out.push_str("\x1b[0");
    if style.bold {
        out.push_str(";1");
    }
    if style.dim {
        out.push_str(";2");
    }
    if let Some(fg) = style.fg {
        let _ = write!(out, ";38;2;{};{};{}", fg.r, fg.g, fg.b);
    }
    if let Some(bg) = style.bg {
        let _ = write!(out, ";48;2;{};{};{}", bg.r, bg.g, bg.b);
    }
    out.push('m');
}

#[cfg(test)]
mod tests {
    use vynl_core::theme::Rgb;

    use super::{Buffer, CellStyle, Rect};

    #[test]
    fn draws_box_and_produces_minimal_ansi_diff() {
        let fg = Rgb::new(240, 240, 240);
        let bg = Rgb::new(18, 18, 18);
        let mut a = Buffer::filled(20, 5, fg, bg);
        a.draw_box(
            Rect::new(0, 0, 20, 5),
            "vynl",
            CellStyle::fg_bg(fg, bg),
            CellStyle::fg_bg(fg, bg),
        );
        let plain = a.to_plain_text();
        assert!(plain.contains("╭─"));
        assert!(plain.contains("vynl"));

        let mut b = a.clone();
        b.set(5, 2, 'X', CellStyle::fg_bg(fg, bg));
        let diff = b.diff_ansi(Some(&a));
        assert!(diff.contains("\x1b[3;6H"));
        assert!(diff.contains('X'));
        assert!(!diff.contains("\x1b[2J"));
    }
}
