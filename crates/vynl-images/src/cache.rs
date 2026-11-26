//! Two-tier bounded image caching: L1 in-memory LRU frame cache and L2 on-disk
//! PPM cache with byte-budget LRU eviction.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use vynl_core::theme::Rgb;

use crate::image::{RgbImage, fnv1a_64};
use crate::protocol::{EncodedImage, ImageProtocol};

/// Cache lookup key for an encoded terminal image frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageCacheKey {
    /// Source cover-art URL or deterministic demo seed.
    pub url: String,
    /// Target width in terminal columns.
    pub cols: u16,
    /// Target height in terminal rows.
    pub rows: u16,
    /// Terminal image protocol used to encode the frame.
    pub protocol: ImageProtocol,
}

impl ImageCacheKey {
    /// Constructs a new L1 cache key.
    #[must_use]
    pub fn new(url: impl Into<String>, cols: u16, rows: u16, protocol: ImageProtocol) -> Self {
        Self {
            url: url.into(),
            cols,
            rows,
            protocol,
        }
    }
}

impl From<(String, u16, u16, ImageProtocol)> for ImageCacheKey {
    fn from((url, cols, rows, protocol): (String, u16, u16, ImageProtocol)) -> Self {
        Self {
            url,
            cols,
            rows,
            protocol,
        }
    }
}

/// Cached encoded terminal frame plus its extracted dominant cover-art palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedFrame {
    /// Protocol-encoded terminal frame ready for rendering.
    pub encoded: EncodedImage,
    /// Dominant cover-art colors for WCAG-AA adaptive theme accents.
    pub palette: Vec<Rgb>,
}

/// Bounded in-memory LRU cache keyed by `(url, cols, rows, protocol)`.
#[derive(Debug)]
pub struct L1ImageCache {
    capacity: usize,
    tick: u64,
    entries: HashMap<ImageCacheKey, (CachedFrame, u64)>,
}

impl L1ImageCache {
    /// Creates a new bounded L1 LRU cache holding up to `capacity` encoded frames.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            tick: 0,
            entries: HashMap::with_capacity(capacity.min(1024)),
        }
    }

    /// Maximum number of frames retained in memory.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current number of cached frames.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no frames are cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up a frame by `key` and marks it as most-recently used.
    pub fn get(&mut self, key: &ImageCacheKey) -> Option<&CachedFrame> {
        let next_tick = self.next_tick();
        if let Some((frame, last_used)) = self.entries.get_mut(key) {
            *last_used = next_tick;
            Some(frame)
        } else {
            None
        }
    }

    /// Looks up a frame without updating LRU recency.
    #[must_use]
    pub fn peek(&self, key: &ImageCacheKey) -> Option<&CachedFrame> {
        self.entries.get(key).map(|(frame, _)| frame)
    }

    /// Inserts `frame` under `key`, evicting the least-recently-used entry if at capacity.
    ///
    /// Returns the evicted `(key, frame)` pair when eviction occurs.
    pub fn insert(
        &mut self,
        key: ImageCacheKey,
        frame: CachedFrame,
    ) -> Option<(ImageCacheKey, CachedFrame)> {
        if self.capacity == 0 {
            return None;
        }

        let next_tick = self.next_tick();
        if let Some(existing) = self.entries.get_mut(&key) {
            *existing = (frame, next_tick);
            return None;
        }

        let mut evicted = None;
        if self.entries.len() >= self.capacity
            && let Some(lru_key) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| k.clone())
            && let Some((evicted_frame, _)) = self.entries.remove(&lru_key)
        {
            evicted = Some((lru_key, evicted_frame));
        }

        self.entries.insert(key, (frame, next_tick));
        evicted
    }

    /// Removes and returns the cached frame for `key`, if present.
    pub fn remove(&mut self, key: &ImageCacheKey) -> Option<CachedFrame> {
        self.entries.remove(key).map(|(frame, _)| frame)
    }

    /// Drops all frames from the L1 cache.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    const fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }
}

/// Bounded on-disk PPM cache (`<dir>/<fnv1a64>.ppm`) with byte-budget LRU eviction.
#[derive(Debug)]
pub struct L2DiskCache {
    dir: PathBuf,
    max_bytes: u64,
    recency: Mutex<DiskRecency>,
}

#[derive(Debug, Default)]
struct DiskRecency {
    tick: u64,
    last_used: HashMap<PathBuf, u64>,
}

