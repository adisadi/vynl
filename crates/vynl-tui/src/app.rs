//! Interactive `vynl` TUI application controller wiring `vynl-core`, `vynl-images`,
//! `vynl-spotify`, `vynl-player`, and `vynl-system` into a unified event loop.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use vynl_core::config::AppConfig;
#[cfg(all(
    not(target_os = "freebsd"),
    any(feature = "rodio-backend", feature = "subprocess-backend")
))]
use vynl_core::config::PlaybackMode;
use vynl_core::keymap::{
    KeyAction, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind, resolve_hybrid_key,
};
use vynl_core::model::{ToastLevel, TrackSummary};
#[cfg(all(
    not(target_os = "freebsd"),
    any(feature = "rodio-backend", feature = "subprocess-backend")
))]
use vynl_core::state::AppCommand;
use vynl_core::state::{ActiveModal, AppEvent, AppState, PanelFocus, Route, reduce};
use vynl_core::theme::Theme;
use vynl_images::{
    CachedFrame, HttpImageSource, ImageCacheKey, ImageProtocol, ImageSource, ImageWorker,
    L1ImageCache, L2DiskCache, OcclusionTracker, generate_demo_cover,
};
use vynl_player::PlaybackEngine;
#[cfg(all(
    not(target_os = "freebsd"),
    any(feature = "rodio-backend", feature = "subprocess-backend")
))]
use vynl_player::{
    IntegratedCommand, IntegratedEvent, IntegratedPlayerConfig, IntegratedPlayerHandle,
};
use vynl_spotify::demo::DemoCatalog;
use vynl_spotify::search::search_library;
use vynl_system::ipc::{IpcCommand, IpcResponse, IpcServer, StatusSnapshot};
use vynl_system::media::{MediaControlEvent, SystemMediaBridge};
use vynl_system::notify::{LocalOsNotifier, NotificationDispatcher, NotificationPayload};
use vynl_system::paths::AppPaths;

use crate::buffer::Buffer;
use crate::input::{TerminalGuard, TerminalInput, poll_terminal_event, query_terminal_size};
use crate::layout::FrameLayout;
use crate::render::{RenderContext, filter_palette_entries, render_frame};
use crate::spotify_worker::{BackendRequest, BackendUpdate, SpotifyWorkerHandle};
use crate::storage;

/// Live stateful application runtime owning UI state, caches, playback engine,
/// OS integrations, background Spotify/image workers, and demo/live catalogs.
pub struct VynlApp {
    /// Application configuration.
    pub config: AppConfig,
    /// Synchronous Elm UI state.
    pub state: AppState,
    /// Base theme before adaptive cover accent override.
    pub base_theme: Theme,
    /// Active theme (with adaptive cover accent applied when enabled).
    pub active_theme: Theme,
    /// Custom theme names discovered in `~/.config/vynl/themes/*.toml`.
    pub custom_themes: Vec<String>,
    /// Resolved terminal graphics protocol.
    pub protocol: ImageProtocol,
    /// L1 RAM LRU cache for encoded terminal images and palettes.
    pub l1_cache: L1ImageCache,
    /// Shared L2 disk image cache (also used to export PNG icons for OS notifications).
    disk_cache: Option<Arc<L2DiskCache>>,
    /// Per-frame Modal-Aware Image Occlusion tracker.
    pub occlusion: OcclusionTracker,
    /// Offline demo catalog (used in `--demo` and headless snapshot tests).
    pub demo: DemoCatalog,
    /// Whether this instance is running in offline demo mode (no live Spotify dispatch).
    pub demo_mode: bool,
    /// Whether configuration mutations (theme, Client ID, adaptive toggle) write to disk.
    pub persist_config: bool,
    /// Dual playback engine & stream cache coordinator.
    pub player: Option<PlaybackEngine>,
    /// Integrated `librespot` player handle when compiled and configured in integrated mode.
    #[cfg(all(
        not(target_os = "freebsd"),
        any(feature = "rodio-backend", feature = "subprocess-backend")
    ))]
    integrated_player: Option<IntegratedPlayerHandle>,
    /// Live local OS media control bridge (`souvlaki` MPRIS2/SMTC/MediaRemote with fallback).
    pub media_bridge: SystemMediaBridge,
    /// Inbound OS media-key event receiver.
    media_rx: mpsc::Receiver<MediaControlEvent>,
    /// Local OS desktop notifier.
    pub notifier: LocalOsNotifier,
    /// Resolved OS/XDG application paths.
    pub paths: AppPaths,
    /// Optional non-blocking background Spotify Web API + `redb` cache worker.
    spotify_worker: Option<SpotifyWorkerHandle>,
    /// Cached playlist tracks loaded from Spotify / `redb`, keyed by playlist ID.
    playlist_tracks: BTreeMap<String, Vec<TrackSummary>>,
    /// Cached album tracks loaded from Spotify, keyed by album ID.
    album_tracks: BTreeMap<String, Vec<TrackSummary>>,
    /// Cached artist top tracks loaded from Spotify, keyed by artist ID.
    artist_tracks: BTreeMap<String, Vec<TrackSummary>>,
    /// Known playlist snapshot IDs for `If-None-Match` cache validation.
    playlist_snapshots: BTreeMap<String, String>,
    /// Optional background HTTP cover fetcher & JPEG/PNG/WebP decoder worker.
    image_worker: Option<ImageWorker>,
    /// In-flight remote cover requests so we do not enqueue duplicates per frame.
    pending_images: HashSet<ImageCacheKey>,
    /// Monotonic counter for [`vynl_images::ImageRequest`] IDs.
    next_image_req_id: u64,
    /// Latest spectrum bars for Zen-mode FFT visualization.
    pub spectrum_bars: Vec<f32>,
    /// Tick counter for deterministic demo spectrum animation.
    tick_counter: u64,
}

impl VynlApp {
    /// Creates a new `vynl` application initialized with the offline demo catalog
    /// (ideal for `--render-demo` and deterministic headless snapshot tests — performs
    /// no config disk writes or background network I/O).
    #[must_use]
    pub fn new_demo(protocol: ImageProtocol) -> Self {
        let config = AppConfig::default();
        let paths = AppPaths::resolve();
        Self::build_demo(config, paths, protocol, false, false)
    }

    /// Creates a `--demo` interactive application honoring loaded disk configuration
    /// and custom themes from `~/.config/vynl/themes/*.toml`.
    #[must_use]
    pub fn new_demo_with_config(
        config: AppConfig,
        paths: AppPaths,
        protocol: ImageProtocol,
    ) -> Self {
        Self::build_demo(config, paths, protocol, true, true)
    }

