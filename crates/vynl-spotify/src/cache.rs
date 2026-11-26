//! Atomic local metadata cache with `FNV-1a` integrity verification and a clean
//! [`MetadataStore`] trait seam ready for a future `redb` embedded database backend.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs::{self, File};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use vynl_core::model::{
    AlbumSummary, ArtistSummary, EpisodeSummary, ImageRef, LibraryState, PlaylistSummary,
    TrackSummary, UserProfile,
};

use crate::json::{JsonValue, parse_json, to_json_string};

/// Current on-disk metadata cache schema version.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_64_PRIME: u64 = 0x0100_0000_01b3;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Computes the 64-bit `FNV-1a` integrity hash of `bytes`.
#[must_use]
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A_64_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
    }
    hash
}

/// Computes the lowercase 16-digit hexadecimal `FNV-1a` checksum of `bytes`.
#[must_use]
pub fn fnv1a_64_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(16);
    let _ = write!(out, "{:016x}", fnv1a_64(bytes));
    out
}

/// Errors returned by [`MetadataStore`] and [`MetadataCache`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// Filesystem read, write, sync, or atomic rename failure.
    Io(String),
    /// Malformed or incompatible JSON schema.
    Parse(String),
    /// On-disk cache failed `FNV-1a` integrity verification (corruption or tampering).
    ChecksumMismatch {
        /// Checksum recorded in the cache envelope.
        expected: String,
        /// Recomputed checksum of the payload.
        actual: String,
    },
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "metadata cache I/O error: {msg}"),
            Self::Parse(msg) => write!(f, "metadata cache parse error: {msg}"),
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "metadata cache checksum mismatch (expected {expected}, got {actual})"
            ),
        }
    }
}

impl std::error::Error for CacheError {}

/// Cached track snapshot for a single Spotify playlist keyed by `snapshot_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSnapshot {
    /// Spotify playlist ID.
    pub playlist_id: String,
    /// Spotify `snapshot_id` for conditional refresh validation.
    pub snapshot_id: String,
    /// Optional HTTP `ETag` returned by Spotify.
    pub etag: Option<String>,
    /// Ordered playlist tracks cached for instant cold-start rendering.
    pub tracks: Vec<TrackSummary>,
    /// UNIX epoch timestamp (seconds) when this snapshot was stored.
    pub updated_at_epoch_secs: u64,
}

/// Top-level persisted Spotify metadata snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedMetadata {
    /// Cache format schema version.
    pub schema_version: u32,
    /// UNIX epoch timestamp (seconds) when the snapshot was last saved.
    pub updated_at_epoch_secs: u64,
    /// Cached user profile, if authenticated.
    pub user: Option<UserProfile>,
    /// Cached library collections (Liked Songs, playlists, albums, artists, podcasts).
    pub library: LibraryState,
    /// Per-playlist cached track snapshots keyed by Spotify playlist ID.
    pub playlist_snapshots: BTreeMap<String, PlaylistSnapshot>,
    /// Cached recently played track history.
    pub recently_played: Vec<TrackSummary>,
}

impl Default for CachedMetadata {
    fn default() -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            updated_at_epoch_secs: 0,
            user: None,
            library: LibraryState::default(),
            playlist_snapshots: BTreeMap::new(),
            recently_played: Vec::new(),
        }
    }
}

/// Storage contract for persistent Spotify metadata caches (implemented by
/// [`MetadataCache`] and ready to be backed by `redb` with zero caller changes).
pub trait MetadataStore {
    /// Loads the full cached metadata snapshot, or `Ok(None)` if not yet initialized.
    ///
    /// # Errors
    /// Returns [`CacheError`] if reading, parsing, or checksum verification fails.
    fn load_metadata(&self) -> Result<Option<CachedMetadata>, CacheError>;

    /// Atomically persists the full [`CachedMetadata`] snapshot.
    ///
    /// # Errors
    /// Returns [`CacheError`] if atomic write or rename fails.
    fn save_metadata(&self, metadata: &CachedMetadata) -> Result<(), CacheError>;

    /// Loads a cached playlist track snapshot by `playlist_id`.
    ///
    /// # Errors
    /// Returns [`CacheError`] if the underlying cache cannot be read or verified.
    fn playlist_snapshot(&self, playlist_id: &str) -> Result<Option<PlaylistSnapshot>, CacheError>;

