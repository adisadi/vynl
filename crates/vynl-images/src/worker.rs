//! Non-blocking background image worker pool with request prioritization and
//! instant viewport cancellation.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use vynl_core::theme::Rgb;

use crate::cache::{ImageCacheKey, L2DiskCache};
use crate::image::{RgbImage, generate_demo_cover};
use crate::protocol::{EncodedImage, ImageProtocol, encode_for_protocol};

/// Scheduling priority for background image work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum ImagePriority {
    /// Speculative prefetch or off-screen list thumbnails.
    Low = 0,
    /// Visible track/search list thumbnails.
    #[default]
    Normal = 1,
    /// Hero header or Now Playing cover art currently in focus.
    High = 2,
}

/// Request submitted from the UI thread to [`ImageWorker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRequest {
    /// Caller-assigned unique request identifier.
    pub id: u64,
    /// Viewport epoch used for bulk cancellation on fast scrolling or view switches.
    pub viewport_epoch: u64,
    /// Source cover-art URL or deterministic demo seed.
    pub url: String,
    /// Target display width in terminal columns.
    pub cols: u16,
    /// Target display height in terminal rows.
    pub rows: u16,
    /// Terminal cell width in pixels (used for aspect-preserving fitting).
    pub cell_px_w: u16,
    /// Terminal cell height in pixels (used for aspect-preserving fitting).
    pub cell_px_h: u16,
    /// Terminal graphics protocol to encode for.
    pub protocol: ImageProtocol,
    /// Queue scheduling priority.
    pub priority: ImagePriority,
    /// Maximum number of dominant colors to extract for adaptive theming.
    pub palette_colors: usize,
}

impl ImageRequest {
    /// Builds the corresponding [`ImageCacheKey`] for this request.
    #[must_use]
    pub fn cache_key(&self) -> ImageCacheKey {
        ImageCacheKey::new(self.url.clone(), self.cols, self.rows, self.protocol)
    }
}

/// Completed image frame delivered from [`ImageWorker`] back to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReady {
    /// Originating request identifier.
    pub id: u64,
    /// Viewport epoch of the request.
    pub viewport_epoch: u64,
    /// Cache key for direct insertion into [`crate::cache::L1ImageCache`].
    pub key: ImageCacheKey,
    /// Encoded terminal graphics payload.
    pub encoded: EncodedImage,
    /// Extracted dominant cover-art color palette.
    pub palette: Vec<Rgb>,
    /// Indicates whether raw pixels were satisfied from the L2 disk cache.
    pub from_disk_cache: bool,
}

/// Pluggable raw image loader seam used by [`ImageWorker`].
///
/// Production builds wire a Spotify CDN HTTP fetcher behind this trait, while offline
/// builds and tests use [`DemoImageSource`].
pub trait ImageSource: Send + Sync + 'static {
    /// Loads raw `RGB24` pixels for `url`.
    ///
    /// # Errors
    /// Returns a human-readable error if fetching or decoding fails.
    fn load(&self, url: &str) -> Result<RgbImage, String>;
}

/// Offline procedural cover-art [`ImageSource`] backed by [`generate_demo_cover`].
#[derive(Debug, Clone, Copy)]
pub struct DemoImageSource {
    /// Square pixel size of generated cover art.
    pub cover_size: u32,
}

impl Default for DemoImageSource {
    fn default() -> Self {
        Self { cover_size: 96 }
    }
}

impl ImageSource for DemoImageSource {
    fn load(&self, url: &str) -> Result<RgbImage, String> {
        Ok(generate_demo_cover(url, self.cover_size))
    }
}

#[derive(Debug)]
struct QueuedRequest {
    seq: u64,
    request: ImageRequest,
}

impl PartialEq for QueuedRequest {
    fn eq(&self, other: &Self) -> bool {
        self.request.priority == other.request.priority && self.seq == other.seq
    }
}

impl Eq for QueuedRequest {}