    /// Creates a live `vynl` application backed by disk `config.toml`, custom themes,
    /// OS-keyring OAuth tokens, `HttpImageSource` cover art worker, `souvlaki` media
    /// controls, optional integrated `librespot` playback, and the background
    /// `SpotifyApiClient` + `redb` worker.
    #[must_use]
    pub fn new_live(config: AppConfig, paths: AppPaths, protocol: ImageProtocol) -> Self {
        let custom_themes = storage::discover_custom_themes(&paths);
        let base_theme = storage::load_theme(&paths, &config.ui.theme).unwrap_or_default();
        let player =
            PlaybackEngine::from_config(&config.spotify, paths.audio_cache_dir.clone()).ok();

        let (media_bridge, media_rx) = if config.integrations.media_controls {
            SystemMediaBridge::connect()
        } else {
            SystemMediaBridge::in_memory()
        };

        let (image_worker, disk_cache) = spawn_http_image_worker(&config, &paths);
        let l1_cap = config.images.max_memory_cache_items;
        let notif_enabled = config.integrations.notifications;

        let mut app = Self {
            l1_cache: L1ImageCache::new(l1_cap),
            disk_cache,
            occlusion: OcclusionTracker::new(),
            active_theme: base_theme.clone(),
            base_theme,
            custom_themes,
            protocol,
            state: AppState::default(),
            demo: DemoCatalog::builtin(),
            demo_mode: false,
            persist_config: true,
            player,
            #[cfg(all(
                not(target_os = "freebsd"),
                any(feature = "rodio-backend", feature = "subprocess-backend")
            ))]
            integrated_player: None,
            media_bridge,
            media_rx,
            notifier: LocalOsNotifier::new().with_enabled(notif_enabled),
            paths,
            config,
            spotify_worker: None,
            playlist_tracks: BTreeMap::new(),
            album_tracks: BTreeMap::new(),
            artist_tracks: BTreeMap::new(),
            playlist_snapshots: BTreeMap::new(),
            image_worker,
            pending_images: HashSet::new(),
            next_image_req_id: 1,
            spectrum_bars: vec![0.0; 16],
            tick_counter: 0,
        };

        app.try_connect_live_spotify();
        app.refresh_adaptive_theme();
        app.sync_os_bridges(false);
        app
    }

    fn build_demo(
        config: AppConfig,
        paths: AppPaths,
        protocol: ImageProtocol,
        persist_config: bool,
        connect_os_media: bool,
    ) -> Self {
        let demo = DemoCatalog::builtin();
        let custom_themes = if persist_config {
            storage::discover_custom_themes(&paths)
        } else {
            Vec::new()
        };
        let base_theme = if persist_config {
            storage::load_theme(&paths, &config.ui.theme).unwrap_or_default()
        } else {
            Theme::builtin(&config.ui.theme).unwrap_or_default()
        };

        let mut state = AppState {
            user: Some(demo.user.clone()),
            library: demo.to_library_state(),
            devices: demo.devices.clone(),
            playback: demo.initial_playback_state(),
            ..AppState::default()
        };
        state.queue.up_next = demo.tracks.iter().skip(1).take(5).cloned().collect();
        state
            .queue
            .recently_played
            .clone_from(&demo.recently_played);
        state.search_results = demo.search("");

        let player =
            PlaybackEngine::from_config(&config.spotify, paths.audio_cache_dir.clone()).ok();

        let (media_bridge, media_rx) = if connect_os_media && config.integrations.media_controls {
            SystemMediaBridge::connect()
        } else {
            SystemMediaBridge::in_memory()
        };

        let l1_cap = config.images.max_memory_cache_items;
        let notif_enabled = config.integrations.notifications;

        let mut app = Self {
            l1_cache: L1ImageCache::new(l1_cap),
            disk_cache: None,
            occlusion: OcclusionTracker::new(),
            active_theme: base_theme.clone(),
            base_theme,
            custom_themes,
            protocol,
            state,
            demo,
            demo_mode: true,
            persist_config,
            player,
            #[cfg(all(
                not(target_os = "freebsd"),
                any(feature = "rodio-backend", feature = "subprocess-backend")
            ))]
            integrated_player: None,
            media_bridge,
            media_rx,
            notifier: LocalOsNotifier::new().with_enabled(notif_enabled),
            paths,
            config,
            spotify_worker: None,
            playlist_tracks: BTreeMap::new(),
            album_tracks: BTreeMap::new(),
            artist_tracks: BTreeMap::new(),
            playlist_snapshots: BTreeMap::new(),
            image_worker: None,
            pending_images: HashSet::new(),
            next_image_req_id: 1,
            spectrum_bars: vec![0.0; 16],
            tick_counter: 0,
        };
        app.refresh_adaptive_theme();
        app.sync_os_bridges(false);
        app
    }

    /// Attaches a pre-built [`SpotifyWorkerHandle`] (useful for tests with a mock transport)
    /// and switches the app into live dispatch mode.
    pub fn attach_spotify_worker(&mut self, worker: SpotifyWorkerHandle) {
        self.demo_mode = false;
        self.spotify_worker = Some(worker);
    }

    fn try_connect_live_spotify(&mut self) {
        let Some(client_id) = self.config.spotify.client_id.clone() else {
            self.dispatch(AppEvent::OpenModal(ActiveModal::SetupWizard {
                client_id_input: String::new(),
            }));
            return;
        };

        match storage::load_tokens(&self.paths) {
            Ok(Some(tokens)) => {
                #[cfg(all(
                    not(target_os = "freebsd"),
                    any(feature = "rodio-backend", feature = "subprocess-backend")
                ))]
                if self.config.spotify.playback_mode == PlaybackMode::Integrated {
                    let player_cfg = IntegratedPlayerConfig::from_spotify_config(
                        &self.config.spotify,
                        self.paths.audio_cache_dir.clone(),
                    );
                    if let Ok(handle) = IntegratedPlayerHandle::spawn_with_access_token(
                        player_cfg,
                        tokens.access_token.clone(),
                    ) {
                        self.integrated_player = Some(handle);
                    }
                }

                match SpotifyWorkerHandle::spawn_live(&self.paths, client_id, tokens) {
                    Ok(worker) => {
                        worker.send(BackendRequest::Bootstrap);
                        self.spotify_worker = Some(worker);
                    }
                    Err(err) => {
                        self.dispatch(AppEvent::ShowToast {
                            level: ToastLevel::Error,
                            text: format!("Failed to start Spotify worker: {err}"),
                        });
                    }
                }
            }
            Ok(None) => {
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Info,
                    text: String::from(
                        "Run `vynl auth` to sign in to Spotify (or launch with `--demo`)",
                    ),
                });
            }
            Err(err) => {
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Warning,
                    text: format!("Could not load saved Spotify tokens: {err} — run `vynl auth`"),
                });
            }
        }
    }

    /// Central Elm reducer dispatcher: updates [`AppState`] and forwards every emitted
    /// [`vynl_core::state::AppCommand`] to the integrated player and/or background
    /// Spotify Web API worker when connected.
    fn dispatch(&mut self, event: AppEvent) {
        let commands = reduce(&mut self.state, event);
        for cmd in commands {
            #[cfg(all(
                not(target_os = "freebsd"),
                any(feature = "rodio-backend", feature = "subprocess-backend")
            ))]
            if let Some(ref integrated) = self.integrated_player {
                let handled_locally = match &cmd {
                    AppCommand::PlayTrackId(track_id) => {
                        let uri = if track_id.starts_with("spotify:") {
                            track_id.clone()
                        } else {
                            format!("spotify:track:{track_id}")
                        };
                        let _ = integrated.send(IntegratedCommand::LoadTrack {
                            spotify_uri: uri,
                            start_playing: true,
                            position_ms: 0,
                        });
                        true
                    }
                    AppCommand::SetPlaying(true) => {
                        let _ = integrated.send(IntegratedCommand::Play);
                        true
                    }
                    AppCommand::SetPlaying(false) => {
                        let _ = integrated.send(IntegratedCommand::Pause);
                        true
                    }
                    AppCommand::SeekMs(ms) => {
                        let _ = integrated.send(IntegratedCommand::Seek(
                            u32::try_from(*ms).unwrap_or(u32::MAX),
                        ));
                        true
                    }
                    AppCommand::SetVolume(vol) => {
                        let _ = integrated.send(IntegratedCommand::SetVolume(*vol));
                        true
                    }
                    _ => false,
                };
                if handled_locally {
                    continue;
                }
            }

            if let Some(ref worker) = self.spotify_worker {
                worker.send(BackendRequest::Command(cmd));
            }
        }
    }

    /// Computes the list of tracks currently visible for the active route + inline filter.
    #[must_use]
    pub fn active_view_tracks(&self) -> Vec<TrackSummary> {
        let base: Vec<TrackSummary> = match &self.state.route {
            Route::Home | Route::LikedSongs => self.state.library.liked_tracks.clone(),
            Route::Playlist(id) => {
                if let Some(tracks) = self.playlist_tracks.get(id) {
                    tracks.clone()
                } else if self.demo_mode {
                    self.demo.tracks_for_playlist(id)
                } else {
                    Vec::new()
                }
            }
            Route::Album(id) => {
                if let Some(tracks) = self.album_tracks.get(id) {
                    tracks.clone()
                } else {
                    self.state
                        .library
                        .albums
                        .iter()
                        .find(|a| a.id == *id)
                        .map_or_else(Vec::new, |a| {
                            if self.demo_mode {
                                self.demo.tracks_for_album(&a.title)
                            } else {
                                self.state
                                    .library
                                    .liked_tracks
                                    .iter()
                                    .filter(|t| t.album == a.title)
                                    .cloned()
                                    .collect()
                            }
                        })
                }
            }
            Route::Artist(id) => {
                if let Some(tracks) = self.artist_tracks.get(id) {
                    tracks.clone()
                } else {
                    self.state
                        .library
                        .artists
                        .iter()
                        .find(|a| a.id == *id)
                        .map_or_else(Vec::new, |artist| {
                            let source = if self.demo_mode {
                                &self.demo.tracks
                            } else {
                                &self.state.library.liked_tracks
                            };
                            source
                                .iter()
                                .filter(|t| t.artists.iter().any(|n| n == &artist.name))
                                .cloned()
                                .collect()
                        })
                }
            }
            Route::Search => self.state.search_results.tracks.clone(),
            Route::Podcasts | Route::ZenNowPlaying => Vec::new(),
        };

        if let Some(ref query) = self.state.inline_filter
            && !query.trim().is_empty()
        {
            let ranked = vynl_core::fuzzy::rank_items(query, &base, |t| {
                format!("{} {} {}", t.title, t.artist_line(), t.album)
            });
            return ranked
                .into_iter()
                .filter_map(|(i, _)| base.get(i).cloned())
                .collect();
        }
        base
    }

    /// Renders a single snapshot frame at `(cols, rows)` without touching stdout.
    pub fn render_snapshot(&mut self, cols: u16, rows: u16) -> Buffer {
        self.dispatch(AppEvent::Resize { cols, rows });
        let view_tracks = self.active_view_tracks();
        let mut ctx = RenderContext {
            state: &self.state,
            theme: &self.active_theme,
            protocol: self.protocol,
            images_enabled: self.config.images.enabled,
            view_tracks: &view_tracks,
            spectrum_bars: &self.spectrum_bars,
            custom_themes: &self.custom_themes,
            l1_cache: &mut self.l1_cache,
            occlusion: &mut self.occlusion,
            image_worker: self.image_worker.as_ref(),
            pending_images: &mut self.pending_images,
            next_image_req_id: &mut self.next_image_req_id,
        };
        render_frame(cols, rows, &mut ctx)
    }

    /// Drains completed background image decodes, Spotify Web API updates, integrated
    /// player events, and OS media control events without blocking the UI thread.
    pub fn poll_background_workers(&mut self) {
        // 1. Drain completed remote cover decodes from ImageWorker into L1 cache.
        if let Some(ref worker) = self.image_worker {
            let active_cover_url = self
                .state
                .playback
                .current_track
                .as_ref()
                .and_then(|t| t.cover.as_ref())
                .map(|c| c.url.clone());

            for ready in worker.drain_ready() {
                self.pending_images.remove(&ready.key);
                let is_active_cover = active_cover_url.as_deref() == Some(ready.key.url.as_str());
                let palette = ready.palette.clone();
                self.l1_cache.insert(
                    ready.key,
                    CachedFrame {
                        encoded: ready.encoded,
                        palette: ready.palette,
                    },
                );
                if is_active_cover && self.config.ui.adaptive_cover_colors && !palette.is_empty() {
                    self.active_theme = self.base_theme.with_adaptive_palette(&palette);
                }
            }
        }

        // 2. Drain background Spotify Web API + redb cache updates.
        let updates = self
            .spotify_worker
            .as_ref()
            .map(SpotifyWorkerHandle::drain_updates)
            .unwrap_or_default();
        for update in updates {
            self.apply_backend_update(update);
        }

        // 3. Drain event-driven updates from the integrated librespot playback engine.
        #[cfg(all(
            not(target_os = "freebsd"),
            any(feature = "rodio-backend", feature = "subprocess-backend")
        ))]
        {
            let mut integrated_events = Vec::new();
            if let Some(ref integrated) = self.integrated_player {
                while let Some(ev) = integrated.try_recv_event() {
                    integrated_events.push(ev);
                }
            }
            for ev in integrated_events {
                self.apply_integrated_event(ev);
            }
        }

        // 4. Drain inbound OS media-key events (MPRIS2 / SMTC / MediaRemote).
        let mut media_events = Vec::new();
        while let Ok(ev) = self.media_rx.try_recv() {
            media_events.push(ev);
        }
        for ev in media_events {
            self.handle_media_control_event(ev);
        }
    }

    #[cfg(all(
        not(target_os = "freebsd"),
        any(feature = "rodio-backend", feature = "subprocess-backend")
    ))]
    fn apply_integrated_event(&mut self, event: IntegratedEvent) {
        match event {
            IntegratedEvent::Connected { device_name } => {
                self.state.playback.active_device = Some(vynl_core::model::ConnectDevice {
                    id: String::from("vynl-integrated"),
                    name: device_name.clone(),
                    device_type: String::from("Computer"),
                    is_active: true,
                    volume_percent: self.state.playback.volume_percent,
                });
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Info,
                    text: format!("Integrated Spotify Connect active: {device_name}"),
                });
            }
            IntegratedEvent::Playing { position_ms, .. } => {
                self.state.playback.is_playing = true;
                self.state.playback.position_ms = u64::from(position_ms);
                self.sync_os_bridges(false);
            }
            IntegratedEvent::Paused { position_ms, .. } => {
                self.state.playback.is_playing = false;
                self.state.playback.position_ms = u64::from(position_ms);
                self.sync_os_bridges(false);
            }
            IntegratedEvent::Stopped => {
                self.state.playback.is_playing = false;
                self.sync_os_bridges(false);
            }
            IntegratedEvent::TrackEnded { .. } => {
                self.execute_key_action(KeyAction::NextTrack);
            }
            IntegratedEvent::Error(msg) => {
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Warning,
                    text: format!("Integrated player: {msg}"),
                });
            }
        }
    }

    fn apply_backend_update(&mut self, update: BackendUpdate) {
        match update {
            BackendUpdate::MetadataHydrated {
                user,
                library,
                recently_played,
                playlist_tracks,
                from_cache: _,
            } => {
                if user.is_some() {
                    self.state.user = user;
                }
                self.state.library = library;
                self.state.queue.recently_played = recently_played;
                for (id, tracks) in playlist_tracks {
                    self.playlist_tracks.insert(id, tracks);
                }
                self.clamp_selections();
            }
            BackendUpdate::PlaybackSynced(maybe_playback) => {
                let prev_id = self
                    .state
                    .playback
                    .current_track
                    .as_ref()
                    .map(|t| t.id.clone());
                if let Some(playback) = maybe_playback {
                    self.state.playback = playback;
                } else {
                    self.state.playback.is_playing = false;
                }
                let next_id = self
                    .state
                    .playback
                    .current_track
                    .as_ref()
                    .map(|t| t.id.clone());
                let changed = prev_id != next_id;
                if changed {
                    self.refresh_adaptive_theme();
                }
                self.sync_os_bridges(changed);
            }
            BackendUpdate::DevicesLoaded(devices) => {
                self.state.devices = devices;
            }
            BackendUpdate::PlaylistTracksLoaded {
                playlist_id,
                snapshot_id,
                tracks,
            } => {
                self.playlist_snapshots
                    .insert(playlist_id.clone(), snapshot_id);
                self.playlist_tracks.insert(playlist_id, tracks);
                self.clamp_selections();
            }
            BackendUpdate::AlbumTracksLoaded { album_id, tracks } => {
                self.album_tracks.insert(album_id, tracks);
                self.clamp_selections();
            }
            BackendUpdate::ArtistTracksLoaded { artist_id, tracks } => {
                self.artist_tracks.insert(artist_id, tracks);
                self.clamp_selections();
            }
            BackendUpdate::SearchResultsLoaded(results) => {
                self.state.search_results = results;
                self.clamp_selections();
            }
            BackendUpdate::RateLimited(retry_after) => {
                if let Some(ref mut player) = self.player {
                    player.sync_mut().register_rate_limit(retry_after);
                }
            }
            BackendUpdate::Toast { level, text } => {
                self.dispatch(AppEvent::ShowToast { level, text });
            }
        }
    }

    /// Translates an inbound OS media control event (MPRIS2 / SMTC / `MediaRemote`)
    /// into the standard application reducer + backend dispatch pipeline.
    pub fn handle_media_control_event(&mut self, event: MediaControlEvent) {
        match event {
            MediaControlEvent::Play => {
                if !self.state.playback.is_playing {
                    self.execute_key_action(KeyAction::TogglePlayback);
                }
            }
            MediaControlEvent::Pause | MediaControlEvent::Stop => {
                if self.state.playback.is_playing {
                    self.execute_key_action(KeyAction::TogglePlayback);
                }
            }
            MediaControlEvent::PlayPause => self.execute_key_action(KeyAction::TogglePlayback),
            MediaControlEvent::Next => self.execute_key_action(KeyAction::NextTrack),
            MediaControlEvent::Previous => self.execute_key_action(KeyAction::PrevTrack),
            MediaControlEvent::SeekByMs(delta_ms) => {
                self.dispatch(AppEvent::SeekRelative(delta_ms));
                self.sync_os_bridges(false);
            }
            MediaControlEvent::SetVolume(vol) => {
                self.dispatch(AppEvent::SetVolume(vol.min(100)));
                self.sync_os_bridges(false);
            }
        }
    }

    /// Processes a single decoded keyboard event (handling active modals, inline filters,
    /// search input, and hybrid global keybindings).
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key == KeyEvent::ctrl('c') {
            self.dispatch(AppEvent::Quit);
            return;
        }

        // 1. Active modal captures input first
        if let Some(modal) = self.state.modal.clone() {
            self.handle_modal_key(modal, key);
            return;
        }

        // 2. Active inline filter captures typing
        if let Some(mut filter) = self.state.inline_filter.clone() {
            match key.code {
                KeyCode::Esc => {
                    self.dispatch(AppEvent::SetInlineFilter(None));
                    return;
                }
                KeyCode::Enter => return,
                KeyCode::Backspace => {
                    filter.pop();
                    self.dispatch(AppEvent::SetInlineFilter(Some(filter)));
                    return;
                }
                KeyCode::Char(c) if !key.modifiers.ctrl && !key.modifiers.alt => {
                    filter.push(c);
                    self.dispatch(AppEvent::SetInlineFilter(Some(filter)));
                    return;
                }
                _ => {}
            }
        }

        // 3. Search view typing when focused on Main
        if self.state.route == Route::Search && self.state.focus == PanelFocus::Main {
            match key.code {
                KeyCode::Backspace => {
                    let mut q = self.state.search_query.clone();
                    q.pop();
                    self.update_search(q);
                    return;
                }
                KeyCode::Char(c)
                    if !key.modifiers.ctrl && !key.modifiers.alt && !matches!(c, '?' | ':') =>
                {
                    let mut q = self.state.search_query.clone();
                    q.push(c);
                    self.update_search(q);
                    return;
                }
                _ => {}
            }
        }

        // 4. Hybrid global keymap
        let Some(action) = resolve_hybrid_key(key) else {
            return;
        };
        self.execute_key_action(action);
    }

    #[expect(clippy::too_many_lines)]
    fn handle_modal_key(&mut self, modal: ActiveModal, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.dispatch(AppEvent::DismissModal);
            return;
        }

        match modal {
            ActiveModal::CommandPalette {
                mut query,
                mut selected,
            } => {
                let matches = filter_palette_entries(&query, &self.custom_themes);
                match key.code {
                    KeyCode::Down => {
                        if !matches.is_empty() {
                            selected = (selected + 1) % matches.len();
                        }
                        self.state.modal = Some(ActiveModal::CommandPalette { query, selected });
                    }
                    KeyCode::Up => {
                        if !matches.is_empty() {
                            selected = selected.checked_sub(1).unwrap_or(matches.len() - 1);
                        }
                        self.state.modal = Some(ActiveModal::CommandPalette { query, selected });
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        self.state.modal = Some(ActiveModal::CommandPalette { query, selected: 0 });
                    }
                    KeyCode::Char(c) if !key.modifiers.ctrl && !key.modifiers.alt => {
                        query.push(c);
                        self.state.modal = Some(ActiveModal::CommandPalette { query, selected: 0 });
                    }
                    KeyCode::Enter => {
                        if let Some(cmd) = matches.get(selected) {
                            let id = cmd.id.clone();
                            self.state.modal = None;
                            self.execute_palette_command(&id);
                        }
                    }
                    _ => {}
                }
            }
            ActiveModal::DevicePicker { mut selected } => {
                let len = self.state.devices.len();
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') if len > 0 => {
                        selected = (selected + 1) % len;
                        self.state.modal = Some(ActiveModal::DevicePicker { selected });
                    }
                    KeyCode::Up | KeyCode::Char('k') if len > 0 => {
                        selected = selected.checked_sub(1).unwrap_or(len - 1);
                        self.state.modal = Some(ActiveModal::DevicePicker { selected });
                    }
                    KeyCode::Enter => {
                        self.dispatch(AppEvent::SelectDevice(selected));
                    }
                    _ => {}
                }
            }
            ActiveModal::AddToPlaylist {
                track_ids,
                mut selected,
            } => {
                let len = self.state.library.playlists.len();
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') if len > 0 => {
                        selected = (selected + 1) % len;
                        self.state.modal = Some(ActiveModal::AddToPlaylist {
                            track_ids,
                            selected,
                        });
                    }
                    KeyCode::Up | KeyCode::Char('k') if len > 0 => {
                        selected = selected.checked_sub(1).unwrap_or(len - 1);
                        self.state.modal = Some(ActiveModal::AddToPlaylist {
                            track_ids,
                            selected,
                        });
                    }
                    KeyCode::Enter => {
                        if let Some(pl) = self.state.library.playlists.get(selected) {
                            let playlist_id = pl.id.clone();
                            self.dispatch(AppEvent::AddTracksToPlaylist {
                                playlist_id,
                                track_ids,
                            });
                        }
                    }
                    _ => {}
                }
            }
            ActiveModal::Help => {
                if matches!(key.code, KeyCode::Char('?' | 'q') | KeyCode::Enter) {
                    self.state.modal = None;
                }
            }
            ActiveModal::SetupWizard {
                mut client_id_input,
            } => match key.code {
                KeyCode::Backspace => {
                    client_id_input.pop();
                    self.state.modal = Some(ActiveModal::SetupWizard { client_id_input });
                }
                KeyCode::Char(c) if !key.modifiers.ctrl && !key.modifiers.alt => {
                    client_id_input.push(c);
                    self.state.modal = Some(ActiveModal::SetupWizard { client_id_input });
                }
                KeyCode::Enter => {
                    let trimmed = client_id_input.trim();
                    if !trimmed.is_empty() {
                        self.config.spotify.client_id = Some(trimmed.to_owned());
                    }
                    self.state.modal = None;
                    if self.persist_config {
                        match storage::save_config(&self.paths, &self.config) {
                            Ok(()) => {
                                self.dispatch(AppEvent::ShowToast {
                                    level: ToastLevel::Info,
                                    text: String::from(
                                        "Spotify Client ID saved to config.toml — run `vynl auth` to log in",
                                    ),
                                });
                            }
                            Err(err) => {
                                self.dispatch(AppEvent::ShowToast {
                                    level: ToastLevel::Error,
                                    text: format!("Failed to save config.toml: {err}"),
                                });
                            }
                        }
                    } else {
                        self.dispatch(AppEvent::ShowToast {
                            level: ToastLevel::Info,
                            text: String::from("Spotify Client ID saved"),
                        });
                    }
                }
                _ => {}
            },
        }
    }

    #[expect(clippy::too_many_lines)]
    fn execute_key_action(&mut self, action: KeyAction) {
        let prev_track_id = self
            .state
            .playback
            .current_track
            .as_ref()
            .map(|t| t.id.clone());

        match action {
            KeyAction::MoveUp => {
                self.dispatch(AppEvent::MoveSelection(-1));
            }
            KeyAction::MoveDown => {
                self.dispatch(AppEvent::MoveSelection(1));
                self.clamp_selections();
            }
            KeyAction::FocusLeft => {
                self.dispatch(AppEvent::SetFocus(PanelFocus::Sidebar));
            }
            KeyAction::FocusRight => {
                self.dispatch(AppEvent::SetFocus(PanelFocus::Main));
            }
            KeyAction::JumpTop => {
                self.dispatch(AppEvent::JumpSelection { top: true });
            }
            KeyAction::JumpBottom => {
                self.dispatch(AppEvent::JumpSelection { top: false });
                self.clamp_selections();
            }
            KeyAction::NextPanel => {
                self.dispatch(AppEvent::FocusNextPanel);
            }
            KeyAction::PrevPanel => {
                self.dispatch(AppEvent::FocusPrevPanel);
            }
            KeyAction::Select => self.activate_selection(),
            KeyAction::TogglePlayback => {
                self.dispatch(AppEvent::TogglePlayback);
            }
            KeyAction::NextTrack => {
                self.dispatch(AppEvent::NextTrack);
            }
            KeyAction::PrevTrack => {
                self.dispatch(AppEvent::PrevTrack);
            }
            KeyAction::ToggleShuffle => {
                self.dispatch(AppEvent::ToggleShuffle);
            }
            KeyAction::CycleRepeat => {
                self.dispatch(AppEvent::CycleRepeat);
            }
            KeyAction::VolumeUp => {
                self.dispatch(AppEvent::AdjustVolume(5));
            }
            KeyAction::VolumeDown => {
                self.dispatch(AppEvent::AdjustVolume(-5));
            }
            KeyAction::ToggleMute => {
                self.dispatch(AppEvent::ToggleMute);
            }
            KeyAction::SeekForward => {
                self.dispatch(AppEvent::SeekRelative(5_000));
            }
            KeyAction::SeekBackward => {
                self.dispatch(AppEvent::SeekRelative(-5_000));
            }
            KeyAction::OpenSearch => {
                self.dispatch(AppEvent::Navigate(Route::Search));
                self.dispatch(AppEvent::SetFocus(PanelFocus::Main));
            }
            KeyAction::OpenInlineFilter => {
                if self.state.focus == PanelFocus::Sidebar {
                    self.dispatch(AppEvent::CycleLibraryFilter);
                    self.clamp_selections();
                } else {
                    self.dispatch(AppEvent::SetInlineFilter(Some(String::new())));
                }
            }
            KeyAction::OpenCommandPalette => {
                self.dispatch(AppEvent::OpenModal(ActiveModal::CommandPalette {
                    query: String::new(),
                    selected: 0,
                }));
            }
            KeyAction::ToggleLike => {
                self.dispatch(AppEvent::ToggleCurrentLike);
            }
            KeyAction::AddToQueue => {
                let tracks = self.active_view_tracks();
                if let Some(track) = tracks.get(self.state.main_selected).cloned() {
                    self.dispatch(AppEvent::EnqueueTrack(track));
                }
            }
            KeyAction::OpenPlaylistModal => {
                let tracks = self.active_view_tracks();
                let mut ids: Vec<String> = self
                    .state
                    .multi_selected
                    .iter()
                    .filter_map(|&i| tracks.get(i).map(|t| t.id.clone()))
                    .collect();
                if ids.is_empty()
                    && let Some(track) = tracks.get(self.state.main_selected)
                {
                    ids.push(track.id.clone());
                }
                if !ids.is_empty() {
                    self.dispatch(AppEvent::OpenModal(ActiveModal::AddToPlaylist {
                        track_ids: ids,
                        selected: 0,
                    }));
                }
            }
            KeyAction::ToggleMultiSelect => {
                let idx = self.state.main_selected;
                self.dispatch(AppEvent::ToggleMultiSelect(idx));
            }
            KeyAction::OpenDevicePicker => {
                self.dispatch(AppEvent::OpenModal(ActiveModal::DevicePicker {
                    selected: 0,
                }));
            }
            KeyAction::ToggleQueue => {
                self.dispatch(AppEvent::ToggleQueuePanel);
            }
            KeyAction::ToggleZenMode => {
                self.dispatch(AppEvent::ToggleZenMode);
            }
            KeyAction::NavigateBack => {
                self.dispatch(AppEvent::NavigateBack);
            }
            KeyAction::OpenHelp => {
                self.dispatch(AppEvent::OpenModal(ActiveModal::Help));
            }
            KeyAction::Dismiss => {
                self.dispatch(AppEvent::DismissModal);
            }
            KeyAction::Quit => {
                self.dispatch(AppEvent::Quit);
            }
        }

        let next_track_id = self
            .state
            .playback
            .current_track
            .as_ref()
            .map(|t| t.id.clone());
        let track_changed = prev_track_id != next_track_id;
        if track_changed {
            self.refresh_adaptive_theme();
        }
        self.sync_os_bridges(track_changed);
    }

    fn execute_palette_command(&mut self, id: &str) {
        if let Some(custom_name) = id.strip_prefix("theme_custom:") {
            self.switch_theme(custom_name);
            return;
        }
        match id {
            "play_pause" => self.execute_key_action(KeyAction::TogglePlayback),
            "next_track" => self.execute_key_action(KeyAction::NextTrack),
            "prev_track" => self.execute_key_action(KeyAction::PrevTrack),
            "toggle_shuffle" => self.execute_key_action(KeyAction::ToggleShuffle),
            "cycle_repeat" => self.execute_key_action(KeyAction::CycleRepeat),
            "device_picker" => self.execute_key_action(KeyAction::OpenDevicePicker),
            "zen_mode" => self.execute_key_action(KeyAction::ToggleZenMode),
            "toggle_queue" => self.execute_key_action(KeyAction::ToggleQueue),
            "go_home" => self.dispatch(AppEvent::Navigate(Route::Home)),
            "go_search" => self.execute_key_action(KeyAction::OpenSearch),
            "go_liked" => self.dispatch(AppEvent::Navigate(Route::LikedSongs)),
            "go_podcasts" => self.dispatch(AppEvent::Navigate(Route::Podcasts)),
            "theme_vynl_dark" => self.switch_theme("vynl-dark"),
            "theme_catppuccin" => self.switch_theme("catppuccin-mocha"),
            "theme_tokyo_night" => self.switch_theme("tokyo-night"),
            "theme_gruvbox" => self.switch_theme("gruvbox"),
            "theme_nord" => self.switch_theme("nord"),
            "toggle_adaptive" => {
                self.config.ui.adaptive_cover_colors = !self.config.ui.adaptive_cover_colors;
                self.refresh_adaptive_theme();
                if self.persist_config {
                    let _ = storage::save_config(&self.paths, &self.config);
                }
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Info,
                    text: format!(
                        "Adaptive cover colors: {}",
                        if self.config.ui.adaptive_cover_colors {
                            "ON"
                        } else {
                            "OFF"
                        }
                    ),
                });
            }
            "open_help" => self.execute_key_action(KeyAction::OpenHelp),
            "quit" => self.execute_key_action(KeyAction::Quit),
            _ => {}
        }
    }

    /// Loads and activates a built-in or custom `~/.config/vynl/themes/<name>.toml` theme.
    pub fn switch_theme(&mut self, name: &str) {
        let loaded = if self.persist_config {
            storage::load_theme(&self.paths, name)
        } else {
            Theme::builtin(name).ok_or_else(|| format!("unknown theme `{name}`"))
        };

        match loaded {
            Ok(theme) => {
                self.base_theme = theme;
                name.clone_into(&mut self.config.ui.theme);
                self.refresh_adaptive_theme();
                if self.persist_config {
                    let _ = storage::save_config(&self.paths, &self.config);
                }
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Info,
                    text: format!("Theme switched to {name}"),
                });
            }
            Err(err) => {
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Error,
                    text: format!("Could not load theme `{name}`: {err}"),
                });
            }
        }
    }

    fn activate_selection(&mut self) {
        match self.state.focus {
            PanelFocus::Sidebar => {
                let sel = self.state.sidebar_selected;
                let route = match sel {
                    0 => Route::Home,
                    1 => Route::Search,
                    2 => Route::LikedSongs,
                    3 => Route::Podcasts,
                    4 => Route::ZenNowPlaying,
                    idx => {
                        let list_idx = idx.saturating_sub(5);
                        match self.state.library_filter {
                            vynl_core::model::LibraryFilter::All
                            | vynl_core::model::LibraryFilter::Playlists => {
                                let Some(pl) = self.state.library.playlists.get(list_idx) else {
                                    return;
                                };
                                let pl_id = pl.id.clone();
                                self.request_playlist_tracks(&pl_id);
                                Route::Playlist(pl_id)
                            }
                            vynl_core::model::LibraryFilter::Albums => {
                                let Some(alb) = self.state.library.albums.get(list_idx) else {
                                    return;
                                };
                                let alb_id = alb.id.clone();
                                self.request_album_tracks(&alb_id);
                                Route::Album(alb_id)
                            }
                            vynl_core::model::LibraryFilter::Artists => {
                                let Some(artist) = self.state.library.artists.get(list_idx) else {
                                    return;
                                };
                                let artist_id = artist.id.clone();
                                self.request_artist_top_tracks(&artist_id);
                                Route::Artist(artist_id)
                            }
                            vynl_core::model::LibraryFilter::Podcasts => Route::Podcasts,
                        }
                    }
                };
                self.dispatch(AppEvent::Navigate(route));
                self.dispatch(AppEvent::SetFocus(PanelFocus::Main));
            }
            PanelFocus::Main | PanelFocus::RightSidebar => {
                let tracks = self.active_view_tracks();
                if let Some(track) = tracks.get(self.state.main_selected).cloned() {
                    self.dispatch(AppEvent::PlayTrack(track));
                    self.refresh_adaptive_theme();
                    self.sync_os_bridges(true);
                }
            }
        }
    }

    fn request_playlist_tracks(&self, playlist_id: &str) {
        if let Some(ref worker) = self.spotify_worker {
            let cached_snap = self.playlist_snapshots.get(playlist_id).cloned();
            worker.send(BackendRequest::FetchPlaylistTracks {
                playlist_id: playlist_id.to_owned(),
                cached_snapshot_id: cached_snap,
            });
        }
    }

    fn request_album_tracks(&self, album_id: &str) {
        if let Some(ref worker) = self.spotify_worker {
            worker.send(BackendRequest::FetchAlbumTracks {
                album_id: album_id.to_owned(),
            });
        }
    }

    fn request_artist_top_tracks(&self, artist_id: &str) {
        if let Some(ref worker) = self.spotify_worker {
            worker.send(BackendRequest::FetchArtistTopTracks {
                artist_id: artist_id.to_owned(),
            });
        }
    }

    fn clamp_selections(&mut self) {
        let filter_items = match self.state.library_filter {
            vynl_core::model::LibraryFilter::All | vynl_core::model::LibraryFilter::Playlists => {
                self.state.library.playlists.len()
            }
            vynl_core::model::LibraryFilter::Albums => self.state.library.albums.len(),
            vynl_core::model::LibraryFilter::Artists => self.state.library.artists.len(),
            vynl_core::model::LibraryFilter::Podcasts => self.state.library.episodes.len(),
        };
        let sidebar_len = 5 + filter_items;
        if sidebar_len > 0 {
            self.state.sidebar_selected = self.state.sidebar_selected.min(sidebar_len - 1);
        }
        let main_len = self.active_view_tracks().len();
        if main_len > 0 {
            self.state.main_selected = self.state.main_selected.min(main_len - 1);
        } else {
            self.state.main_selected = 0;
        }
    }

    fn update_search(&mut self, query: String) {
        if self.demo_mode {
            self.state.search_results = self.demo.search(&query);
        } else {
            // Instant local nucleo search over cached library while remote API responds.
            self.state.search_results = search_library(&query, &self.state.library);
        }
        self.dispatch(AppEvent::SetSearchQuery(query));
    }

    /// Processes a decoded mouse event (click, scroll wheel, drag scrubbing).
    pub fn handle_mouse(&mut self, event: MouseEvent, cols: u16, rows: u16) {
        if !self.config.ui.mouse_enabled {
            return;
        }
        let layout = FrameLayout::compute(cols, rows, self.state.show_right_sidebar);

        match event.kind {
            MouseEventKind::ScrollUp => {
                self.dispatch(AppEvent::MoveSelection(-1));
            }
            MouseEventKind::ScrollDown => {
                self.dispatch(AppEvent::MoveSelection(1));
                self.clamp_selections();
            }
            MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Drag(MouseButton::Left) => {
                // Click or drag on Progress Bar -> Seek!
                if layout.progress_bar.contains(event.col, event.row)
                    && let Some(ref track) = self.state.playback.current_track
                    && layout.progress_bar.width > 0
                {
                    let rel = u64::from(event.col.saturating_sub(layout.progress_bar.x));
                    let target = rel.saturating_mul(track.duration_ms)
                        / u64::from(layout.progress_bar.width);
                    self.dispatch(AppEvent::SeekAbsolute(target));
                    self.sync_os_bridges(false);
                    return;
                }

                // Click or drag on Volume Bar -> Set Volume!
                if layout.volume_bar.contains(event.col, event.row) && layout.volume_bar.width > 0 {
                    let rel = u32::from(event.col.saturating_sub(layout.volume_bar.x));
                    let pct = (rel * 100) / u32::from(layout.volume_bar.width);
                    if let Ok(v) = u8::try_from(pct.min(100)) {
                        self.dispatch(AppEvent::SetVolume(v));
                        self.sync_os_bridges(false);
                    }
                    return;
                }

                if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
                    if let Some(sb) = layout.sidebar
                        && sb.contains(event.col, event.row)
                    {
                        self.dispatch(AppEvent::SetFocus(PanelFocus::Sidebar));
                        let inner_row = usize::from(event.row.saturating_sub(sb.y + 1));
                        self.state.sidebar_selected = inner_row;
                        self.clamp_selections();
                        self.activate_selection();
                    } else if layout.main.contains(event.col, event.row) {
                        self.dispatch(AppEvent::SetFocus(PanelFocus::Main));
                    } else if let Some(rs) = layout.right_sidebar
                        && rs.contains(event.col, event.row)
                    {
                        self.dispatch(AppEvent::SetFocus(PanelFocus::RightSidebar));
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.dispatch(AppEvent::OpenModal(ActiveModal::CommandPalette {
                    query: String::new(),
                    selected: 0,
                }));
            }
            _ => {}
        }
    }

    /// Handles an incoming local IPC command (`vynl status`, `vynl play-pause`, etc.).
    pub fn handle_ipc_command(&mut self, cmd: IpcCommand) -> IpcResponse {
        match cmd {
            IpcCommand::Status { .. } => {
                let snapshot = StatusSnapshot::from_playback_state(&self.state.playback);
                IpcResponse::with_status("ok", snapshot)
            }
            IpcCommand::PlayPause => {
                self.execute_key_action(KeyAction::TogglePlayback);
                IpcResponse::with_status(
                    if self.state.playback.is_playing {
                        "playing"
                    } else {
                        "paused"
                    },
                    StatusSnapshot::from_playback_state(&self.state.playback),
                )
            }
            IpcCommand::Next => {
                self.execute_key_action(KeyAction::NextTrack);
                IpcResponse::with_status(
                    "skipped to next track",
                    StatusSnapshot::from_playback_state(&self.state.playback),
                )
            }
            IpcCommand::Prev => {
                self.execute_key_action(KeyAction::PrevTrack);
                IpcResponse::with_status(
                    "skipped to previous track",
                    StatusSnapshot::from_playback_state(&self.state.playback),
                )
            }
            IpcCommand::VolumeDelta(delta) => {
                self.dispatch(AppEvent::AdjustVolume(delta));
                self.sync_os_bridges(false);
                IpcResponse::with_status(
                    format!("volume {}%", self.state.playback.volume_percent),
                    StatusSnapshot::from_playback_state(&self.state.playback),
                )
            }
            IpcCommand::OpenUri(uri) => {
                if let Some(track_id) = uri.strip_prefix("spotify:track:") {
                    if let Some(ref worker) = self.spotify_worker {
                        worker.send(BackendRequest::Command(
                            vynl_core::state::AppCommand::PlayTrackId(track_id.to_owned()),
                        ));
                    }
                } else if let Some(playlist_id) = uri.strip_prefix("spotify:playlist:") {
                    self.request_playlist_tracks(playlist_id);
                    self.dispatch(AppEvent::Navigate(Route::Playlist(playlist_id.to_owned())));
                    self.dispatch(AppEvent::SetFocus(PanelFocus::Main));
                } else if let Some(album_id) = uri.strip_prefix("spotify:album:") {
                    self.request_album_tracks(album_id);
                    self.dispatch(AppEvent::Navigate(Route::Album(album_id.to_owned())));
                    self.dispatch(AppEvent::SetFocus(PanelFocus::Main));
                } else if let Some(artist_id) = uri.strip_prefix("spotify:artist:") {
                    self.request_artist_top_tracks(artist_id);
                    self.dispatch(AppEvent::Navigate(Route::Artist(artist_id.to_owned())));
                    self.dispatch(AppEvent::SetFocus(PanelFocus::Main));
                }
                self.dispatch(AppEvent::ShowToast {
                    level: ToastLevel::Info,
                    text: format!("Opened {uri}"),
                });
                IpcResponse::ok(format!("opened {uri}"))
            }
        }
    }

    /// Advances playback interpolation, adaptive remote polling, and Zen spectrum bars.
    pub fn on_tick(&mut self, elapsed_ms: u64) {
        self.tick_counter = self.tick_counter.saturating_add(1);
        let prev_id = self
            .state
            .playback
            .current_track
            .as_ref()
            .map(|t| t.id.clone());
        self.dispatch(AppEvent::Tick { elapsed_ms });
        let next_id = self
            .state
            .playback
            .current_track
            .as_ref()
            .map(|t| t.id.clone());
        if prev_id != next_id {
            self.refresh_adaptive_theme();
            self.sync_os_bridges(true);
        }

        // Trigger adaptive remote playback sync when due (3–5s active, 20–30s paused).
        if let Some(ref worker) = self.spotify_worker
            && let Some(ref mut player) = self.player
        {
            let now = Instant::now();
            if player.sync().should_poll_at(now) {
                player.sync_mut().mark_polled_at(now);
                worker.send(BackendRequest::PollPlayback);
            }
        }

        // Feed real-time FFT spectrum bars from the integrated librespot player when
        // the `visualizer` feature is compiled and active; fall back to smooth animation.
        if !self.poll_live_spectrum() {
            if self.state.playback.is_playing {
                #[expect(clippy::cast_precision_loss)]
                let phase = (self.tick_counter as f32) * 0.35;
                for (i, bar) in self.spectrum_bars.iter_mut().enumerate() {
                    #[expect(clippy::cast_precision_loss)]
                    let freq = (i as f32) * 0.55 + phase;
                    *bar = (freq.sin().abs() * 0.75 + 0.15).clamp(0.0, 1.0);
                }
            } else {
                for bar in &mut self.spectrum_bars {
                    *bar *= 0.6;
                }
            }
        }
    }

    #[cfg(all(
        not(target_os = "freebsd"),
        any(feature = "rodio-backend", feature = "subprocess-backend")
    ))]
    fn poll_live_spectrum(&mut self) -> bool {
        if let Some(ref integrated) = self.integrated_player {
            let live_bars = integrated.spectrum_bars();
            if !live_bars.is_empty() {
                self.spectrum_bars = live_bars;
                return true;
            }
        }
        false
    }

    #[cfg(not(all(
        not(target_os = "freebsd"),
        any(feature = "rodio-backend", feature = "subprocess-backend")
    )))]
    const fn poll_live_spectrum(&self) -> bool {
        false
    }

    /// Extracts dominant colors from the active track cover and applies WCAG-AA accent.
    pub fn refresh_adaptive_theme(&mut self) {
        if !self.config.ui.adaptive_cover_colors {
            self.active_theme = self.base_theme.clone();
            return;
        }
        if let Some(ref track) = self.state.playback.current_track
            && let Some(ref cover) = track.cover
        {
            let img = generate_demo_cover(&cover.url, 48);
            let palette = img.extract_dominant_palette(4);
            self.active_theme = self.base_theme.with_adaptive_palette(&palette);
        } else {
            self.active_theme = self.base_theme.clone();
        }
    }

    fn sync_os_bridges(&mut self, notify_track_change: bool) {
        if let Some(ref mut player) = self.player {
            player.update_state(self.state.playback.clone());
        }
        if self.config.integrations.media_controls {
            self.media_bridge
                .sync_from_playback_state(&self.state.playback);
        }
        if notify_track_change
            && self.config.integrations.notifications
            && let Some(ref track) = self.state.playback.current_track
        {
            let icon_path = track.cover.as_ref().and_then(|cover| {
                let cache = self.disk_cache.as_ref()?;
                let dest = self.paths.image_cache_dir.join("notify-cover.png");
                cache
                    .export_png(&cover.url, &dest)
                    .ok()
                    .and_then(|exported| exported.then_some(dest))
            });
            let payload = NotificationPayload::from_track(track, icon_path);
            let _ = self.notifier.notify(&payload);
        }
    }

    /// Runs the full interactive TUI event loop with native `crossterm` raw mode,
    /// diff-based rendering, background Spotify/image/media polling, live local IPC
    /// socket server, and panic-safe terminal restoration.
    ///
    /// # Errors
    /// Returns an I/O error if terminal setup, input polling, or stdout writes fail.
    pub fn run_interactive(self) -> io::Result<()> {
        let mouse_enabled = self.config.ui.mouse_enabled;
        let ipc_enabled = self.config.integrations.ipc_socket;
        let socket_path = self.paths.ipc_socket_path.clone();

        let _guard = TerminalGuard::enter(mouse_enabled)?;

        let shared = Arc::new(Mutex::new(self));
        let _ipc_server = if ipc_enabled {
            let ipc_app = Arc::clone(&shared);
            IpcServer::bind(socket_path, move |cmd| {
                ipc_app.lock().map_or_else(
                    |_| IpcResponse::error("app lock poisoned"),
                    |mut app| app.handle_ipc_command(cmd),
                )
            })
            .ok()
        } else {
            None
        };

        let mut stdout = io::stdout();
        let mut prev_frame: Option<Buffer> = None;
        let mut last_tick = Instant::now();
        let tick_interval = Duration::from_millis(250);
        let (mut cols, mut rows) = query_terminal_size();

        loop {
            // Drain any queued native crossterm input events without blocking.
            while let Some(ev) = poll_terminal_event(Duration::ZERO)? {
                match ev {
                    TerminalInput::Resize(new_cols, new_rows) => {
                        if new_cols > 0 && new_rows > 0 {
                            cols = new_cols;
                            rows = new_rows;
                        }
                    }
                    TerminalInput::Key(k) => {
                        if let Ok(mut app) = shared.lock() {
                            app.handle_key(k);
                        }
                    }
                    TerminalInput::Mouse(m) => {
                        if let Ok(mut app) = shared.lock() {
                            app.handle_mouse(m, cols, rows);
                        }
                    }
                }
            }

            if let Ok(mut app) = shared.lock() {
                app.poll_background_workers();
            }

            if last_tick.elapsed() >= tick_interval {
                let elapsed_ms = u64::try_from(last_tick.elapsed().as_millis()).unwrap_or(250);
                last_tick = Instant::now();
                if let Ok(mut app) = shared.lock() {
                    app.on_tick(elapsed_ms);
                }
            }

            let (frame, should_quit) = match shared.lock() {
                Ok(mut app) => {
                    let frame = app.render_snapshot(cols, rows);
                    (frame, app.state.should_quit)
                }
                Err(_) => break,
            };

            let diff = frame.diff_ansi(prev_frame.as_ref());
            stdout.write_all(diff.as_bytes())?;
            stdout.flush()?;
            prev_frame = Some(frame);

            if should_quit {
                break;
            }

            // Sleep efficiently up to 50 ms waiting for the next native crossterm event.
            if let Some(ev) = poll_terminal_event(Duration::from_millis(50))? {
                match ev {
                    TerminalInput::Resize(new_cols, new_rows) => {
                        if new_cols > 0 && new_rows > 0 {
                            cols = new_cols;
                            rows = new_rows;
                        }
                    }
                    TerminalInput::Key(k) => {
                        if let Ok(mut app) = shared.lock() {
                            app.handle_key(k);
                        }
                    }
                    TerminalInput::Mouse(m) => {
                        if let Ok(mut app) = shared.lock() {
                            app.handle_mouse(m, cols, rows);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

fn spawn_http_image_worker(
    config: &AppConfig,
    paths: &AppPaths,
) -> (Option<ImageWorker>, Option<Arc<L2DiskCache>>) {
    if !config.images.enabled {
        return (None, None);
    }
    let Ok(http_source) = HttpImageSource::new() else {
        return (None, None);
    };
    let source: Arc<dyn ImageSource> = Arc::new(http_source);
    let max_bytes = u64::from(config.images.max_disk_cache_mb).saturating_mul(1024 * 1024);
    let disk_cache = L2DiskCache::new(paths.image_cache_dir.clone(), max_bytes)
        .ok()
        .map(Arc::new);
    let worker = ImageWorker::spawn(2, &source, disk_cache.as_ref()).ok();
    (worker, disk_cache)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use vynl_core::config::AppConfig;
    use vynl_core::keymap::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
    use vynl_core::state::{ActiveModal, LayoutBreakpoint, Route};
    use vynl_core::theme::Rgb;
    use vynl_images::ImageProtocol;
    use vynl_spotify::api::{HttpResponse, MockSpotifyTransport};
    use vynl_spotify::pkce::TokenBundle;
    use vynl_system::ipc::IpcCommand;
    use vynl_system::media::MediaControlEvent;
    use vynl_system::paths::{AppPaths, EnvSnapshot};

    use crate::spotify_worker::{BackendRequest, SpotifyWorkerHandle};
    use crate::storage;

    use super::VynlApp;

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn renders_all_four_breakpoints_without_panics() {
        let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);

        let wide = app.render_snapshot(150, 40);
        assert_eq!(app.state.breakpoint, LayoutBreakpoint::Wide);
        let wide_txt = wide.to_plain_text();
        assert!(wide_txt.contains("Library"));
        assert!(wide_txt.contains("Now Playing"));
        assert!(wide_txt.contains('▀')); // Real halfblock cover art rendered!

        let medium = app.render_snapshot(110, 32);
        assert_eq!(app.state.breakpoint, LayoutBreakpoint::Medium);
        assert!(medium.to_plain_text().contains("Library"));

        let compact = app.render_snapshot(72, 24);
        assert_eq!(app.state.breakpoint, LayoutBreakpoint::Compact);
        assert!(compact.to_plain_text().contains("Home"));

        let mini = app.render_snapshot(50, 12);
        assert_eq!(app.state.breakpoint, LayoutBreakpoint::MiniPlayer);
        assert!(mini.to_plain_text().contains("Instant Crush"));
    }

    #[test]
    fn modal_aware_image_occlusion_suppresses_overlapping_kitty_overlays() {
        let mut app = VynlApp::new_demo(ImageProtocol::Kitty);

        let without_modal = app.render_snapshot(150, 40);
        let overlays_open = without_modal.graphics_overlays().len();
        assert!(overlays_open > 0);

        // Open Command Palette over center of screen
        app.handle_key(KeyEvent::ctrl('p'));
        assert!(matches!(
            app.state.modal,
            Some(ActiveModal::CommandPalette { .. })
        ));
        let with_modal = app.render_snapshot(150, 40);
        assert!(with_modal.to_plain_text().contains("Command Palette"));
        // Overlapping center/hero slots must be suppressed while modal is open
        assert!(with_modal.graphics_overlays().len() < overlays_open);

        // Dismiss modal -> images return immediately from L1 cache
        app.handle_key(KeyEvent::plain(KeyCode::Esc));
        let restored = app.render_snapshot(150, 40);
        assert_eq!(restored.graphics_overlays().len(), overlays_open);
    }

    #[test]
    fn command_palette_theme_switch_and_ipc_commands_work() {
        let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);

        // Switch to Zen mode via shortcut
        app.handle_key(KeyEvent::plain(KeyCode::Char('z')));
        assert_eq!(app.state.route, Route::ZenNowPlaying);
        app.on_tick(250);
        let zen_frame = app.render_snapshot(140, 38);
        assert!(zen_frame.to_plain_text().contains("Zen Now Playing"));

        // IPC status & play-pause
        let status = app.handle_ipc_command(IpcCommand::Status { json: true });
        assert!(status.ok);
        assert!(status.to_json().contains("Instant Crush"));

        let toggled = app.handle_ipc_command(IpcCommand::PlayPause);
        assert!(toggled.ok);
        assert!(!app.state.playback.is_playing);

        // OS media control event resumes playback cleanly
        app.handle_media_control_event(MediaControlEvent::Play);
        assert!(app.state.playback.is_playing);

        // Mouse volume click
        let layout = crate::layout::FrameLayout::compute(150, 40, true);
        app.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                col: layout.volume_bar.x + layout.volume_bar.width / 2,
                row: layout.volume_bar.y,
            },
            150,
            40,
        );
        assert!(app.state.playback.volume_percent <= 60);
    }

    #[test]
    fn live_app_persists_custom_themes_and_dispatches_spotify_commands() {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("vynl-app-live-{}-{seq}", std::process::id()));
        let env = EnvSnapshot {
            xdg_config_home: Some(root.join("config")),
            xdg_cache_home: Some(root.join("cache")),
            xdg_state_home: Some(root.join("state")),
            xdg_runtime_dir: Some(root.join("run")),
            home: Some(root.clone()),
            appdata: None,
            local_appdata: None,
            temp_dir: root.join("tmp"),
        };
        let paths = AppPaths::from_env(&env, false);
        assert!(paths.ensure_dirs().is_ok());

        let custom_theme_toml = r##"
background = "#090a12"
surface = "#111322"
text_primary = "#f0f4ff"
text_muted = "#8892b0"
accent = "#00d9ff"
border = "#242844"
"##;
        assert!(fs::write(paths.themes_dir.join("ocean.toml"), custom_theme_toml).is_ok());

        let mut cfg = AppConfig::default();
        cfg.ui.theme = String::from("ocean");
        cfg.ui.adaptive_cover_colors = false;
        cfg.integrations.media_controls = false;
        cfg.integrations.notifications = false;

        let mut app = VynlApp::new_demo_with_config(cfg, paths.clone(), ImageProtocol::Halfblocks);
        assert_eq!(app.base_theme.name, "ocean");
        assert_eq!(app.active_theme.accent, Rgb::new(0x00, 0xd9, 0xff));
        assert!(app.custom_themes.contains(&String::from("ocean")));

        // Switching theme persists config.toml to disk
        app.switch_theme("nord");
        let saved = storage::load_config(&paths);
        assert!(saved.is_ok());
        if let Ok(reloaded) = saved {
            assert_eq!(reloaded.ui.theme, "nord");
        }

        // Attach a mock-backed SpotifyWorkerHandle and verify AppCommand dispatch
        let transport = MockSpotifyTransport::new();
        // me, liked_tracks, playlists, saved_albums, followed_artists, saved_episodes, recently_played, playback, devices, next_track
        transport.push_json(
            200,
            r#"{"id":"u42","display_name":"Grace","product":"premium"}"#,
        );
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_json(200, r#"{"artists":{"items":[]}}"#);
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_json(200, r#"{"items":[]}"#);
        transport.push_response(HttpResponse::new(204, ""));
        transport.push_json(200, r#"{"devices":[]}"#);
        transport.push_response(HttpResponse::new(204, ""));

        let tokens = TokenBundle {
            access_token: String::from("tok"),
            refresh_token: String::new(),
            expires_at_epoch_secs: u64::MAX / 2,
        };
        let worker_res = SpotifyWorkerHandle::spawn_with_transport(
            transport,
            String::from("cid"),
            tokens,
            None,
            Box::new(|_| {}),
        );
        assert!(worker_res.is_ok());
        if let Ok(worker) = worker_res {
            worker.send(BackendRequest::Bootstrap);
            app.attach_spotify_worker(worker);
            // Trigger an AppCommand via keybinding (NextTrack)
            app.handle_key(KeyEvent::plain(KeyCode::Char('n')));

            for _ in 0..30 {
                app.poll_background_workers();
                if app
                    .state
                    .user
                    .as_ref()
                    .is_some_and(|u| u.display_name == "Grace")
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            assert_eq!(
                app.state.user.as_ref().map(|u| u.display_name.as_str()),
                Some("Grace")
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sidebar_library_filter_and_ipc_open_uri_navigate_albums_artists_and_playlists() {
        let mut app = VynlApp::new_demo(ImageProtocol::Halfblocks);

        // Cycle Sidebar filter: All -> Playlists -> Albums
        app.state.focus = vynl_core::state::PanelFocus::Sidebar;
        app.handle_key(KeyEvent::plain(KeyCode::Char('f')));
        assert_eq!(
            app.state.library_filter,
            vynl_core::model::LibraryFilter::Playlists
        );
        app.handle_key(KeyEvent::plain(KeyCode::Char('f')));
        assert_eq!(
            app.state.library_filter,
            vynl_core::model::LibraryFilter::Albums
        );

        // Select first album in sidebar (index 5) and press Enter -> navigates to Route::Album
        app.state.sidebar_selected = 5;
        app.handle_key(KeyEvent::plain(KeyCode::Enter));
        assert!(matches!(app.state.route, Route::Album(_)));
        assert_ne!(app.active_view_tracks().len(), 0);

        // IPC OpenUri routes spotify:album:, spotify:artist:, spotify:playlist:
        let resp_artist = app.handle_ipc_command(IpcCommand::OpenUri(String::from(
            "spotify:artist:art-daft-punk",
        )));
        assert!(resp_artist.ok);
        assert_eq!(
            app.state.route,
            Route::Artist(String::from("art-daft-punk"))
        );

        let resp_playlist = app.handle_ipc_command(IpcCommand::OpenUri(String::from(
            "spotify:playlist:pl-late-night",
        )));
        assert!(resp_playlist.ok);
        assert_eq!(
            app.state.route,
            Route::Playlist(String::from("pl-late-night"))
        );
    }
}