    /// Inserts or replaces a single [`PlaylistSnapshot`] atomically.
    ///
    /// # Errors
    /// Returns [`CacheError`] if reading or atomically updating the cache fails.
    fn upsert_playlist_snapshot(&self, snapshot: PlaylistSnapshot) -> Result<(), CacheError>;

    /// Removes the persisted cache store.
    ///
    /// # Errors
    /// Returns [`CacheError`] if removal fails for reasons other than file absence.
    fn clear(&self) -> Result<(), CacheError>;
}

/// Pure-Rust atomic JSON + `FNV-1a` checksum implementation of [`MetadataStore`].
#[derive(Debug, Clone)]
pub struct MetadataCache {
    path: PathBuf,
}

impl MetadataCache {
    /// Creates a new [`MetadataCache`] stored at `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the on-disk file path backing this cache.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads and verifies the persisted [`CachedMetadata`] snapshot.
    ///
    /// # Errors
    /// Returns [`CacheError`] on I/O, JSON syntax, schema, or `FNV-1a` checksum errors.
    pub fn load(&self) -> Result<Option<CachedMetadata>, CacheError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(CacheError::Io(format!("failed to read cache file: {err}"))),
        };

        let envelope = parse_json(&raw).map_err(CacheError::Parse)?;
        let expected = envelope
            .get_str("checksum_fnv1a")
            .ok_or_else(|| {
                CacheError::Parse(String::from("cache envelope missing `checksum_fnv1a`"))
            })?
            .to_owned();
        let payload = envelope
            .get("payload")
            .ok_or_else(|| CacheError::Parse(String::from("cache envelope missing `payload`")))?;

        let canonical_payload = to_json_string(payload);
        let actual = fnv1a_64_hex(canonical_payload.as_bytes());
        if actual != expected {
            return Err(CacheError::ChecksumMismatch { expected, actual });
        }

        decode_metadata(payload).map(Some)
    }

    /// Persists `metadata` atomically (`write` temp + `sync_all` + `rename`) with an
    /// `FNV-1a` integrity checksum envelope.
    ///
    /// # Errors
    /// Returns [`CacheError::Io`] if directory creation, write, sync, or rename fails.
    pub fn save(&self, metadata: &CachedMetadata) -> Result<(), CacheError> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|err| {
                CacheError::Io(format!("failed to create cache directory: {err}"))
            })?;
        }

        let payload = encode_metadata(metadata);
        let canonical_payload = to_json_string(&payload);
        let checksum = fnv1a_64_hex(canonical_payload.as_bytes());
        let envelope = JsonValue::object([
            ("checksum_fnv1a", JsonValue::from(checksum)),
            ("payload", payload),
        ]);
        let bytes = to_json_string(&envelope);

        let tmp_path = temp_path_for(&self.path);
        let write_res = (|| -> Result<(), CacheError> {
            let mut file = File::create(&tmp_path).map_err(|err| {
                CacheError::Io(format!("failed to create temp cache file: {err}"))
            })?;
            file.write_all(bytes.as_bytes())
                .map_err(|err| CacheError::Io(format!("failed to write temp cache file: {err}")))?;
            file.sync_all()
                .map_err(|err| CacheError::Io(format!("failed to sync temp cache file: {err}")))?;
            Ok(())
        })();

        if let Err(err) = write_res {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        if let Err(err) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(CacheError::Io(format!(
                "failed to atomically replace cache file: {err}"
            )));
        }

        Ok(())
    }
}

impl MetadataStore for MetadataCache {
    fn load_metadata(&self) -> Result<Option<CachedMetadata>, CacheError> {
        self.load()
    }

    fn save_metadata(&self, metadata: &CachedMetadata) -> Result<(), CacheError> {
        self.save(metadata)
    }

    fn playlist_snapshot(&self, playlist_id: &str) -> Result<Option<PlaylistSnapshot>, CacheError> {
        Ok(self
            .load()?
            .and_then(|mut meta| meta.playlist_snapshots.remove(playlist_id)))
    }