impl PartialOrd for QueuedRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for QueuedRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        self.request
            .priority
            .cmp(&other.request.priority)
            // Earlier submission seq wins among equal priorities (FIFO per priority tier).
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct SharedQueue {
    heap: BinaryHeap<QueuedRequest>,
    cancelled_ids: HashSet<u64>,
    next_seq: u64,
    shutdown: bool,
}

impl SharedQueue {
    fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            cancelled_ids: HashSet::new(),
            next_seq: 0,
            shutdown: false,
        }
    }
}

struct WorkerState {
    queue: Mutex<SharedQueue>,
    available: Condvar,
    viewport_epoch: AtomicU64,
}

impl WorkerState {
    fn is_cancelled(&self, id: u64, viewport_epoch: u64) -> bool {
        if viewport_epoch < self.viewport_epoch.load(AtomicOrdering::Acquire) {
            return true;
        }
        self.queue
            .lock()
            .is_ok_and(|guard| guard.cancelled_ids.contains(&id))
    }
}

/// Non-blocking background thread pool that loads, caches, resizes, extracts palettes,
/// and encodes terminal images without ever stalling the TUI render loop.
pub struct ImageWorker {
    state: Arc<WorkerState>,
    ready_rx: mpsc::Receiver<ImageReady>,
    handles: Vec<JoinHandle<()>>,
}

impl ImageWorker {
    /// Spawns a background pool with `worker_threads` (minimum 1) threads.
    ///
    /// # Errors
    /// Returns an error if spawning any OS worker thread fails.
    pub fn spawn(
        worker_threads: usize,
        source: &Arc<dyn ImageSource>,
        disk_cache: Option<&Arc<L2DiskCache>>,
    ) -> Result<Self, String> {
        let count = worker_threads.max(1);
        let state = Arc::new(WorkerState {
            queue: Mutex::new(SharedQueue::new()),
            available: Condvar::new(),
            viewport_epoch: AtomicU64::new(0),
        });
        let (ready_tx, ready_rx) = mpsc::channel();

        let mut handles = Vec::with_capacity(count);
        for idx in 0..count {
            let thread_state = Arc::clone(&state);
            let thread_source = Arc::clone(source);
            let thread_cache = disk_cache.map(Arc::clone);
            let thread_tx = ready_tx.clone();

            let handle = thread::Builder::new()
                .name(format!("vynl-img-{idx}"))
                .spawn(move || {
                    worker_loop(
                        &thread_state,
                        &thread_source,
                        thread_cache.as_deref(),
                        &thread_tx,
                    );
                })
                .map_err(|err| format!("failed to spawn image worker thread {idx}: {err}"))?;
            handles.push(handle);
        }

        Ok(Self {
            state,
            ready_rx,
            handles,
        })
    }

    /// Convenience constructor backed by [`DemoImageSource`].
    ///
    /// # Errors
    /// Returns an error if spawning worker threads fails.
    pub fn spawn_demo(
        worker_threads: usize,
        disk_cache: Option<&Arc<L2DiskCache>>,
    ) -> Result<Self, String> {
        let source: Arc<dyn ImageSource> = Arc::new(DemoImageSource::default());
        Self::spawn(worker_threads, &source, disk_cache)
    }

    /// Enqueues an [`ImageRequest`] without blocking the caller.
    ///
    /// Stale requests older than [`Self::current_viewport`] are ignored immediately.
    ///
    /// # Errors
    /// Returns an error if the worker queue mutex is poisoned or the pool has shut down.
    pub fn submit(&self, request: ImageRequest) -> Result<(), String> {
        if request.viewport_epoch < self.current_viewport() {
            return Ok(());
        }
        let mut guard = self
            .state
            .queue
            .lock()
            .map_err(|_| String::from("image worker queue mutex poisoned"))?;
        if guard.shutdown {
            return Err(String::from("image worker has already shut down"));
        }
        let seq = guard.next_seq;
        guard.next_seq = guard.next_seq.saturating_add(1);
        guard.cancelled_ids.remove(&request.id);
        guard.heap.push(QueuedRequest { seq, request });
        drop(guard);
        self.state.available.notify_one();
        Ok(())
    }

