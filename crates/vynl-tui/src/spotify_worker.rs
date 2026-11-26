//! Background [`SpotifyApiClient`] command dispatcher, [`RedbMetadataCache`] hydration,
//! automatic OAuth token refresh, and adaptive remote polling worker.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use vynl_core::model::{
    ConnectDevice, LibraryState, PlaybackState, RepeatMode, SearchResults, ToastLevel,
    TrackSummary, UserProfile,
};
use vynl_core::state::AppCommand;
use vynl_spotify::api::{
    ApiError, HttpTransport, PlaylistTracksOutcome, ReqwestHttpTransport, SpotifyApiClient,
};
use vynl_spotify::cache::{CachedMetadata, MetadataStore, PlaylistSnapshot, RedbMetadataCache};
use vynl_spotify::pkce::{TokenBundle, current_epoch_secs};
use vynl_system::paths::AppPaths;

use crate::storage;

/// Token refresh leeway before access token expiry (seconds).
const TOKEN_REFRESH_LEEWAY_SECS: u64 = 60;

/// Request sent from the UI thread to the background Spotify worker thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendRequest {
    /// Load cached metadata from `redb`, then hydrate live profile, library, devices,
    /// recently played, and playback state from the Spotify Web API.
    Bootstrap,
    /// Dispatch a reducer-emitted [`AppCommand`] to the Spotify Web API.
    Command(AppCommand),
    /// Fetch tracks for a playlist using `snapshot_id` / `ETag` cache validation.
    FetchPlaylistTracks {
        /// Spotify playlist ID.
        playlist_id: String,
        /// Last known snapshot ID (for `If-None-Match` short-circuiting).
        cached_snapshot_id: Option<String>,
    },
    /// Fetch tracks belonging to a specific Spotify album.
    FetchAlbumTracks {
        /// Spotify album ID.
        album_id: String,
    },
    /// Fetch top tracks for a specific Spotify artist.
    FetchArtistTopTracks {
        /// Spotify artist ID.
        artist_id: String,
    },
    /// Poll active playback state and Connect devices (adaptive remote sync).
    PollPlayback,
    /// Gracefully terminate the worker thread.
    Shutdown,
}

/// Update emitted asynchronously from the Spotify worker back to the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendUpdate {
    /// Hydrated user profile, library, recently played list, and cached playlist tracks.
    MetadataHydrated {
        /// Authenticated Spotify user profile.
        user: Option<UserProfile>,
        /// Liked tracks, playlists, albums, artists, and episodes.
        library: LibraryState,
        /// Recently played tracks.
        recently_played: Vec<TrackSummary>,
        /// Tracks keyed by playlist ID from cache or API.
        playlist_tracks: BTreeMap<String, Vec<TrackSummary>>,
        /// Whether this hydration came from the local `redb` cache (before live refresh).
        from_cache: bool,
    },
    /// Fresh playback state snapshot from Spotify Connect.
    PlaybackSynced(Option<PlaybackState>),
    /// Fresh list of available Spotify Connect devices.
    DevicesLoaded(Vec<ConnectDevice>),
    /// Tracks loaded for a specific playlist.
    PlaylistTracksLoaded {
        /// Spotify playlist ID.
        playlist_id: String,
        /// Snapshot ID returned by Spotify.
        snapshot_id: String,
        /// Ordered track summaries.
        tracks: Vec<TrackSummary>,
    },
    /// Tracks loaded for a specific album.
    AlbumTracksLoaded {
        /// Spotify album ID.
        album_id: String,
        /// Ordered track summaries.
        tracks: Vec<TrackSummary>,
    },
    /// Top tracks loaded for a specific artist.
    ArtistTracksLoaded {
        /// Spotify artist ID.
        artist_id: String,
        /// Ordered track summaries.
        tracks: Vec<TrackSummary>,
    },
    /// Remote search results returned by `GET /search`.
    SearchResultsLoaded(SearchResults),
    /// Rate limit signal (`429 Retry-After`) for adaptive polling backoff.
    RateLimited(Duration),
    /// User-facing status notification toast.
    Toast {
        /// Severity level.
        level: ToastLevel,
        /// Human-readable English message.
        text: String,
    },
}

