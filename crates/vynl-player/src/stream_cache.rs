//! Bounded LRU on-disk cache for encrypted/opaque audio stream chunks.
//!
//! Stores chunks under `~/.cache/vynl/audio` by default with a `256 MiB` cap.
//! Setting `max_bytes == 0` disables disk caching completely without touching
//! the filesystem.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::error::PlayerError;

/// Number of bytes in one mebibyte (`1024 * 1024`).
pub const BYTES_PER_MIB: u64 = 1024 * 1024;

/// Default bounded stream cache size (`256 MiB`).
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 256 * BYTES_PER_MIB;

const CHUNK_EXTENSION: &str = ".chunk";

/// Returns the default audio stream cache directory (`~/.cache/vynl/audio`).
#[must_use]
pub fn default_cache_dir() -> PathBuf {
    if let Some(xdg) = env::var_os("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("vynl").join("audio");
    }
    if let Some(home) = env::var_os("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home)
            .join(".cache")
            .join("vynl")
            .join("audio");
    }
    env::temp_dir().join("vynl").join("audio")
}

/// Snapshot of runtime metrics tracked by [`StreamCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamCacheStats {
    /// Number of chunk entries currently indexed in the cache.
    pub entry_count: usize,
    /// Exact sum of stored chunk bytes currently on disk.
    pub current_bytes: u64,
    /// Configured maximum cache capacity in bytes (`0` means disabled).
    pub max_bytes: u64,
    /// Total cache read hits.
    pub hits: u64,
    /// Total cache read misses.
    pub misses: u64,
    /// Total entries evicted to respect `max_bytes`.
    pub evictions: u64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    file_name: String,
    size_bytes: u64,
    access_seq: u64,
}

/// Bounded LRU disk cache manager for encrypted or opaque audio chunks.
#[derive(Debug)]
pub struct StreamCache {
    dir: PathBuf,
    max_bytes: u64,
    current_bytes: u64,
    next_seq: u64,
    entries: HashMap<String, CacheEntry>,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl StreamCache {
    /// Opens or initializes a bounded audio stream cache at `dir`.
    ///
    /// When `max_bytes == 0`, disk caching is disabled and the directory is not created.
    /// Existing `.chunk` files in `dir` are indexed in modification-time order and
    /// immediately trimmed if they exceed `max_bytes`.
    ///
    /// # Errors
    /// Returns [`PlayerError::Io`] if directory creation or inspection fails.
    pub fn open(dir: impl Into<PathBuf>, max_bytes: u64) -> Result<Self, PlayerError> {
        let dir = dir.into();
        let mut cache = Self {
            dir,
            max_bytes,
            current_bytes: 0,
            next_seq: 0,
            entries: HashMap::new(),
            hits: 0,
            misses: 0,
            evictions: 0,
        };

        if max_bytes == 0 {
            return Ok(cache);
        }

        fs::create_dir_all(&cache.dir)?;
        cache.scan_existing()?;
        cache.evict_until_within(cache.max_bytes)?;
        Ok(cache)
    }

    /// Convenience constructor accepting a capacity in mebibytes (`0` disables caching).
    ///
    /// # Errors
    /// Returns [`PlayerError::Io`] if directory creation or inspection fails.
    pub fn from_mib(dir: impl Into<PathBuf>, max_mib: u32) -> Result<Self, PlayerError> {
        let max_bytes = u64::from(max_mib).saturating_mul(BYTES_PER_MIB);
        Self::open(dir, max_bytes)
    }

    /// Returns `true` when disk caching is enabled (`max_bytes > 0`).
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.max_bytes > 0
    }

    /// Returns the configured cache directory path.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the maximum capacity in bytes (`0` when disabled).
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Returns the exact number of cached bytes currently tracked on disk.
    #[must_use]
    pub const fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    /// Returns the number of cached chunk entries.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns a snapshot of cache usage and hit/miss/eviction statistics.
    #[must_use]
    pub fn stats(&self) -> StreamCacheStats {
        StreamCacheStats {
            entry_count: self.entries.len(),
            current_bytes: self.current_bytes,
            max_bytes: self.max_bytes,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }

    /// Returns `true` if `key` is currently indexed in the cache.
    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        if !self.is_enabled() {
            return false;
        }
        let stem = key_to_stem(key);
        self.entries.contains_key(&stem)
    }