    /// Cancels a specific queued or in-flight request by `id`.
    pub fn cancel_request(&self, id: u64) {
        if let Ok(mut guard) = self.state.queue.lock() {
            guard.cancelled_ids.insert(id);
        }
    }

    /// Advances the active viewport epoch and purges all queued requests belonging
    /// to older viewports, returning the new epoch.
    pub fn advance_viewport(&self) -> u64 {
        let next_epoch = self
            .state
            .viewport_epoch
            .fetch_add(1, AtomicOrdering::AcqRel)
            .saturating_add(1);
        if let Ok(mut guard) = self.state.queue.lock() {
            let retained: BinaryHeap<QueuedRequest> = guard
                .heap
                .drain()
                .filter(|item| item.request.viewport_epoch >= next_epoch)
                .collect();
            guard.heap = retained;
            guard.cancelled_ids.clear();
        }
        next_epoch
    }

    /// Returns the currently active viewport epoch.
    #[must_use]
    pub fn current_viewport(&self) -> u64 {
        self.state.viewport_epoch.load(AtomicOrdering::Acquire)
    }

    /// Returns the number of requests currently waiting in the queue.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.state.queue.lock().map_or(0, |guard| guard.heap.len())
    }

    /// Non-blocking poll for a single ready image frame.
    #[must_use]
    pub fn try_recv(&self) -> Option<ImageReady> {
        self.ready_rx.try_recv().ok()
    }

    /// Drains all currently ready frames without blocking the UI thread.
    #[must_use]
    pub fn drain_ready(&self) -> Vec<ImageReady> {
        let mut out = Vec::new();
        while let Ok(item) = self.ready_rx.try_recv() {
            out.push(item);
        }
        out
    }

    /// Waits up to `timeout` for the next ready frame.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ImageReady> {
        self.ready_rx.recv_timeout(timeout).ok()
    }

    /// Gracefully stops all worker threads and waits for them to exit.
    pub fn shutdown(mut self) {
        self.stop_workers();
    }

    fn stop_workers(&mut self) {
        if let Ok(mut guard) = self.state.queue.lock() {
            guard.shutdown = true;
            guard.heap.clear();
        }
        self.state.available.notify_all();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for ImageWorker {
    fn drop(&mut self) {
        self.stop_workers();
    }
}

fn worker_loop(
    state: &WorkerState,
    source: &Arc<dyn ImageSource>,
    disk_cache: Option<&L2DiskCache>,
    ready_tx: &mpsc::Sender<ImageReady>,
) {
    loop {
        let request = {
            let Ok(mut guard) = state.queue.lock() else {
                return;
            };
            while guard.heap.is_empty() && !guard.shutdown {
                let Ok(next_guard) = state.available.wait(guard) else {
                    return;
                };
                guard = next_guard;
            }
            if guard.shutdown {
                return;
            }
            let Some(item) = guard.heap.pop() else {
                continue;
            };
            if guard.cancelled_ids.remove(&item.request.id) {
                continue;
            }
            item.request
        };

        if state.is_cancelled(request.id, request.viewport_epoch) {
            continue;
        }

        let (raw_image, from_disk_cache) =
            if let Some(cached) = disk_cache.and_then(|c| c.get(&request.url).ok().flatten()) {
                (cached, true)
            } else {
                let Ok(loaded) = source.load(&request.url) else {
                    continue;
                };
                if let Some(cache) = disk_cache {
                    let _ = cache.put(&request.url, &loaded);
                }
                (loaded, false)
            };

        if state.is_cancelled(request.id, request.viewport_epoch) {
            continue;
        }

        let palette = raw_image.extract_dominant_palette(request.palette_colors);
        let encoded = encode_for_protocol(
            &raw_image,
            request.protocol,
            request.cols,
            request.rows,
            request.cell_px_w,
            request.cell_px_h,
        );

        if state.is_cancelled(request.id, request.viewport_epoch) {
            continue;
        }

        let ready = ImageReady {
            id: request.id,
            viewport_epoch: request.viewport_epoch,
            key: request.cache_key(),
            encoded,
            palette,
            from_disk_cache,
        };
        if ready_tx.send(ready).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::cache::L2DiskCache;
    use crate::protocol::{EncodedImage, ImageProtocol};

    use super::{ImagePriority, ImageRequest, ImageWorker};

    #[test]
    fn worker_processes_requests_with_l2_disk_cache_hit() -> Result<(), String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = env::temp_dir().join(format!(
            "vynl-images-worker-test-{}-{nonce}",
            std::process::id()
        ));
        let disk = Arc::new(L2DiskCache::new(dir.clone(), 4 * 1024 * 1024)?);
        let worker = ImageWorker::spawn_demo(2, Some(&disk))?;

        let req1 = ImageRequest {
            id: 1,
            viewport_epoch: 0,
            url: String::from("demo://album/midnight"),
            cols: 12,
            rows: 6,
            cell_px_w: 8,
            cell_px_h: 16,
            protocol: ImageProtocol::Kitty,
            priority: ImagePriority::High,
            palette_colors: 4,
        };
        worker.submit(req1)?;

        let first = worker
            .recv_timeout(Duration::from_secs(2))
            .ok_or_else(|| String::from("timed out waiting for first image"))?;
        assert_eq!(first.id, 1);
        assert!(!first.from_disk_cache);
        assert_ne!(first.palette, []);
        assert!(matches!(first.encoded, EncodedImage::Kitty(ref s) if !s.is_empty()));

        // Second request for the same URL should hit L2 disk cache.
        let req2 = ImageRequest {
            id: 2,
            viewport_epoch: 0,
            url: String::from("demo://album/midnight"),
            cols: 8,
            rows: 4,
            cell_px_w: 8,
            cell_px_h: 16,
            protocol: ImageProtocol::Halfblocks,
            priority: ImagePriority::Normal,
            palette_colors: 3,
        };
        worker.submit(req2)?;

        let second = worker
            .recv_timeout(Duration::from_secs(2))
            .ok_or_else(|| String::from("timed out waiting for cached image"))?;
        assert_eq!(second.id, 2);
        assert!(second.from_disk_cache);
        assert!(matches!(second.encoded, EncodedImage::Halfblocks(ref rows) if rows.len() == 4));

        worker.shutdown();
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn advancing_viewport_cancels_stale_requests() -> Result<(), String> {
        let worker = ImageWorker::spawn_demo(1, None)?;
        let epoch = worker.advance_viewport();
        assert_eq!(epoch, 1);

        // Stale epoch 0 request must be dropped immediately on submit.
        worker.submit(ImageRequest {
            id: 10,
            viewport_epoch: 0,
            url: String::from("demo://stale"),
            cols: 6,
            rows: 3,
            cell_px_w: 8,
            cell_px_h: 16,
            protocol: ImageProtocol::Sixel,
            priority: ImagePriority::Low,
            palette_colors: 2,
        })?;
        assert_eq!(worker.pending_count(), 0);

        // Active epoch 1 request succeeds.
        worker.submit(ImageRequest {
            id: 11,
            viewport_epoch: epoch,
            url: String::from("demo://active"),
            cols: 6,
            rows: 3,
            cell_px_w: 8,
            cell_px_h: 16,
            protocol: ImageProtocol::ITerm2,
            priority: ImagePriority::High,
            palette_colors: 3,
        })?;

        let ready = worker
            .recv_timeout(Duration::from_secs(2))
            .ok_or_else(|| String::from("timed out waiting for active epoch image"))?;
        assert_eq!(ready.id, 11);
        assert_eq!(ready.viewport_epoch, 1);
        assert!(matches!(ready.encoded, EncodedImage::ITerm2(ref s) if !s.is_empty()));

        worker.shutdown();
        Ok(())
    }
}