/// Non-blocking handle to the background Spotify Web API worker thread.
pub struct SpotifyWorkerHandle {
    req_tx: mpsc::Sender<BackendRequest>,
    upd_rx: mpsc::Receiver<BackendUpdate>,
    worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SpotifyWorkerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpotifyWorkerHandle")
            .field("active", &self.worker.is_some())
            .finish_non_exhaustive()
    }
}

impl SpotifyWorkerHandle {
    /// Spawns a live background Spotify worker backed by [`ReqwestHttpTransport`]
    /// (`rustls-tls`), [`RedbMetadataCache`] at `paths.metadata_cache_file()`, and
    /// OS-keyring token refresh persistence.
    ///
    /// # Errors
    /// Returns an error if the HTTPS client or worker thread cannot be initialized.
    pub fn spawn_live(
        paths: &AppPaths,
        client_id: String,
        tokens: TokenBundle,
    ) -> Result<Self, String> {
        let transport = ReqwestHttpTransport::new()
            .map_err(|err| format!("failed to initialize Spotify HTTPS transport: {err}"))?;
        let cache = RedbMetadataCache::open(paths.metadata_cache_file()).ok();
        let save_paths = paths.clone();
        let persist_tokens: Box<dyn Fn(&TokenBundle) + Send> = Box::new(move |bundle| {
            let _ = storage::save_tokens(&save_paths, bundle);
        });

        Self::spawn_with_transport(transport, client_id, tokens, cache, persist_tokens)
    }

    /// Spawns a background Spotify worker using a caller-supplied [`HttpTransport`]
    /// (enabling deterministic tests with [`vynl_spotify::api::MockSpotifyTransport`]).
    ///
    /// # Errors
    /// Returns an error if the background thread cannot be spawned.
    pub fn spawn_with_transport<T>(
        transport: T,
        client_id: String,
        tokens: TokenBundle,
        cache: Option<RedbMetadataCache>,
        on_tokens_refreshed: Box<dyn Fn(&TokenBundle) + Send>,
    ) -> Result<Self, String>
    where
        T: HttpTransport + Send + 'static,
    {
        let (req_tx, req_rx) = mpsc::channel::<BackendRequest>();
        let (upd_tx, upd_rx) = mpsc::channel::<BackendUpdate>();

        let handle = thread::Builder::new()
            .name(String::from("vynl-spotify-worker"))
            .spawn(move || {
                let mut ctx = WorkerContext {
                    client: SpotifyApiClient::new(transport, tokens.access_token.clone()),
                    client_id,
                    tokens,
                    cache,
                    on_tokens_refreshed,
                    upd_tx,
                };
                ctx.run(&req_rx);
            })
            .map_err(|err| format!("failed to spawn Spotify worker thread: {err}"))?;

        Ok(Self {
            req_tx,
            upd_rx,
            worker: Some(handle),
        })
    }

    /// Enqueues a [`BackendRequest`] without blocking the UI thread.
    pub fn send(&self, request: BackendRequest) {
        let _ = self.req_tx.send(request);
    }

    /// Receives the next completed [`BackendUpdate`], if any.
    #[must_use]
    pub fn try_recv(&self) -> Option<BackendUpdate> {
        self.upd_rx.try_recv().ok()
    }

    /// Drains all currently buffered [`BackendUpdate`] messages.
    #[must_use]
    pub fn drain_updates(&self) -> Vec<BackendUpdate> {
        let mut updates = Vec::new();
        while let Ok(upd) = self.upd_rx.try_recv() {
            updates.push(upd);
        }
        updates
    }
}