    /// Reads a cached chunk by `key` and refreshes its LRU recency on hit.
    ///
    /// Returns `Ok(None)` when caching is disabled or the entry is absent.
    ///
    /// # Errors
    /// Returns [`PlayerError::Io`] if reading an existing chunk fails unexpectedly.
    pub fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>, PlayerError> {
        if !self.is_enabled() {
            self.misses = self.misses.saturating_add(1);
            return Ok(None);
        }

        let stem = key_to_stem(key);
        let Some(file_name) = self.entries.get(&stem).map(|e| e.file_name.clone()) else {
            self.misses = self.misses.saturating_add(1);
            return Ok(None);
        };

        let path = self.dir.join(file_name);
        match fs::read(&path) {
            Ok(bytes) => {
                let seq = self.bump_seq();
                if let Some(entry) = self.entries.get_mut(&stem) {
                    entry.access_seq = seq;
                }
                self.hits = self.hits.saturating_add(1);
                Ok(Some(bytes))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                if let Some(removed) = self.entries.remove(&stem) {
                    self.current_bytes = self.current_bytes.saturating_sub(removed.size_bytes);
                }
                self.misses = self.misses.saturating_add(1);
                Ok(None)
            }
            Err(err) => Err(PlayerError::Io(err)),
        }
    }

    /// Stores an opaque/encrypted audio chunk under `key`, evicting oldest LRU entries as needed.
    ///
    /// Returns `Ok(true)` if the chunk was written, or `Ok(false)` if caching is disabled
    /// or `data.len()` exceeds `max_bytes`.
    ///
    /// # Errors
    /// Returns [`PlayerError::Io`] if writing or evicting chunk files fails.
    pub fn put(&mut self, key: &str, data: &[u8]) -> Result<bool, PlayerError> {
        if !self.is_enabled() {
            return Ok(false);
        }

        let size = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if size > self.max_bytes {
            return Ok(false);
        }

        let stem = key_to_stem(key);
        if let Some(existing) = self.entries.remove(&stem) {
            self.current_bytes = self.current_bytes.saturating_sub(existing.size_bytes);
        }

        let target_limit = self.max_bytes.saturating_sub(size);
        self.evict_until_within(target_limit)?;

        let file_name = format!("{stem}{CHUNK_EXTENSION}");
        let final_path = self.dir.join(&file_name);
        let seq = self.bump_seq();
        let tmp_path = self.dir.join(format!("{stem}.{seq}.tmp"));

        if let Err(err) = fs::write(&tmp_path, data) {
            let _ = fs::remove_file(&tmp_path);
            return Err(PlayerError::Io(err));
        }
        if let Err(err) = fs::rename(&tmp_path, &final_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(PlayerError::Io(err));
        }

        self.entries.insert(
            stem,
            CacheEntry {
                file_name,
                size_bytes: size,
                access_seq: seq,
            },
        );
        self.current_bytes = self.current_bytes.saturating_add(size);
        Ok(true)
    }

    /// Removes a single entry by `key` from disk and the in-memory index.
    ///
    /// Returns `Ok(true)` if an entry existed and was removed.
    ///
    /// # Errors
    /// Returns [`PlayerError::Io`] if deleting the file fails for reasons other than `NotFound`.
    pub fn remove(&mut self, key: &str) -> Result<bool, PlayerError> {
        if !self.is_enabled() {
            return Ok(false);
        }
        let stem = key_to_stem(key);
        let Some(entry) = self.entries.remove(&stem) else {
            return Ok(false);
        };
        self.current_bytes = self.current_bytes.saturating_sub(entry.size_bytes);
        remove_if_exists(&self.dir.join(entry.file_name))?;
        Ok(true)
    }

    /// Removes all cached chunks from disk and resets byte accounting.
    ///
    /// # Errors
    /// Returns [`PlayerError::Io`] if removing any chunk file fails.
    pub fn clear(&mut self) -> Result<(), PlayerError> {
        if !self.is_enabled() {
            return Ok(());
        }
        let entries: Vec<CacheEntry> = self.entries.drain().map(|(_, entry)| entry).collect();
        self.current_bytes = 0;
        for entry in entries {
            remove_if_exists(&self.dir.join(entry.file_name))?;
        }
        Ok(())
    }

    fn bump_seq(&mut self) -> u64 {
        self.next_seq = self.next_seq.saturating_add(1);
        self.next_seq
    }

