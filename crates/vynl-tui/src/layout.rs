//! Responsive desktop-parity layout geometry across all 4 breakpoints.

use vynl_core::state::LayoutBreakpoint;

use crate::buffer::Rect;

/// Computed panel rectangles for the current terminal frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    /// Active breakpoint tier.
    pub breakpoint: LayoutBreakpoint,
    /// Top navigation & search chrome bar.
    pub top_bar: Option<Rect>,
    /// Left navigation & library sidebar.
    pub sidebar: Option<Rect>,
    /// Primary content view area.
    pub main: Rect,
    /// Right Now Playing / Queue sidebar.
    pub right_sidebar: Option<Rect>,
    /// Bottom persistent Now Playing transport & progress bar.
    pub player_bar: Rect,
    /// Progress bar clickable/draggable track area inside `player_bar`.
    pub progress_bar: Rect,
    /// Volume bar clickable/draggable area inside `player_bar`.
    pub volume_bar: Rect,
}

impl FrameLayout {
    /// Computes responsive panel bounds for `(cols, rows)` and right-sidebar visibility.
    #[must_use]
    pub fn compute(cols: u16, rows: u16, show_right_sidebar: bool) -> Self {
        let breakpoint = LayoutBreakpoint::from_terminal_size(cols, rows);

        if breakpoint == LayoutBreakpoint::MiniPlayer {
            let player_h = rows.min(4);
            let main_h = rows.saturating_sub(player_h);
            let player_bar = Rect::new(0, main_h, cols, player_h);
            let progress_bar = Rect::new(
                2,
                player_bar.y + player_h.saturating_sub(1),
                cols.saturating_sub(4),
                1,
            );
            return Self {
                breakpoint,
                top_bar: None,
                sidebar: None,
                main: Rect::new(0, 0, cols, main_h),
                right_sidebar: None,
                player_bar,
                progress_bar,
                volume_bar: Rect::default(),
            };
        }

        let top_h = 3u16;
        let player_h = 4u16;
        let body_y = top_h;
        let body_h = rows.saturating_sub(top_h + player_h);

        let top_bar = Some(Rect::new(0, 0, cols, top_h));
        let player_bar = Rect::new(0, body_y + body_h, cols, player_h);

        let (sidebar, main, right_sidebar) = match breakpoint {
            LayoutBreakpoint::Wide => {
                let left_w = 28u16.min(cols / 4);
                let right_w = if show_right_sidebar {
                    32u16.min(cols / 4)
                } else {
                    0
                };
                let main_w = cols.saturating_sub(left_w + right_w);
                (
                    Some(Rect::new(0, body_y, left_w, body_h)),
                    Rect::new(left_w, body_y, main_w, body_h),
                    if right_w > 0 {
                        Some(Rect::new(left_w + main_w, body_y, right_w, body_h))
                    } else {
                        None
                    },
                )
            }
            LayoutBreakpoint::Medium => {
                let left_w = 24u16.min(cols / 3);
                let main_w = cols.saturating_sub(left_w);
                (
                    Some(Rect::new(0, body_y, left_w, body_h)),
                    Rect::new(left_w, body_y, main_w, body_h),
                    None,
                )
            }
            LayoutBreakpoint::Compact | LayoutBreakpoint::MiniPlayer => {
                (None, Rect::new(0, body_y, cols, body_h), None)
            }
        };

        let pb_x = cols / 4;
        let pb_w = (cols / 2).max(10).min(cols.saturating_sub(pb_x + 2));
        let progress_bar = Rect::new(pb_x, player_bar.y + 2, pb_w, 1);

        let vol_w = 12u16.min(cols.saturating_sub(4));
        let vol_x = cols.saturating_sub(vol_w + 3);
        let volume_bar = Rect::new(vol_x, player_bar.y + 1, vol_w, 1);

        Self {
            breakpoint,
            top_bar,
            sidebar,
            main,
            right_sidebar,
            player_bar,
            progress_bar,
            volume_bar,
        }
    }

    /// Computes a centered floating modal rectangle suitable for Command Palette,
    /// Device Picker, Playlist Modal, or Help Overlay.
    #[must_use]
    pub fn centered_modal(cols: u16, rows: u16, width_pct: u16, height_pct: u16) -> Rect {
        let w = (u32::from(cols) * u32::from(width_pct.min(95)) / 100).clamp(32, u32::from(cols));
        let h = (u32::from(rows) * u32::from(height_pct.min(90)) / 100).clamp(10, u32::from(rows));
        let w = u16::try_from(w).unwrap_or(cols);
        let h = u16::try_from(h).unwrap_or(rows);
        let x = cols.saturating_sub(w) / 2;
        let y = rows.saturating_sub(h) / 2;
        Rect::new(x, y, w, h)
    }
}

#[cfg(test)]
mod tests {
    use vynl_core::state::LayoutBreakpoint;

    use super::FrameLayout;

    #[test]
    fn computes_all_four_responsive_breakpoints() {
        let wide = FrameLayout::compute(160, 40, true);
        assert_eq!(wide.breakpoint, LayoutBreakpoint::Wide);
        assert!(wide.sidebar.is_some());
        assert!(wide.right_sidebar.is_some());

        let medium = FrameLayout::compute(110, 35, true);
        assert_eq!(medium.breakpoint, LayoutBreakpoint::Medium);
        assert!(medium.sidebar.is_some());
        assert!(medium.right_sidebar.is_none());

        let compact = FrameLayout::compute(75, 24, true);
        assert_eq!(compact.breakpoint, LayoutBreakpoint::Compact);
        assert!(compact.sidebar.is_none());

        let mini = FrameLayout::compute(50, 12, true);
        assert_eq!(mini.breakpoint, LayoutBreakpoint::MiniPlayer);
        assert!(mini.top_bar.is_none());
    }
}
