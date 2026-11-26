//! Pure-Rust terminal image engine for `vynl`.
//!
//! Provides:
//! - Terminal graphics protocol detection and encoders for Kitty, `iTerm2`, Sixel,
//!   and 24-bit `TrueColor` Unicode halfblocks.
//! - `RGB24` image buffer, nearest/bilinear resizing, aspect-ratio cell fitting,
//!   PPM (`P6`) and BMP codecs, `Base64` encoding, procedural demo cover synthesis,
//!   and dominant color palette extraction for WCAG-AA adaptive themes.
//! - Two-tier bounded caching (`L1ImageCache` in-memory LRU + `L2DiskCache` PPM LRU)
//!   and a non-blocking prioritized background [`ImageWorker`] with viewport cancellation.
//! - **Modal-Aware Image Occlusion** geometry and per-frame [`OcclusionTracker`] (ADR-0004).

pub mod cache;
pub mod image;
pub mod occlusion;
pub mod protocol;
pub mod worker;

pub use crate::cache::{CachedFrame, ImageCacheKey, L1ImageCache, L2DiskCache};
pub use crate::image::{RgbImage, encode_base64, fnv1a_64, generate_demo_cover};
pub use crate::occlusion::{CellRect, ImageSlot, OcclusionTracker, is_occluded_by_modal};
pub use crate::protocol::{
    EncodedImage, ImageProtocol, encode_for_protocol, encode_halfblocks, encode_iterm2,
    encode_kitty, encode_sixel,
};
pub use crate::worker::{
    DEFAULT_IMAGE_TIMEOUT, DEFAULT_MAX_IMAGE_BYTES, DemoImageSource, HttpImageSource,
    ImagePriority, ImageReady, ImageRequest, ImageSource, ImageWorker,
};

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    use vynl_core::theme::{Theme, WCAG_AA_MIN_CONTRAST};

    use crate::{
        CachedFrame, CellRect, EncodedImage, ImagePriority, ImageProtocol, ImageRequest,
        ImageSource, ImageWorker, L1ImageCache, OcclusionTracker, RgbImage, encode_for_protocol,
        generate_demo_cover,
    };

    #[test]
    fn demo_cover_palette_integrates_with_adaptive_theme_wcag_aa() {
        let base = Theme::vynl_dark();
        let cover = generate_demo_cover("spotify:album:vynl-demo-01", 64);
        let palette = cover.extract_dominant_palette(5);
        assert_ne!(palette, []);

        let adapted = base.with_adaptive_palette(&palette);
        assert!(adapted.accent.contrast_ratio(adapted.background) >= WCAG_AA_MIN_CONTRAST);
        assert!(adapted.accent.contrast_ratio(adapted.surface) >= WCAG_AA_MIN_CONTRAST);
    }

    #[test]
    fn all_four_protocols_encode_non_empty_frames() {
        let cover = generate_demo_cover("spotify:album:all-protocols", 32);

        for protocol in [
            ImageProtocol::Kitty,
            ImageProtocol::ITerm2,
            ImageProtocol::Sixel,
            ImageProtocol::Halfblocks,
        ] {
            let encoded = encode_for_protocol(&cover, protocol, 10, 5, 8, 16);
            assert_eq!(encoded.protocol(), protocol);
            assert!(encoded.byte_len() > 0);
        }
    }

    struct BlockingSource {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    impl ImageSource for BlockingSource {
        fn load(&self, url: &str) -> Result<RgbImage, String> {
            if url == "gate" {
                let _ = self.entered.wait();
                let _ = self.release.wait();
            }
            Ok(generate_demo_cover(url, 16))
        }
    }

    #[test]
    fn worker_prioritizes_high_over_low_requests_and_populates_l1_cache() -> Result<(), String> {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let source: Arc<dyn ImageSource> = Arc::new(BlockingSource {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        // Single worker thread guarantees deterministic ordering after gate opens.
        let worker = ImageWorker::spawn(1, &source, None)?;

        let make_req = |id: u64, url: &str, priority: ImagePriority| ImageRequest {
            id,
            viewport_epoch: 0,
            url: String::from(url),
            cols: 8,
            rows: 4,
            cell_px_w: 8,
            cell_px_h: 16,
            protocol: ImageProtocol::Halfblocks,
            priority,
            palette_colors: 3,
        };

        // First request occupies the worker thread; wait until it has been dequeued.
        worker.submit(make_req(1, "gate", ImagePriority::Normal))?;
        let _ = entered.wait();

        // Queue low-priority then high-priority while worker is blocked on release.
        worker.submit(make_req(2, "low-thumb", ImagePriority::Low))?;
        worker.submit(make_req(3, "high-hero", ImagePriority::High))?;

        // Release the worker thread.
        let _ = release.wait();

        let r1 = worker
            .recv_timeout(Duration::from_secs(2))
            .ok_or_else(|| String::from("missing gate response"))?;
        let r2 = worker
            .recv_timeout(Duration::from_secs(2))
            .ok_or_else(|| String::from("missing second response"))?;
        let r3 = worker
            .recv_timeout(Duration::from_secs(2))
            .ok_or_else(|| String::from("missing third response"))?;

        assert_eq!(r1.id, 1);
        assert_eq!(r2.id, 3); // High priority preempted Low priority!
        assert_eq!(r3.id, 2);

        let mut l1 = L1ImageCache::new(4);
        let key = r2.key.clone();
        l1.insert(
            key.clone(),
            CachedFrame {
                encoded: r2.encoded,
                palette: r2.palette,
            },
        );
        let cached = l1
            .get(&key)
            .ok_or_else(|| String::from("expected cached frame in L1"))?;
        assert!(matches!(cached.encoded, EncodedImage::Halfblocks(ref rows) if rows.len() == 4));

        // Verify occlusion tracker integration with modal bounds.
        let mut tracker = OcclusionTracker::new();
        tracker.register_slot("hero", CellRect::new(4, 4, 16, 8), "high-hero");
        tracker.register_slot("footer", CellRect::new(2, 24, 8, 4), "low-thumb");
        let modal = Some(CellRect::new(10, 6, 30, 10));
        let visible = tracker.visible_slots(modal);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "footer");

        worker.shutdown();
        Ok(())
    }
}