    fn scan_existing(&mut self) -> Result<(), PlayerError> {
        let mut discovered: Vec<(SystemTime, String, String, u64)> = Vec::new();
        for dir_entry in fs::read_dir(&self.dir)? {
            let dir_entry = dir_entry?;
            let file_name_os = dir_entry.file_name();
            let Some(file_name) = file_name_os.to_str() else {
                continue;
            };
            let Some(stem) = file_name.strip_suffix(CHUNK_EXTENSION) else {
                continue;
            };
            let metadata = dir_entry.metadata()?;
            if !metadata.is_file() {
                continue;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            discovered.push((
                modified,
                stem.to_owned(),
                file_name.to_owned(),
                metadata.len(),
            ));
        }

        discovered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        for (_, stem, file_name, size_bytes) in discovered {
            let seq = self.bump_seq();
            self.current_bytes = self.current_bytes.saturating_add(size_bytes);
            self.entries.insert(
                stem,
                CacheEntry {
                    file_name,
                    size_bytes,
                    access_seq: seq,
                },
            );
        }
        Ok(())
    }

    fn evict_until_within(&mut self, limit_bytes: u64) -> Result<(), PlayerError> {
        while self.current_bytes > limit_bytes {
            let oldest_key = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.access_seq)
                .map(|(k, _)| k.clone());

            let Some(victim_key) = oldest_key else {
                self.current_bytes = 0;
                break;
            };

            if let Some(victim) = self.entries.remove(&victim_key) {
                self.current_bytes = self.current_bytes.saturating_sub(victim.size_bytes);
                self.evictions = self.evictions.saturating_add(1);
                remove_if_exists(&self.dir.join(victim.file_name))?;
            }
        }
        Ok(())
    }
}

fn remove_if_exists(path: &Path) -> Result<(), PlayerError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(PlayerError::Io(err)),
    }
}