impl DiskRecency {
    fn touch(&mut self, path: PathBuf) {
        self.tick = self.tick.saturating_add(1);
        self.last_used.insert(path, self.tick);
    }

    fn remove(&mut self, path: &Path) {
        self.last_used.remove(path);
    }

    fn rank(&self, path: &Path) -> u64 {
        self.last_used.get(path).copied().unwrap_or(0)
    }
}

impl L2DiskCache {
    /// Resolves the default L2 image cache directory (`~/.cache/vynl/images`).
    #[must_use]
    pub fn default_cache_dir() -> PathBuf {
        if let Ok(xdg) = env::var("XDG_CACHE_HOME")
            && !xdg.is_empty()
        {
            return PathBuf::from(xdg).join("vynl").join("images");
        }
        if let Ok(home) = env::var("HOME")
            && !home.is_empty()
        {
            return PathBuf::from(home)
                .join(".cache")
                .join("vynl")
                .join("images");
        }
        env::temp_dir().join("vynl").join("images")
    }

    /// Opens or creates an L2 disk cache in `dir` capped at `max_bytes`.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created.
    pub fn new(dir: PathBuf, max_bytes: u64) -> Result<Self, String> {
        fs::create_dir_all(&dir)
            .map_err(|err| format!("failed to create image cache dir {}: {err}", dir.display()))?;
        Ok(Self {
            dir,
            max_bytes,
            recency: Mutex::new(DiskRecency::default()),
        })
    }

    /// Opens or creates an L2 disk cache in [`Self::default_cache_dir`] using a MiB budget.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created.
    pub fn from_max_mb(max_disk_cache_mb: u32) -> Result<Self, String> {
        let max_bytes = u64::from(max_disk_cache_mb).saturating_mul(1024 * 1024);
        Self::new(Self::default_cache_dir(), max_bytes)
    }

    /// Returns the root directory storing cached `.ppm` files.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the maximum byte budget enforced on disk.
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Computes the deterministic `.ppm` file path for `key` using 64-bit FNV-1a hashing.
    #[must_use]
    pub fn path_for_key(&self, key: &str) -> PathBuf {
        let hash = fnv1a_64(key);
        self.dir.join(format!("{hash:016x}.ppm"))
    }

