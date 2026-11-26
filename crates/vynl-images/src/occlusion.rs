//! **Modal-Aware Image Occlusion** geometry and per-frame slot tracking (ADR-0004).
//!
//! Floating modals (command palette, help overlay, confirmation dialogs) must suppress
//! overlapping terminal graphics rectangles so Kitty, `iTerm2`, and Sixel images never
//! bleed over popup chrome.

/// Terminal cell rectangle used for Modal-Aware Image Occlusion checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    #[must_use]
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if `self` and `other` share at least one terminal cell.
    #[must_use]
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
#[must_use]
pub const fn is_occluded_by_modal(image_rect: CellRect, modal_rect: Option<CellRect>) -> bool {
    match modal_rect {
        Some(modal) => image_rect.intersects(modal),
        None => false,
    }
}

/// A registered image placement slot for the active TUI frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSlot {
    /// Logical UI slot identifier (e.g., `"now_playing_cover"`, `"track_row_3"`).
    pub id: String,
    /// Screen-space cell bounds occupied by the image.
    pub rect: CellRect,
    /// Source cover-art URL or demo seed key.
    pub url: String,
}

impl ImageSlot {
    /// Constructs a new frame image slot.
    #[must_use]
    pub fn new(id: impl Into<String>, rect: CellRect, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            rect,
            url: url.into(),
        }
    }
}

/// Tracks all image slots registered during a single render frame and filters them
/// against active floating modal bounds.
#[derive(Debug, Clone, Default)]
pub struct OcclusionTracker {
    slots: Vec<ImageSlot>,
}

impl OcclusionTracker {
    /// Creates an empty per-frame occlusion tracker.
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Clears slots from the previous frame at the start of a new render pass.
    pub fn begin_frame(&mut self) {
        self.slots.clear();
    }

    /// Registers an [`ImageSlot`] for the current frame.
    pub fn register(&mut self, slot: ImageSlot) {
        self.slots.push(slot);
    }

    /// Convenience helper to register a slot directly from `(id, rect, url)`.
    pub fn register_slot(&mut self, id: impl Into<String>, rect: CellRect, url: impl Into<String>) {
        self.slots.push(ImageSlot::new(id, rect, url));
    }

    /// Returns all slots registered for the current frame.
    #[must_use]
    pub fn slots(&self) -> &[ImageSlot] {
        &self.slots
    }

    /// Returns only the slots that are **not** occluded by `modal_rect`.
    #[must_use]
    pub fn visible_slots(&self, modal_rect: Option<CellRect>) -> Vec<&ImageSlot> {
        self.slots
            .iter()
            .filter(|slot| !is_occluded_by_modal(slot.rect, modal_rect))
            .collect()
    }

    /// Returns the slots that **are** occluded by `modal_rect` (e.g., so native
    /// graphics protocols can clear stale placements underneath a newly opened modal).
    #[must_use]
    pub fn occluded_slots(&self, modal_rect: Option<CellRect>) -> Vec<&ImageSlot> {
        self.slots
            .iter()
            .filter(|slot| is_occluded_by_modal(slot.rect, modal_rect))
            .collect()
    }

    /// Returns only the slots that do not intersect any rectangle in `modal_rects`.
    #[must_use]
    pub fn visible_slots_multi(&self, modal_rects: &[CellRect]) -> Vec<&ImageSlot> {
        self.slots
            .iter()
            .filter(|slot| !modal_rects.iter().any(|&modal| slot.rect.intersects(modal)))
            .collect()
    }

    /// Checks whether the registered slot with `id` is occluded by `modal_rect`.
    #[must_use]
    pub fn is_slot_occluded(&self, id: &str, modal_rect: Option<CellRect>) -> bool {
        self.slots
            .iter()
            .find(|slot| slot.id == id)
            .is_some_and(|slot| is_occluded_by_modal(slot.rect, modal_rect))
    }
}

#[cfg(test)]
mod tests {
    use super::{CellRect, OcclusionTracker, is_occluded_by_modal};

    #[test]
    fn modal_occlusion_suppresses_only_overlapping_image_rects() {
        let cover = CellRect::new(10, 5, 20, 10);
        let overlapping_modal = CellRect::new(25, 10, 40, 15);
        let disjoint_modal = CellRect::new(40, 5, 30, 10);

        assert!(is_occluded_by_modal(cover, Some(overlapping_modal)));
        assert!(!is_occluded_by_modal(cover, Some(disjoint_modal)));
        assert!(!is_occluded_by_modal(cover, None));
    }

    #[test]
    fn tracker_filters_visible_and_occluded_slots_per_frame() {
        let mut tracker = OcclusionTracker::new();
        tracker.register_slot("sidebar_cover", CellRect::new(2, 20, 16, 8), "cover://a");
        tracker.register_slot("hero_cover", CellRect::new(24, 4, 20, 10), "cover://b");
        tracker.register_slot("queue_thumb", CellRect::new(70, 6, 8, 4), "cover://c");

        let modal = Some(CellRect::new(20, 5, 36, 12));
        let visible = tracker.visible_slots(modal);
        let occluded = tracker.occluded_slots(modal);

        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].id, "sidebar_cover");
        assert_eq!(visible[1].id, "queue_thumb");
        assert_eq!(occluded.len(), 1);
        assert_eq!(occluded[0].id, "hero_cover");
        assert!(tracker.is_slot_occluded("hero_cover", modal));
        assert!(!tracker.is_slot_occluded("sidebar_cover", modal));

        // No modal reveals all slots.
        assert_eq!(tracker.visible_slots(None).len(), 3);
        assert_eq!(tracker.occluded_slots(None), Vec::<&_>::new());

        // New frame resets registered slots.
        tracker.begin_frame();
        assert_eq!(tracker.slots(), &[]);
    }
}
