//! Terminal graphics protocol selection, smart list-thumbnail scaling, and
//! **Modal-Aware Image Occlusion** geometry (ADR-0004).

use vynl_core::config::ListThumbnailPolicy;

/// Resolved terminal image protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// Kitty Graphics Protocol (Kitty, Ghostty, `WezTerm`).
    Kitty,
    /// `iTerm2` Inline Images (`iTerm2`, `WezTerm`, Konsole).
    ITerm2,
    /// Sixel graphics (Foot, `XTerm`, Windows Terminal).
    Sixel,
    /// 24-bit `TrueColor` Unicode halfblocks (`▀`/`▄`).
    Halfblocks,
}

impl ImageProtocol {
    /// Human-readable label for `vynl doctor` and UI status.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::ITerm2 => "iterm2",
            Self::Sixel => "sixel",
            Self::Halfblocks => "halfblocks",
        }
    }

    /// Best-effort environment heuristic (Phase 1 augments this with `ratatui-image`
    /// terminal queries and `tmux` passthrough probing).
    pub fn detect_from_env(
        term_program: Option<&str>,
        term: Option<&str>,
        kitty_window_id: Option<&str>,
        in_tmux: bool,
    ) -> Self {
        if in_tmux {
            // Safe default until tmux `allow-passthrough` is verified via query.
            return Self::Halfblocks;
        }
        if kitty_window_id.is_some() {
            return Self::Kitty;
        }
        match term_program.unwrap_or_default() {
            "ghostty" | "WezTerm" | "kitty" => Self::Kitty,
            "iTerm.app" => Self::ITerm2,
            _ => match term.unwrap_or_default() {
                "xterm-kitty" | "xterm-ghostty" => Self::Kitty,
                "foot" | "foot-extra" | "mlterm" => Self::Sixel,
                _ => Self::Halfblocks,
            },
        }
    }

    /// Applies smart protocol scaling to decide whether small thumbnails inside
    /// scrolling track/search lists should be rendered.
    pub const fn render_list_thumbnails(self, policy: ListThumbnailPolicy) -> bool {
        match policy {
            ListThumbnailPolicy::Always => true,
            ListThumbnailPolicy::Never => false,
            ListThumbnailPolicy::Auto => matches!(self, Self::Kitty | Self::ITerm2),
        }
    }
}

/// Terminal cell rectangle used for Modal-Aware Image Occlusion checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    /// Left column (0-indexed).
    pub x: u16,
    /// Top row (0-indexed).
    pub y: u16,
    /// Width in character cells.
    pub width: u16,
    /// Height in character cells.
    pub height: u16,
}

impl CellRect {
    /// Constructs a new terminal cell rectangle.
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if `self` and `other` share at least one terminal cell.
    pub const fn intersects(self, other: Self) -> bool {
        if self.width == 0 || self.height == 0 || other.width == 0 || other.height == 0 {
            return false;
        }
        let self_right = self.x.saturating_add(self.width);
        let self_bottom = self.y.saturating_add(self.height);
        let other_right = other.x.saturating_add(other.width);
        let other_bottom = other.y.saturating_add(other.height);

        self.x < other_right
            && self_right > other.x
            && self.y < other_bottom
            && self_bottom > other.y
    }
}

/// Determines whether an image slot should be suppressed while a floating modal
/// occupies `modal_rect` (prevents Kitty/Sixel Z-order bleeding artifacts).
pub const fn is_occluded_by_modal(image_rect: CellRect, modal_rect: Option<CellRect>) -> bool {
    match modal_rect {
        Some(modal) => image_rect.intersects(modal),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use vynl_core::config::ListThumbnailPolicy;

    use super::{CellRect, ImageProtocol, is_occluded_by_modal};

    #[test]
    fn smart_protocol_scaling_enables_list_thumbnails_on_fast_protocols_only() {
        assert!(ImageProtocol::Kitty.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(ImageProtocol::ITerm2.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(!ImageProtocol::Sixel.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(!ImageProtocol::Halfblocks.render_list_thumbnails(ListThumbnailPolicy::Auto));
        assert!(ImageProtocol::Halfblocks.render_list_thumbnails(ListThumbnailPolicy::Always));
        assert!(!ImageProtocol::Kitty.render_list_thumbnails(ListThumbnailPolicy::Never));
    }

    #[test]
    fn detects_known_terminals_and_falls_back_in_tmux() {
        assert_eq!(
            ImageProtocol::detect_from_env(Some("ghostty"), None, None, false),
            ImageProtocol::Kitty
        );
        assert_eq!(
            ImageProtocol::detect_from_env(Some("iTerm.app"), None, None, false),
            ImageProtocol::ITerm2
        );
        assert_eq!(
            ImageProtocol::detect_from_env(None, Some("foot"), None, false),
            ImageProtocol::Sixel
        );
        assert_eq!(
            ImageProtocol::detect_from_env(Some("ghostty"), None, Some("1"), true),
            ImageProtocol::Halfblocks
        );
    }

    #[test]
    fn modal_occlusion_suppresses_only_overlapping_image_rects() {
        let cover = CellRect::new(10, 5, 20, 10);
        let overlapping_modal = CellRect::new(25, 10, 40, 15);
        let disjoint_modal = CellRect::new(40, 5, 30, 10);

        assert!(is_occluded_by_modal(cover, Some(overlapping_modal)));
        assert!(!is_occluded_by_modal(cover, Some(disjoint_modal)));
        assert!(!is_occluded_by_modal(cover, None));
    }
}