    /// Loads and decodes a cached image from disk if present, updating LRU recency.
    ///
    /// # Errors
    /// Returns an error if the file exists but cannot be read or contains corrupt PPM data.
    pub fn get(&self, key: &str) -> Result<Option<RgbImage>, String> {
        let path = self.path_for_key(key);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)
            .map_err(|err| format!("failed to read cached image {}: {err}", path.display()))?;
        let image = RgbImage::decode_ppm(&bytes)?;
        if let Ok(mut guard) = self.recency.lock() {
            guard.touch(path);
        }
        Ok(Some(image))
    }

    /// Encodes `image` as PPM (`P6`), writes it atomically to disk, and enforces the
    /// byte-budget LRU limit.
    ///
    /// # Errors
    /// Returns an error if writing, renaming, or evicting cache files fails.
    pub fn put(&self, key: &str, image: &RgbImage) -> Result<PathBuf, String> {
        let path = self.path_for_key(key);
        let ppm = image.encode_ppm();
        let tmp_name = format!("{:016x}.{}.tmp", fnv1a_64(key), std::process::id());
        let tmp_path = self.dir.join(tmp_name);

        fs::write(&tmp_path, &ppm)
            .map_err(|err| format!("failed to write temp cache {}: {err}", tmp_path.display()))?;

        if let Err(rename_err) = fs::rename(&tmp_path, &path) {
            let _ = fs::remove_file(&path);
            fs::rename(&tmp_path, &path).map_err(|err| {
                let _ = fs::remove_file(&tmp_path);
                format!(
                    "failed to commit cache file {} (first err: {rename_err}): {err}",
                    path.display()
                )
            })?;
        }

        if let Ok(mut guard) = self.recency.lock() {
            guard.touch(path.clone());
        }

        let _ = self.evict_if_needed()?;
        Ok(path)
    }

    /// Exports a cached image for `key` to `dest` as a lossless PNG file.
    ///
    /// Returns `Ok(false)` on cache miss without creating `dest`. On cache hit,
    /// creates parent directories if needed, atomically writes the PNG via a
    /// temporary file and rename, and returns `Ok(true)`.
    ///
    /// # Errors
    /// Returns an error if reading the cached PPM entry, encoding PNG bytes,
    /// creating parent directories, or atomically writing `dest` fails.
    pub fn export_png(&self, key: &str, dest: &Path) -> Result<bool, String> {
        static EXPORT_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

        let Some(img) = self.get(key)? else {
            return Ok(false);
        };

        let png_bytes = img.encode_png()?;

        if let Some(parent) = dest.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "failed to create PNG export directory {}: {err}",
                    parent.display()
                )
            })?;
        }

        let seq = EXPORT_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(
            ".vynl-export-{:016x}.{}.{seq}.tmp",
            fnv1a_64(key),
            std::process::id()
        );
        let tmp_path = match dest.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
            _ => PathBuf::from(tmp_name),
        };

        fs::write(&tmp_path, &png_bytes).map_err(|err| {
            format!(
                "failed to write temp PNG export {}: {err}",
                tmp_path.display()
            )
        })?;

        if let Err(rename_err) = fs::rename(&tmp_path, dest) {
            let _ = fs::remove_file(dest);
            fs::rename(&tmp_path, dest).map_err(|err| {
                let _ = fs::remove_file(&tmp_path);
                format!(
                    "failed to commit PNG export {} (first err: {rename_err}): {err}",
                    dest.display()
                )
            })?;
        }

        Ok(true)
    }

    /// Returns the total size in bytes of all `.ppm` files currently stored in the cache.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be read.
    pub fn total_bytes(&self) -> Result<u64, String> {
        let entries = self.list_ppm_entries()?;
        Ok(entries.iter().map(|entry| entry.size).sum())
    }

    /// Evicts least-recently-used `.ppm` files until total disk usage is within `max_bytes`.
    ///
    /// Returns the number of evicted files.
    ///
    /// # Errors
    /// Returns an error if directory listing or file deletion fails.
    pub fn evict_if_needed(&self) -> Result<usize, String> {
        let mut entries = self.list_ppm_entries()?;
        let mut total: u64 = entries.iter().map(|entry| entry.size).sum();
        if total <= self.max_bytes {
            return Ok(0);
        }

        // Oldest logical recency first, breaking ties by filesystem modification time
        // and file path for full determinism.
        entries.sort_by(|a, b| {
            a.recency_rank
                .cmp(&b.recency_rank)
                .then_with(|| a.modified_nanos.cmp(&b.modified_nanos))
                .then_with(|| a.path.cmp(&b.path))
        });

        let mut evicted = 0usize;
        for entry in entries {
            if total <= self.max_bytes {
                break;
            }
            if fs::remove_file(&entry.path).is_ok() {
                total = total.saturating_sub(entry.size);
                evicted += 1;
                if let Ok(mut guard) = self.recency.lock() {
                    guard.remove(&entry.path);
                }
            }
        }

        Ok(evicted)
    }

    /// Deletes a single cached `.ppm` entry for `key`. Returns `true` if a file was removed.
    ///
    /// # Errors
    /// Returns an error if the file exists and cannot be removed.
    pub fn remove(&self, key: &str) -> Result<bool, String> {
        let path = self.path_for_key(key);
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(&path)
            .map_err(|err| format!("failed to remove cached image {}: {err}", path.display()))?;
        if let Ok(mut guard) = self.recency.lock() {
            guard.remove(&path);
        }
        Ok(true)
    }

    /// Removes all `.ppm` files inside the cache directory.
    ///
    /// # Errors
    /// Returns an error if reading the cache directory fails.
    pub fn clear(&self) -> Result<usize, String> {
        let entries = self.list_ppm_entries()?;
        let mut removed = 0usize;
        for entry in entries {
            if fs::remove_file(&entry.path).is_ok() {
                removed += 1;
                if let Ok(mut guard) = self.recency.lock() {
                    guard.remove(&entry.path);
                }
            }
        }
        Ok(removed)
    }

    fn list_ppm_entries(&self) -> Result<Vec<DiskEntry>, String> {
        let read_dir = fs::read_dir(&self.dir)
            .map_err(|err| format!("failed to read cache dir {}: {err}", self.dir.display()))?;

        let recency_snapshot = self.recency.lock().ok();
        let mut entries = Vec::new();

        for dir_entry in read_dir.flatten() {
            let path = dir_entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("ppm") {
                continue;
            }
            let Ok(meta) = dir_entry.metadata() else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let modified_nanos = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            let recency_rank = recency_snapshot
                .as_ref()
                .map_or(0, |guard| guard.rank(&path));

            entries.push(DiskEntry {
                path,
                size: meta.len(),
                recency_rank,
                modified_nanos,
            });
        }

        drop(recency_snapshot);
        let _ = SystemTime::now();
        Ok(entries)
    }
}