    fn upsert_playlist_snapshot(&self, snapshot: PlaylistSnapshot) -> Result<(), CacheError> {
        let mut meta = self.load()?.unwrap_or_default();
        meta.playlist_snapshots
            .insert(snapshot.playlist_id.clone(), snapshot);
        self.save(&meta)
    }

    fn clear(&self) -> Result<(), CacheError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(CacheError::Io(format!(
                "failed to delete cache file: {err}"
            ))),
        }
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("metadata.json");
    path.with_file_name(format!(".{name}.tmp.{pid}.{seq}"))
}

fn encode_image(img: Option<&ImageRef>) -> JsonValue {
    match img {
        Some(image) => JsonValue::object([
            ("url", JsonValue::from(image.url.as_str())),
            (
                "width",
                image.width.map_or(JsonValue::Null, JsonValue::from),
            ),
            (
                "height",
                image.height.map_or(JsonValue::Null, JsonValue::from),
            ),
        ]),
        None => JsonValue::Null,
    }
}

fn decode_image(value: Option<&JsonValue>) -> Option<ImageRef> {
    let img = value.filter(|v| !v.is_null())?;
    let url = img.get_str("url")?.to_owned();
    Some(ImageRef {
        url,
        width: img.get_u32("width"),
        height: img.get_u32("height"),
    })
}

fn encode_strings(items: &[String]) -> JsonValue {
    JsonValue::Array(items.iter().map(|s| JsonValue::from(s.as_str())).collect())
}