/// Computes a deterministic 64-character lowercase hexadecimal SHA-256 digest
/// so arbitrary chunk identifiers map safely to flat filenames without path traversal.
fn key_to_stem(key: &str) -> String {
    let digest = sha256(key.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// Minimal pure-Rust FIPS 180-4 SHA-256 implementation for deterministic cache keys.
fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = Vec::with_capacity(data.len() + 72);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    let (blocks, _) = padded.as_chunks::<64>();
    for block in blocks {
        let mut schedule = [0_u32; 64];
        let (words, _) = block.as_chunks::<4>();
        for (idx, word_bytes) in words.iter().enumerate() {
            schedule[idx] = u32::from_be_bytes(*word_bytes);
        }
        for idx in 16..64 {
            let s0 = schedule[idx - 15].rotate_right(7)
                ^ schedule[idx - 15].rotate_right(18)
                ^ (schedule[idx - 15] >> 3);
            let s1 = schedule[idx - 2].rotate_right(17)
                ^ schedule[idx - 2].rotate_right(19)
                ^ (schedule[idx - 2] >> 10);
            schedule[idx] = schedule[idx - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[idx - 7])
                .wrapping_add(s1);
        }

        let [
            mut va,
            mut vb,
            mut vc,
            mut vd,
            mut ve,
            mut vf,
            mut vg,
            mut vh,
        ] = state;
        for idx in 0..64 {
            let s1 = ve.rotate_right(6) ^ ve.rotate_right(11) ^ ve.rotate_right(25);
            let ch = (ve & vf) ^ ((!ve) & vg);
            let temp1 = vh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[idx])
                .wrapping_add(schedule[idx]);
            let s0 = va.rotate_right(2) ^ va.rotate_right(13) ^ va.rotate_right(22);
            let maj = (va & vb) ^ (va & vc) ^ (vb & vc);
            let temp2 = s0.wrapping_add(maj);

            vh = vg;
            vg = vf;
            vf = ve;
            ve = vd.wrapping_add(temp1);
            vd = vc;
            vc = vb;
            vb = va;
            va = temp1.wrapping_add(temp2);
        }

        state[0] = state[0].wrapping_add(va);
        state[1] = state[1].wrapping_add(vb);
        state[2] = state[2].wrapping_add(vc);
        state[3] = state[3].wrapping_add(vd);
        state[4] = state[4].wrapping_add(ve);
        state[5] = state[5].wrapping_add(vf);
        state[6] = state[6].wrapping_add(vg);
        state[7] = state[7].wrapping_add(vh);
    }

    let mut out = [0_u8; 32];
    for (idx, word) in state.into_iter().enumerate() {
        let bytes = word.to_be_bytes();
        out[idx * 4..idx * 4 + 4].copy_from_slice(&bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{DEFAULT_MAX_CACHE_BYTES, StreamCache, default_cache_dir, key_to_stem};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_test_dir(label: &str) -> PathBuf {
        let count = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        std::env::temp_dir().join(format!(
            "vynl_stream_cache_{label}_{}_{count}_{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn disabled_zero_byte_cache_performs_no_disk_writes() -> Result<(), String> {
        let dir = unique_test_dir("disabled");
        let mut cache = StreamCache::open(&dir, 0).map_err(|e| e.to_string())?;
        assert!(!cache.is_enabled());
        assert_eq!(cache.max_bytes(), 0);
        assert_eq!(cache.current_bytes(), 0);
        assert_eq!(cache.entry_count(), 0);
        assert!(!dir.exists());

        let stored = cache
            .put("track:0", &[1, 2, 3, 4])
            .map_err(|e| e.to_string())?;
        assert!(!stored);
        assert!(!cache.contains("track:0"));
        assert_eq!(cache.get("track:0").map_err(|e| e.to_string())?, None);
        assert!(!dir.exists());
        Ok(())
    }

    #[test]
    fn lru_eviction_and_touch_on_read_preserve_hot_chunks() -> Result<(), String> {
        let dir = unique_test_dir("lru");
        let mut cache = StreamCache::open(&dir, 10).map_err(|e| e.to_string())?;

        assert!(
            cache
                .put("chunk_a", &[1, 2, 3, 4])
                .map_err(|e| e.to_string())?
        );
        assert!(
            cache
                .put("chunk_b", &[5, 6, 7, 8])
                .map_err(|e| e.to_string())?
        );
        assert_eq!(cache.current_bytes(), 8);
        assert_eq!(cache.entry_count(), 2);

        // Touch chunk_a so chunk_b becomes the least-recently-used entry.
        let read_a = cache.get("chunk_a").map_err(|e| e.to_string())?;
        assert_eq!(read_a, Some(vec![1, 2, 3, 4]));

        // Insert chunk_c (4 bytes): total would be 12 > 10, so chunk_b must be evicted.
        assert!(
            cache
                .put("chunk_c", &[9, 10, 11, 12])
                .map_err(|e| e.to_string())?
        );
        assert_eq!(cache.current_bytes(), 8);
        assert_eq!(cache.entry_count(), 2);
        assert!(cache.contains("chunk_a"));
        assert!(!cache.contains("chunk_b"));
        assert!(cache.contains("chunk_c"));
        assert_eq!(cache.stats().evictions, 1);
        assert_eq!(cache.stats().hits, 1);

        // Oversized chunk (> max_bytes) is rejected without disturbing existing entries.
        let oversized = cache.put("huge", &[0_u8; 16]).map_err(|e| e.to_string())?;
        assert!(!oversized);
        assert_eq!(cache.current_bytes(), 8);
        assert_eq!(cache.entry_count(), 2);

        cache.clear().map_err(|e| e.to_string())?;
        assert_eq!(cache.current_bytes(), 0);
        assert_eq!(cache.entry_count(), 0);

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn overwrite_and_reopen_keep_exact_byte_accounting() -> Result<(), String> {
        let dir = unique_test_dir("reopen");
        {
            let mut cache = StreamCache::open(&dir, 20).map_err(|e| e.to_string())?;
            assert!(cache.put("alpha", &[1; 6]).map_err(|e| e.to_string())?);
            assert!(cache.put("beta", &[2; 6]).map_err(|e| e.to_string())?);
            // Overwrite alpha with a 10-byte chunk.
            assert!(cache.put("alpha", &[3; 10]).map_err(|e| e.to_string())?);
            assert_eq!(cache.current_bytes(), 16);
            assert_eq!(cache.entry_count(), 2);
        }

        // Reopen with a tighter 10-byte cap: must evict until <= 10 bytes.
        let mut reopened = StreamCache::open(&dir, 10).map_err(|e| e.to_string())?;
        assert!(reopened.current_bytes() <= 10);
        assert_eq!(reopened.entry_count(), 1);
        assert_eq!(reopened.stats().evictions, 1);

        let removed = reopened.remove("alpha").map_err(|e| e.to_string())?
            || reopened.remove("beta").map_err(|e| e.to_string())?;
        assert!(removed);
        assert_eq!(reopened.current_bytes(), 0);

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn defaults_and_sha256_key_hashing_are_valid() {
        assert_eq!(DEFAULT_MAX_CACHE_BYTES, 256 * 1024 * 1024);
        assert!(default_cache_dir().ends_with("audio"));
        // Known FIPS 180-4 SHA-256 test vector for "abc".
        assert_eq!(
            key_to_stem("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