struct DiskEntry {
    path: PathBuf,
    size: u64,
    recency_rank: u64,
    modified_nanos: u128,
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use vynl_core::theme::Rgb;

    use crate::image::generate_demo_cover;
    use crate::protocol::{EncodedImage, ImageProtocol};

    use super::{CachedFrame, ImageCacheKey, L1ImageCache, L2DiskCache};

    fn make_frame(tag: &str) -> CachedFrame {
        CachedFrame {
            encoded: EncodedImage::Kitty(String::from(tag)),
            palette: vec![Rgb::new(29, 185, 84)],
        }
    }

    #[test]
    fn l1_lru_evicts_least_recently_used_entry() {
        let mut cache = L1ImageCache::new(2);
        let k1 = ImageCacheKey::new("cover://1", 16, 8, ImageProtocol::Kitty);
        let k2 = ImageCacheKey::new("cover://2", 16, 8, ImageProtocol::Kitty);
        let k3 = ImageCacheKey::new("cover://3", 16, 8, ImageProtocol::Kitty);

        assert!(cache.insert(k1.clone(), make_frame("one")).is_none());
        assert!(cache.insert(k2.clone(), make_frame("two")).is_none());

        // Touch k1 so k2 becomes the least-recently-used entry.
        assert_eq!(
            cache.get(&k1).map(|f| &f.encoded),
            Some(&EncodedImage::Kitty(String::from("one")))
        );

        let evicted = cache.insert(k3.clone(), make_frame("three"));
        assert_eq!(evicted.map(|(k, _)| k), Some(k2.clone()));
        assert!(cache.peek(&k1).is_some());
        assert!(cache.peek(&k2).is_none());
        assert!(cache.peek(&k3).is_some());
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn l2_disk_cache_round_trips_and_enforces_byte_budget() -> Result<(), String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = env::temp_dir().join(format!(
            "vynl-images-l2-test-{}-{nonce}",
            std::process::id()
        ));

        let img1 = generate_demo_cover("disk://1", 16);
        let img2 = generate_demo_cover("disk://2", 16);
        let img3 = generate_demo_cover("disk://3", 16);
        let single_size =
            u64::try_from(img1.encode_ppm().len()).map_err(|_| String::from("ppm len overflow"))?;

        // Budget allows two 16x16 PPM files, but not three.
        let cache = L2DiskCache::new(dir.clone(), single_size * 2 + 16)?;
        cache.put("disk://1", &img1)?;
        cache.put("disk://2", &img2)?;

        // Access disk://1 so disk://2 becomes the LRU candidate.
        assert_eq!(cache.get("disk://1")?, Some(img1.clone()));

        cache.put("disk://3", &img3)?;
        assert!(cache.total_bytes()? <= cache.max_bytes());
        assert_eq!(cache.get("disk://1")?, Some(img1));
        assert_eq!(cache.get("disk://2")?, None);
        assert_eq!(cache.get("disk://3")?, Some(img3));

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn l2_disk_cache_export_png_handles_miss_and_hit() -> Result<(), String> {
        use crate::image::RgbImage;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let root = env::temp_dir().join(format!(
            "vynl-images-export-png-{}-{nonce}",
            std::process::id()
        ));
        let cache_dir = root.join("cache");
        let export_dest = root.join("nested").join("icons").join("cover.png");

        let cache = L2DiskCache::new(cache_dir, 1024 * 1024)?;

        // Cache miss returns Ok(false) and does not create the destination file.
        assert!(!cache.export_png("missing://cover", &export_dest)?);
        assert!(!export_dest.exists());

        // Cache hit creates parent dirs, writes a valid lossless PNG, and returns Ok(true).
        let cover = generate_demo_cover("export://cover", 20);
        cache.put("export://cover", &cover)?;
        assert!(cache.export_png("export://cover", &export_dest)?);
        assert!(export_dest.exists());

        let exported_bytes =
            fs::read(&export_dest).map_err(|err| format!("failed to read exported PNG: {err}"))?;
        assert!(exported_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

        let decoded = RgbImage::decode_encoded(&exported_bytes)?;
        assert_eq!(decoded, cover);

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }
}