impl Drop for SpotifyWorkerHandle {
    fn drop(&mut self) {
        let _ = self.req_tx.send(BackendRequest::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

struct WorkerContext<T: HttpTransport> {
    client: SpotifyApiClient<T>,
    client_id: String,
    tokens: TokenBundle,
    cache: Option<RedbMetadataCache>,
    on_tokens_refreshed: Box<dyn Fn(&TokenBundle) + Send>,
    upd_tx: mpsc::Sender<BackendUpdate>,
}

impl<T: HttpTransport> WorkerContext<T> {
    fn run(&mut self, req_rx: &mpsc::Receiver<BackendRequest>) {
        while let Ok(req) = req_rx.recv() {
            match req {
                BackendRequest::Shutdown => break,
                BackendRequest::Bootstrap => self.handle_bootstrap(),
                BackendRequest::Command(cmd) => self.handle_command(cmd),
                BackendRequest::FetchPlaylistTracks {
                    playlist_id,
                    cached_snapshot_id,
                } => self.handle_fetch_playlist_tracks(&playlist_id, cached_snapshot_id.as_deref()),
                BackendRequest::FetchAlbumTracks { album_id } => {
                    self.handle_fetch_album_tracks(&album_id);
                }
                BackendRequest::FetchArtistTopTracks { artist_id } => {
                    self.handle_fetch_artist_top_tracks(&artist_id);
                }
                BackendRequest::PollPlayback => self.handle_poll_playback(),
            }
        }
    }

    fn ensure_fresh_token(&mut self) {
        let now = current_epoch_secs();
        if self
            .tokens
            .is_expired_with_leeway(now, TOKEN_REFRESH_LEEWAY_SECS)
        {
            let _ = self.try_refresh_token();
        }
    }

    fn try_refresh_token(&mut self) -> bool {
        let refresh_token = self.tokens.refresh_token.clone();
        if refresh_token.is_empty() {
            return false;
        }
        let now = current_epoch_secs();
        match self
            .client
            .refresh_access_token(&self.client_id, &refresh_token, now)
        {
            Ok(updated) => {
                self.client.set_access_token(updated.access_token.clone());
                self.tokens = updated;
                (self.on_tokens_refreshed)(&self.tokens);
                true
            }
            Err(_) => false,
        }
    }

    fn call_with_refresh<R, F>(&mut self, mut op: F) -> Result<R, ApiError>
    where
        F: FnMut(&SpotifyApiClient<T>) -> Result<R, ApiError>,
    {
        self.ensure_fresh_token();
        match op(&self.client) {
            Err(ApiError::Unauthorized) if self.try_refresh_token() => op(&self.client),
            other => other,
        }
    }

    #[expect(clippy::too_many_lines)]
    fn handle_bootstrap(&mut self) {
        // 1. Emit cached metadata immediately from redb for instant startup.
        if let Some(ref cache) = self.cache
            && let Ok(Some(cached)) = cache.load_metadata()
        {
            let playlist_tracks = cached
                .playlist_snapshots
                .iter()
                .map(|(id, snap)| (id.clone(), snap.tracks.clone()))
                .collect();
            let _ = self.upd_tx.send(BackendUpdate::MetadataHydrated {
                user: cached.user,
                library: cached.library,
                recently_played: cached.recently_played,
                playlist_tracks,
                from_cache: true,
            });
        }

        // 2. Hydrate live data from Spotify Web API.
        let user = match self.call_with_refresh(SpotifyApiClient::me) {
            Ok(u) => Some(u),
            Err(err) => {
                self.report_error("profile", &err);
                None
            }
        };

        let liked_tracks = match self.call_with_refresh(SpotifyApiClient::liked_tracks) {
            Ok(tracks) => tracks,
            Err(err) => {
                self.report_error("liked songs", &err);
                Vec::new()
            }
        };

        let playlists = match self.call_with_refresh(SpotifyApiClient::playlists) {
            Ok(pls) => pls,
            Err(err) => {
                self.report_error("playlists", &err);
                Vec::new()
            }
        };

        let albums = match self.call_with_refresh(SpotifyApiClient::saved_albums) {
            Ok(albums) => albums,
            Err(err) => {
                self.report_error("saved albums", &err);
                Vec::new()
            }
        };

        let artists = match self.call_with_refresh(SpotifyApiClient::followed_artists) {
            Ok(artists) => artists,
            Err(err) => {
                self.report_error("followed artists", &err);
                Vec::new()
            }
        };

        let episodes = match self.call_with_refresh(SpotifyApiClient::saved_episodes) {
            Ok(episodes) => episodes,
            Err(err) => {
                self.report_error("saved episodes", &err);
                Vec::new()
            }
        };

        let recently_played = match self.call_with_refresh(SpotifyApiClient::recently_played) {
            Ok(recent) => recent,
            Err(err) => {
                self.report_error("recently played", &err);
                Vec::new()
            }
        };

        let mut playlist_snapshots = BTreeMap::new();
        let mut playlist_tracks = BTreeMap::new();
        if let Some(ref cache) = self.cache
            && let Ok(Some(prev)) = cache.load_metadata()
        {
            for (id, snap) in prev.playlist_snapshots {
                playlist_tracks.insert(id.clone(), snap.tracks.clone());
                playlist_snapshots.insert(id, snap);
            }
        }

        let library = LibraryState {
            liked_tracks,
            playlists,
            albums,
            artists,
            episodes,
        };

        let metadata = CachedMetadata {
            updated_at_epoch_secs: current_epoch_secs(),
            user: user.clone(),
            library: library.clone(),
            playlist_snapshots,
            recently_played: recently_played.clone(),
            ..CachedMetadata::default()
        };
        if let Some(ref cache) = self.cache {
            let _ = cache.save_metadata(&metadata);
        }

        let _ = self.upd_tx.send(BackendUpdate::MetadataHydrated {
            user,
            library,
            recently_played,
            playlist_tracks,
            from_cache: false,
        });

        self.handle_poll_playback();
    }

    fn handle_poll_playback(&mut self) {
        match self.call_with_refresh(SpotifyApiClient::playback_state) {
            Ok(playback) => {
                let _ = self.upd_tx.send(BackendUpdate::PlaybackSynced(playback));
            }
            Err(err) => self.report_error("playback sync", &err),
        }

        match self.call_with_refresh(SpotifyApiClient::devices) {
            Ok(devices) => {
                let _ = self.upd_tx.send(BackendUpdate::DevicesLoaded(devices));
            }
            Err(err) => self.report_error("devices", &err),
        }
    }

    fn handle_fetch_playlist_tracks(
        &mut self,
        playlist_id: &str,
        cached_snapshot_id: Option<&str>,
    ) {
        let existing_snapshot = self
            .cache
            .as_ref()
            .and_then(|c| c.playlist_snapshot(playlist_id).ok().flatten());
        let effective_snapshot = cached_snapshot_id
            .map(str::to_owned)
            .or_else(|| existing_snapshot.as_ref().map(|s| s.snapshot_id.clone()));

        match self.call_with_refresh(|client| {
            client.playlist_tracks(playlist_id, effective_snapshot.as_deref())
        }) {
            Ok(PlaylistTracksOutcome::NotModified { snapshot_id }) => {
                if let Some(snap) = existing_snapshot {
                    let _ = self.upd_tx.send(BackendUpdate::PlaylistTracksLoaded {
                        playlist_id: playlist_id.to_owned(),
                        snapshot_id,
                        tracks: snap.tracks,
                    });
                }
            }
            Ok(PlaylistTracksOutcome::Updated {
                snapshot_id,
                etag,
                tracks,
            }) => {
                if let Some(ref cache) = self.cache {
                    let _ = cache.upsert_playlist_snapshot(PlaylistSnapshot {
                        playlist_id: playlist_id.to_owned(),
                        snapshot_id: snapshot_id.clone(),
                        etag,
                        tracks: tracks.clone(),
                        updated_at_epoch_secs: current_epoch_secs(),
                    });
                }
                let _ = self.upd_tx.send(BackendUpdate::PlaylistTracksLoaded {
                    playlist_id: playlist_id.to_owned(),
                    snapshot_id,
                    tracks,
                });
            }
            Err(err) => self.report_error("playlist tracks", &err),
        }
    }

    fn handle_fetch_album_tracks(&mut self, album_id: &str) {
        match self.call_with_refresh(|client| client.album_tracks(album_id)) {
            Ok(tracks) => {
                let _ = self.upd_tx.send(BackendUpdate::AlbumTracksLoaded {
                    album_id: album_id.to_owned(),
                    tracks,
                });
            }
            Err(err) => self.report_error("album tracks", &err),
        }
    }

    fn handle_fetch_artist_top_tracks(&mut self, artist_id: &str) {
        match self.call_with_refresh(|client| client.artist_top_tracks(artist_id)) {
            Ok(tracks) => {
                let _ = self.upd_tx.send(BackendUpdate::ArtistTracksLoaded {
                    artist_id: artist_id.to_owned(),
                    tracks,
                });
            }
            Err(err) => self.report_error("artist top tracks", &err),
        }
    }

    fn handle_command(&mut self, cmd: AppCommand) {
        match cmd {
            AppCommand::PlayTrackId(track_id) => {
                let uri = normalize_track_uri(&track_id);
                if let Err(err) = self.call_with_refresh(|c| c.play_uris(&[uri.as_str()])) {
                    self.report_error("play track", &err);
                }
            }
            AppCommand::SetPlaying(true) => {
                if let Err(err) = self.call_with_refresh(SpotifyApiClient::play) {
                    self.report_error("resume playback", &err);
                }
            }
            AppCommand::SetPlaying(false) => {
                if let Err(err) = self.call_with_refresh(SpotifyApiClient::pause) {
                    self.report_error("pause playback", &err);
                }
            }
            AppCommand::NextTrack => {
                if let Err(err) = self.call_with_refresh(SpotifyApiClient::next_track) {
                    self.report_error("next track", &err);
                }
            }
            AppCommand::PrevTrack => {
                if let Err(err) = self.call_with_refresh(SpotifyApiClient::prev_track) {
                    self.report_error("previous track", &err);
                }
            }
            AppCommand::SeekMs(position_ms) => {
                if let Err(err) = self.call_with_refresh(|c| c.seek(position_ms)) {
                    self.report_error("seek", &err);
                }
            }
            AppCommand::SetShuffle(enabled) => {
                if let Err(err) = self.call_with_refresh(|c| c.set_shuffle(enabled)) {
                    self.report_error("shuffle", &err);
                }
            }
            AppCommand::SetRepeat(mode_str) => {
                let mode = match mode_str {
                    "context" => RepeatMode::Context,
                    "track" => RepeatMode::Track,
                    _ => RepeatMode::Off,
                };
                if let Err(err) = self.call_with_refresh(|c| c.set_repeat(mode)) {
                    self.report_error("repeat", &err);
                }
            }
            AppCommand::SetVolume(volume) => {
                if let Err(err) = self.call_with_refresh(|c| c.set_volume(volume)) {
                    self.report_error("volume", &err);
                }
            }
            AppCommand::SetTrackLiked { track_id, liked } => {
                if let Err(err) = self.call_with_refresh(|c| c.toggle_like(&track_id, liked)) {
                    self.report_error("like track", &err);
                }
            }
            AppCommand::EnqueueTrackId(track_id) => {
                let uri = normalize_track_uri(&track_id);
                if let Err(err) = self.call_with_refresh(|c| c.queue_track(&uri)) {
                    self.report_error("enqueue track", &err);
                }
            }
            AppCommand::AddTracksToPlaylist {
                playlist_id,
                track_ids,
            } => {
                if track_ids.is_empty() {
                    return;
                }
                let uris: Vec<String> =
                    track_ids.iter().map(|id| normalize_track_uri(id)).collect();
                let uri_refs: Vec<&str> = uris.iter().map(String::as_str).collect();
                if let Err(err) =
                    self.call_with_refresh(|c| c.add_tracks_to_playlist(&playlist_id, &uri_refs))
                {
                    self.report_error("add tracks to playlist", &err);
                }
            }
            AppCommand::TransferDevice(device_id) => {
                if let Err(err) = self.call_with_refresh(|c| c.transfer_playback(&device_id)) {
                    self.report_error("transfer device", &err);
                }
            }
            AppCommand::ExecuteSearch(query) => {
                if query.trim().is_empty() {
                    return;
                }
                match self.call_with_refresh(|c| c.search(&query)) {
                    Ok(results) => {
                        let _ = self
                            .upd_tx
                            .send(BackendUpdate::SearchResultsLoaded(results));
                    }
                    Err(err) => self.report_error("search", &err),
                }
            }
        }
    }

    fn report_error(&self, context: &str, err: &ApiError) {
        // ADR-0003: HTTP 403 restricted Spotify endpoints degrade silently without nagging.
        if err.is_gracefully_degradable() {
            return;
        }
        match err {
            ApiError::RateLimited { retry_after, .. } => {
                let _ = self.upd_tx.send(BackendUpdate::RateLimited(*retry_after));
                let _ = self.upd_tx.send(BackendUpdate::Toast {
                    level: ToastLevel::Warning,
                    text: format!(
                        "Spotify rate limit hit ({context}) — pausing sync for {}s",
                        retry_after.as_secs()
                    ),
                });
            }
            ApiError::Unauthorized => {
                let _ = self.upd_tx.send(BackendUpdate::Toast {
                    level: ToastLevel::Error,
                    text: String::from(
                        "Spotify session expired — run `vynl auth` to re-authenticate",
                    ),
                });
            }
            other => {
                let _ = self.upd_tx.send(BackendUpdate::Toast {
                    level: ToastLevel::Warning,
                    text: format!("Spotify {context} failed: {other}"),
                });
            }
        }
    }
}

fn normalize_track_uri(track_id: &str) -> String {
    if track_id.starts_with("spotify:") {
        track_id.to_owned()
    } else {
        format!("spotify:track:{track_id}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use vynl_core::state::AppCommand;
    use vynl_spotify::api::{HttpResponse, MockSpotifyTransport};
    use vynl_spotify::cache::{MetadataStore, RedbMetadataCache};
    use vynl_spotify::pkce::TokenBundle;

    use super::{BackendRequest, BackendUpdate, SpotifyWorkerHandle};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn fresh_tokens() -> TokenBundle {
        TokenBundle {
            access_token: String::from("valid-token"),
            refresh_token: String::from("valid-refresh"),
            expires_at_epoch_secs: u64::MAX / 2,
        }
    }

    #[test]
    #[expect(clippy::too_many_lines)]
    fn dispatches_commands_hydrates_redb_and_degrades_gracefully_on_403() {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let db_path = std::env::temp_dir().join(format!(
            "vynl-worker-test-{}-{seq}.redb",
            std::process::id()
        ));

        let transport = MockSpotifyTransport::new();
        // Bootstrap: me, liked_tracks, playlists, saved_albums, followed_artists (403),
        // saved_episodes, recently_played (403!), playback, devices
        transport.push_json(
            200,
            r#"{"id":"u1","display_name":"Ada","product":"premium"}"#,
        );
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_json(
            200,
            r#"{"items":[{"album":{"id":"alb1","name":"Discovery","release_date":"2001-03-12","artists":[{"name":"Daft Punk"}],"images":[]}}]}"#,
        );
        transport.push_response(HttpResponse::new(
            403,
            r#"{"error":{"message":"restricted"}}"#,
        ));
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_response(HttpResponse::new(
            403,
            r#"{"error":{"message":"restricted"}}"#,
        ));
        transport.push_response(HttpResponse::new(204, ""));
        transport.push_json(200, r#"{"devices":[]}"#);
        // Command responses: PlayTrackId, SetVolume, AddTracksToPlaylist, ExecuteSearch, FetchAlbumTracks
        transport.push_response(HttpResponse::new(204, ""));
        transport.push_response(HttpResponse::new(204, ""));
        transport.push_json(201, r#"{"snapshot_id":"snap-2"}"#);
        transport.push_json(
            200,
            r#"{"tracks":{"items":[{"id":"t9","name":"Voyager","duration_ms":227000,"explicit":false,"artists":[{"name":"Daft Punk"}],"album":{"name":"Discovery","images":[]}}]}}"#,
        );
        transport.push_json(
            200,
            r#"{"items":[{"id":"t10","name":"Veridis Quo","duration_ms":344000,"explicit":false,"artists":[{"name":"Daft Punk"}]}]}"#,
        );

        let cache = RedbMetadataCache::open(&db_path).ok();
        let worker_res = SpotifyWorkerHandle::spawn_with_transport(
            transport,
            String::from("client-1"),
            fresh_tokens(),
            cache,
            Box::new(|_| {}),
        );
        assert!(worker_res.is_ok());
        let Ok(worker) = worker_res else {
            return;
        };

        worker.send(BackendRequest::Bootstrap);
        worker.send(BackendRequest::Command(AppCommand::PlayTrackId(
            String::from("t9"),
        )));
        worker.send(BackendRequest::Command(AppCommand::SetVolume(75)));
        worker.send(BackendRequest::Command(AppCommand::AddTracksToPlaylist {
            playlist_id: String::from("pl1"),
            track_ids: vec![String::from("t9")],
        }));
        worker.send(BackendRequest::Command(AppCommand::ExecuteSearch(
            String::from("voyager"),
        )));
        worker.send(BackendRequest::FetchAlbumTracks {
            album_id: String::from("alb1"),
        });

        let mut saw_live_hydration = false;
        let mut saw_search = false;
        let mut saw_album_tracks = false;
        let mut saw_error_toast = false;

        for _ in 0..40 {
            for upd in worker.drain_updates() {
                match upd {
                    BackendUpdate::MetadataHydrated {
                        user: Some(ref u),
                        ref library,
                        from_cache: false,
                        ..
                    } => {
                        assert_eq!(u.display_name, "Ada");
                        assert_eq!(library.albums.len(), 1);
                        assert_eq!(library.albums[0].title, "Discovery");
                        saw_live_hydration = true;
                    }
                    BackendUpdate::SearchResultsLoaded(ref res) => {
                        assert_eq!(res.tracks.len(), 1);
                        assert_eq!(res.tracks[0].title, "Voyager");
                        saw_search = true;
                    }
                    BackendUpdate::AlbumTracksLoaded {
                        ref album_id,
                        ref tracks,
                    } => {
                        assert_eq!(album_id, "alb1");
                        assert_eq!(tracks.len(), 1);
                        assert_eq!(tracks[0].title, "Veridis Quo");
                        saw_album_tracks = true;
                    }
                    BackendUpdate::Toast { .. } => {
                        saw_error_toast = true;
                    }
                    _ => {}
                }
            }
            if saw_live_hydration && saw_search && saw_album_tracks {
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }

        drop(worker);

        assert!(saw_live_hydration);
        assert!(saw_search);
        assert!(saw_album_tracks);
        // 403 on followed_artists & recently_played must degrade silently with zero error toast!
        assert!(!saw_error_toast);

        // Verify redb persisted the hydrated profile cleanly.
        if let Ok(reopened) = RedbMetadataCache::open(&db_path)
            && let Ok(Some(meta)) = reopened.load_metadata()
        {
            assert_eq!(
                meta.user.as_ref().map(|u| u.display_name.as_str()),
                Some("Ada")
            );
            assert_eq!(meta.library.albums.len(), 1);
        }

        let _ = fs::remove_file(&db_path);
    }
}