fn decode_strings(value: Option<&JsonValue>) -> Vec<String> {
    value
        .and_then(JsonValue::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn encode_user(user: &UserProfile) -> JsonValue {
    JsonValue::object([
        ("id", JsonValue::from(user.id.as_str())),
        ("display_name", JsonValue::from(user.display_name.as_str())),
        ("product", JsonValue::from(user.product.as_str())),
        ("avatar", encode_image(user.avatar.as_ref())),
    ])
}

fn decode_user(value: &JsonValue) -> Result<UserProfile, CacheError> {
    let id = value
        .get_str("id")
        .ok_or_else(|| CacheError::Parse(String::from("cached user missing `id`")))?
        .to_owned();
    let display_name = value.get_str("display_name").unwrap_or(&id).to_owned();
    let product = value.get_str("product").unwrap_or("free").to_owned();
    Ok(UserProfile {
        id,
        display_name,
        product,
        avatar: decode_image(value.get("avatar")),
    })
}

fn encode_track(track: &TrackSummary) -> JsonValue {
    JsonValue::object([
        ("id", JsonValue::from(track.id.as_str())),
        ("title", JsonValue::from(track.title.as_str())),
        ("artists", encode_strings(&track.artists)),
        ("album", JsonValue::from(track.album.as_str())),
        ("duration_ms", JsonValue::from(track.duration_ms)),
        ("explicit", JsonValue::Bool(track.explicit)),
        ("is_liked", JsonValue::Bool(track.is_liked)),
        ("cover", encode_image(track.cover.as_ref())),
    ])
}

fn decode_track(value: &JsonValue) -> Result<TrackSummary, CacheError> {
    Ok(TrackSummary {
        id: value
            .get_str("id")
            .ok_or_else(|| CacheError::Parse(String::from("cached track missing `id`")))?
            .to_owned(),
        title: value
            .get_str("title")
            .ok_or_else(|| CacheError::Parse(String::from("cached track missing `title`")))?
            .to_owned(),
        artists: decode_strings(value.get("artists")),
        album: value.get_str("album").unwrap_or("").to_owned(),
        duration_ms: value.get_u64("duration_ms").unwrap_or(0),
        explicit: value.get_bool("explicit").unwrap_or(false),
        is_liked: value.get_bool("is_liked").unwrap_or(false),
        cover: decode_image(value.get("cover")),
    })
}

fn encode_album(album: &AlbumSummary) -> JsonValue {
    JsonValue::object([
        ("id", JsonValue::from(album.id.as_str())),
        ("title", JsonValue::from(album.title.as_str())),
        ("artists", encode_strings(&album.artists)),
        ("release_year", JsonValue::from(album.release_year.as_str())),
        ("album_type", JsonValue::from(album.album_type.as_str())),
        ("total_tracks", JsonValue::from(album.total_tracks)),
        ("cover", encode_image(album.cover.as_ref())),
    ])
}

fn decode_album(value: &JsonValue) -> Result<AlbumSummary, CacheError> {
    Ok(AlbumSummary {
        id: value
            .get_str("id")
            .ok_or_else(|| CacheError::Parse(String::from("cached album missing `id`")))?
            .to_owned(),
        title: value
            .get_str("title")
            .ok_or_else(|| CacheError::Parse(String::from("cached album missing `title`")))?
            .to_owned(),
        artists: decode_strings(value.get("artists")),
        release_year: value.get_str("release_year").unwrap_or("").to_owned(),
        album_type: value.get_str("album_type").unwrap_or("album").to_owned(),
        total_tracks: value.get_u32("total_tracks").unwrap_or(0),
        cover: decode_image(value.get("cover")),
    })
}

fn encode_artist(artist: &ArtistSummary) -> JsonValue {
    JsonValue::object([
        ("id", JsonValue::from(artist.id.as_str())),
        ("name", JsonValue::from(artist.name.as_str())),
        ("followers", JsonValue::from(artist.followers)),
        ("genres", encode_strings(&artist.genres)),
        ("following", JsonValue::Bool(artist.following)),
        ("avatar", encode_image(artist.avatar.as_ref())),
    ])
}

fn decode_artist(value: &JsonValue) -> Result<ArtistSummary, CacheError> {
    Ok(ArtistSummary {
        id: value
            .get_str("id")
            .ok_or_else(|| CacheError::Parse(String::from("cached artist missing `id`")))?
            .to_owned(),
        name: value
            .get_str("name")
            .ok_or_else(|| CacheError::Parse(String::from("cached artist missing `name`")))?
            .to_owned(),
        followers: value.get_u64("followers").unwrap_or(0),
        genres: decode_strings(value.get("genres")),
        following: value.get_bool("following").unwrap_or(false),
        avatar: decode_image(value.get("avatar")),
    })
}

fn encode_playlist(playlist: &PlaylistSummary) -> JsonValue {
    JsonValue::object([
        ("id", JsonValue::from(playlist.id.as_str())),
        ("name", JsonValue::from(playlist.name.as_str())),
        (
            "description",
            JsonValue::from(playlist.description.as_str()),
        ),
        ("owner", JsonValue::from(playlist.owner.as_str())),
        ("track_count", JsonValue::from(playlist.track_count)),
        (
            "snapshot_id",
            JsonValue::from(playlist.snapshot_id.as_str()),
        ),
        ("pinned", JsonValue::Bool(playlist.pinned)),
        (
            "folder",
            playlist
                .folder
                .as_deref()
                .map_or(JsonValue::Null, JsonValue::from),
        ),
        ("cover", encode_image(playlist.cover.as_ref())),
    ])
}

fn decode_playlist(value: &JsonValue) -> Result<PlaylistSummary, CacheError> {
    Ok(PlaylistSummary {
        id: value
            .get_str("id")
            .ok_or_else(|| CacheError::Parse(String::from("cached playlist missing `id`")))?
            .to_owned(),
        name: value
            .get_str("name")
            .ok_or_else(|| CacheError::Parse(String::from("cached playlist missing `name`")))?
            .to_owned(),
        description: value.get_str("description").unwrap_or("").to_owned(),
        owner: value.get_str("owner").unwrap_or("").to_owned(),
        track_count: value.get_u32("track_count").unwrap_or(0),
        snapshot_id: value.get_str("snapshot_id").unwrap_or("").to_owned(),
        pinned: value.get_bool("pinned").unwrap_or(false),
        folder: value.get_str("folder").map(ToOwned::to_owned),
        cover: decode_image(value.get("cover")),
    })
}

fn encode_episode(episode: &EpisodeSummary) -> JsonValue {
    JsonValue::object([
        ("id", JsonValue::from(episode.id.as_str())),
        ("title", JsonValue::from(episode.title.as_str())),
        ("show_name", JsonValue::from(episode.show_name.as_str())),
        ("duration_ms", JsonValue::from(episode.duration_ms)),
        (
            "resume_position_ms",
            JsonValue::from(episode.resume_position_ms),
        ),
        ("fully_played", JsonValue::Bool(episode.fully_played)),
        ("cover", encode_image(episode.cover.as_ref())),
    ])
}

fn decode_episode(value: &JsonValue) -> Result<EpisodeSummary, CacheError> {
    Ok(EpisodeSummary {
        id: value
            .get_str("id")
            .ok_or_else(|| CacheError::Parse(String::from("cached episode missing `id`")))?
            .to_owned(),
        title: value
            .get_str("title")
            .ok_or_else(|| CacheError::Parse(String::from("cached episode missing `title`")))?
            .to_owned(),
        show_name: value.get_str("show_name").unwrap_or("").to_owned(),
        duration_ms: value.get_u64("duration_ms").unwrap_or(0),
        resume_position_ms: value.get_u64("resume_position_ms").unwrap_or(0),
        fully_played: value.get_bool("fully_played").unwrap_or(false),
        cover: decode_image(value.get("cover")),
    })
}

fn encode_playlist_snapshot(snapshot: &PlaylistSnapshot) -> JsonValue {
    JsonValue::object([
        (
            "playlist_id",
            JsonValue::from(snapshot.playlist_id.as_str()),
        ),
        (
            "snapshot_id",
            JsonValue::from(snapshot.snapshot_id.as_str()),
        ),
        (
            "etag",
            snapshot
                .etag
                .as_deref()
                .map_or(JsonValue::Null, JsonValue::from),
        ),
        (
            "tracks",
            JsonValue::Array(snapshot.tracks.iter().map(encode_track).collect()),
        ),
        (
            "updated_at_epoch_secs",
            JsonValue::from(snapshot.updated_at_epoch_secs),
        ),
    ])
}

fn decode_playlist_snapshot(value: &JsonValue) -> Result<PlaylistSnapshot, CacheError> {
    let tracks = value
        .get_array("tracks")
        .unwrap_or(&[])
        .iter()
        .map(decode_track)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PlaylistSnapshot {
        playlist_id: value
            .get_str("playlist_id")
            .ok_or_else(|| {
                CacheError::Parse(String::from(
                    "cached playlist snapshot missing `playlist_id`",
                ))
            })?
            .to_owned(),
        snapshot_id: value.get_str("snapshot_id").unwrap_or("").to_owned(),
        etag: value.get_str("etag").map(ToOwned::to_owned),
        tracks,
        updated_at_epoch_secs: value.get_u64("updated_at_epoch_secs").unwrap_or(0),
    })
}

fn encode_metadata(meta: &CachedMetadata) -> JsonValue {
    let mut snapshots = BTreeMap::new();
    for (id, snap) in &meta.playlist_snapshots {
        snapshots.insert(id.clone(), encode_playlist_snapshot(snap));
    }

    let library = JsonValue::object([
        (
            "liked_tracks",
            JsonValue::Array(meta.library.liked_tracks.iter().map(encode_track).collect()),
        ),
        (
            "playlists",
            JsonValue::Array(meta.library.playlists.iter().map(encode_playlist).collect()),
        ),
        (
            "albums",
            JsonValue::Array(meta.library.albums.iter().map(encode_album).collect()),
        ),
        (
            "artists",
            JsonValue::Array(meta.library.artists.iter().map(encode_artist).collect()),
        ),
        (
            "episodes",
            JsonValue::Array(meta.library.episodes.iter().map(encode_episode).collect()),
        ),
    ]);

    JsonValue::object([
        ("schema_version", JsonValue::from(meta.schema_version)),
        (
            "updated_at_epoch_secs",
            JsonValue::from(meta.updated_at_epoch_secs),
        ),
        (
            "user",
            meta.user.as_ref().map_or(JsonValue::Null, encode_user),
        ),
        ("library", library),
        ("playlist_snapshots", JsonValue::Object(snapshots)),
        (
            "recently_played",
            JsonValue::Array(meta.recently_played.iter().map(encode_track).collect()),
        ),
    ])
}

fn decode_metadata(value: &JsonValue) -> Result<CachedMetadata, CacheError> {
    let schema_version = value
        .get_u32("schema_version")
        .ok_or_else(|| CacheError::Parse(String::from("cache missing `schema_version`")))?;
    if schema_version != CACHE_SCHEMA_VERSION {
        return Err(CacheError::Parse(format!(
            "unsupported cache schema version {schema_version} (expected {CACHE_SCHEMA_VERSION})"
        )));
    }

    let updated_at_epoch_secs = value.get_u64("updated_at_epoch_secs").unwrap_or(0);
    let user = match value.get("user") {
        Some(u) if !u.is_null() => Some(decode_user(u)?),
        _ => None,
    };

    let library = if let Some(lib) = value.get("library") {
        LibraryState {
            liked_tracks: lib
                .get_array("liked_tracks")
                .unwrap_or(&[])
                .iter()
                .map(decode_track)
                .collect::<Result<Vec<_>, _>>()?,
            playlists: lib
                .get_array("playlists")
                .unwrap_or(&[])
                .iter()
                .map(decode_playlist)
                .collect::<Result<Vec<_>, _>>()?,
            albums: lib
                .get_array("albums")
                .unwrap_or(&[])
                .iter()
                .map(decode_album)
                .collect::<Result<Vec<_>, _>>()?,
            artists: lib
                .get_array("artists")
                .unwrap_or(&[])
                .iter()
                .map(decode_artist)
                .collect::<Result<Vec<_>, _>>()?,
            episodes: lib
                .get_array("episodes")
                .unwrap_or(&[])
                .iter()
                .map(decode_episode)
                .collect::<Result<Vec<_>, _>>()?,
        }
    } else {
        LibraryState::default()
    };

    let mut playlist_snapshots = BTreeMap::new();
    if let Some(snaps) = value.get_object("playlist_snapshots") {
        for (id, val) in snaps {
            playlist_snapshots.insert(id.clone(), decode_playlist_snapshot(val)?);
        }
    }

    let recently_played = value
        .get_array("recently_played")
        .unwrap_or(&[])
        .iter()
        .map(decode_track)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CachedMetadata {
        schema_version,
        updated_at_epoch_secs,
        user,
        library,
        playlist_snapshots,
        recently_played,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::demo::DemoCatalog;

    use super::{
        CACHE_SCHEMA_VERSION, CacheError, CachedMetadata, MetadataCache, MetadataStore,
        PlaylistSnapshot, fnv1a_64,
    };

    #[test]
    fn fnv1a_64_matches_reference_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn metadata_cache_atomically_round_trips_and_detects_corruption() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("vynl-cache-test-{}", std::process::id()));
        let path = dir.join("metadata-cache.json");
        let cache = MetadataCache::new(&path);
        cache.clear().map_err(|e| e.to_string())?;

        assert_eq!(cache.load_metadata().map_err(|e| e.to_string())?, None);

        let demo = DemoCatalog::builtin();
        let mut snapshots = BTreeMap::new();
        snapshots.insert(
            String::from("pl_late_night_coding"),
            PlaylistSnapshot {
                playlist_id: String::from("pl_late_night_coding"),
                snapshot_id: String::from("demo-snap-001"),
                etag: Some(String::from("\"demo-snap-001\"")),
                tracks: demo.tracks_for_playlist("pl_late_night_coding"),
                updated_at_epoch_secs: 1_800_000_000,
            },
        );

        let meta = CachedMetadata {
            schema_version: CACHE_SCHEMA_VERSION,
            updated_at_epoch_secs: 1_800_000_000,
            user: Some(demo.user.clone()),
            library: demo.to_library_state(),
            playlist_snapshots: snapshots,
            recently_played: demo.recently_played.clone(),
        };

        cache.save_metadata(&meta).map_err(|e| e.to_string())?;
        let loaded = cache.load_metadata().map_err(|e| e.to_string())?;
        assert_eq!(loaded, Some(meta));

        let snap = cache
            .playlist_snapshot("pl_late_night_coding")
            .map_err(|e| e.to_string())?;
        assert_eq!(
            snap.as_ref().map(|s| s.snapshot_id.as_str()),
            Some("demo-snap-001")
        );

        // Corrupt a byte in the persisted payload and verify FNV-1a detects it
        let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let tampered = raw.replacen("Vinyl Explorer", "Tampered User!", 1);
        std::fs::write(&path, tampered).map_err(|e| e.to_string())?;
        let corrupted = cache.load_metadata();
        assert!(matches!(
            corrupted,
            Err(CacheError::ChecksumMismatch { .. })
        ));

        cache.clear().map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
