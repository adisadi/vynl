//! Interactive `vynl` TUI application controller wiring `vynl-core`, `vynl-images`,
//! `vynl-spotify`, `vynl-player`, and `vynl-system` into a unified event loop.

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use vynl_core::config::AppConfig;
use vynl_core::keymap::{
    KeyAction, KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind, resolve_hybrid_key,
};
use vynl_core::model::{ToastLevel, TrackSummary};
use vynl_core::state::{ActiveModal, AppEvent, AppState, PanelFocus, Route, reduce};
use vynl_core::theme::Theme;
use vynl_images::{ImageProtocol, L1ImageCache, OcclusionTracker, generate_demo_cover};
use vynl_player::PlaybackEngine;
use vynl_spotify::demo::DemoCatalog;
use vynl_system::ipc::{IpcCommand, IpcResponse, IpcServer, StatusSnapshot};
use vynl_system::media::LocalMediaBridge;
use vynl_system::notify::{LocalOsNotifier, NotificationDispatcher, NotificationPayload};
use vynl_system::paths::AppPaths;

use crate::buffer::Buffer;
use crate::input::{TerminalGuard, TerminalInput, parse_input_bytes, query_terminal_size};
use crate::layout::FrameLayout;
use crate::render::{RenderContext, filter_palette_commands, render_frame};

/// Live stateful application runtime owning UI state, caches, playback engine,
/// OS integrations, and demo/Spotify catalog.
pub struct VynlApp {
    /// Application configuration.
    pub config: AppConfig,
    /// Synchronous Elm UI state.
    pub state: AppState,
    /// Base theme before adaptive cover accent override.
    pub base_theme: Theme,
    /// Active theme (with adaptive cover accent applied when enabled).
    pub active_theme: Theme,
    /// Resolved terminal graphics protocol.
    pub protocol: ImageProtocol,
    /// L1 RAM LRU cache for encoded terminal images and palettes.
    pub l1_cache: L1ImageCache,
    /// Per-frame Modal-Aware Image Occlusion tracker.
    pub occlusion: OcclusionTracker,
    /// Offline demo catalog (used in `--demo` and fallback browsing).
    pub demo: DemoCatalog,
    /// Dual playback engine & stream cache coordinator.
    pub player: Option<PlaybackEngine>,
    /// Local OS media control metadata bridge (MPRIS2 / SMTC / `MediaRemote`).
    pub media_bridge: LocalMediaBridge,
    /// Local OS desktop notifier.
    pub notifier: LocalOsNotifier,
    /// Resolved OS/XDG application paths.
    pub paths: AppPaths,
    /// Latest spectrum bars for Zen-mode FFT visualization.
    pub spectrum_bars: Vec<f32>,
    /// Tick counter for deterministic demo spectrum animation.
    tick_counter: u64,
}

impl VynlApp {
    /// Creates a new `vynl` application initialized with the offline demo catalog
    /// (ideal for `--demo`, `--render-demo`, and headless snapshot tests).
    #[must_use]
    pub fn new_demo(protocol: ImageProtocol) -> Self {
        let config = AppConfig::default();
        let demo = DemoCatalog::builtin();
        let base_theme = Theme::builtin(&config.ui.theme).unwrap_or_default();
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

        let paths = AppPaths::resolve();
        let player =
            PlaybackEngine::from_config(&config.spotify, paths.audio_cache_dir.clone()).ok();

        let mut app = Self {
            l1_cache: L1ImageCache::new(config.images.max_memory_cache_items),
            occlusion: OcclusionTracker::new(),
            active_theme: base_theme.clone(),
            base_theme,
            protocol,
            state,
            demo,
            player,
            media_bridge: LocalMediaBridge::new(),
            notifier: LocalOsNotifier::new().with_enabled(config.integrations.notifications),
            paths,
            config,
            spectrum_bars: vec![0.0; 16],
            tick_counter: 0,
        };
        app.refresh_adaptive_theme();
        app.sync_os_bridges(false);
        app
    }

    /// Computes the list of tracks currently visible for the active route + inline filter.
    #[must_use]
    pub fn active_view_tracks(&self) -> Vec<TrackSummary> {
        let base: Vec<TrackSummary> = match &self.state.route {
            Route::Home | Route::LikedSongs => self.state.library.liked_tracks.clone(),
            Route::Playlist(id) => self.demo.tracks_for_playlist(id),
            Route::Album(id) => self
                .state
                .library
                .albums
                .iter()
                .find(|a| a.id == *id)
                .map_or_else(Vec::new, |a| self.demo.tracks_for_album(&a.title)),
            Route::Artist(id) => self
                .state
                .library
                .artists
                .iter()
                .find(|a| a.id == *id)
                .map_or_else(Vec::new, |artist| {
                    self.demo
                        .tracks
                        .iter()
                        .filter(|t| t.artists.iter().any(|n| n == &artist.name))
                        .cloned()
                        .collect()
                }),
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
        let _ = reduce(&mut self.state, AppEvent::Resize { cols, rows });
        let view_tracks = self.active_view_tracks();
        let mut ctx = RenderContext {
            state: &self.state,
            theme: &self.active_theme,
            protocol: self.protocol,
            images_enabled: self.config.images.enabled,
            view_tracks: &view_tracks,
            spectrum_bars: &self.spectrum_bars,
            l1_cache: &mut self.l1_cache,
            occlusion: &mut self.occlusion,
        };
        render_frame(cols, rows, &mut ctx)
    }

    /// Processes a single decoded keyboard event (handling active modals, inline filters,
    /// search input, and hybrid global keybindings).
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key == KeyEvent::ctrl('c') {
            let _ = reduce(&mut self.state, AppEvent::Quit);
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
                    let _ = reduce(&mut self.state, AppEvent::SetInlineFilter(None));
                    return;
                }
                KeyCode::Enter => return,
                KeyCode::Backspace => {
                    filter.pop();
                    let _ = reduce(&mut self.state, AppEvent::SetInlineFilter(Some(filter)));
                    return;
                }
                KeyCode::Char(c) if !key.modifiers.ctrl && !key.modifiers.alt => {
                    filter.push(c);
                    let _ = reduce(&mut self.state, AppEvent::SetInlineFilter(Some(filter)));
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
            let _ = reduce(&mut self.state, AppEvent::DismissModal);
            return;
        }

        match modal {
            ActiveModal::CommandPalette {
                mut query,
                mut selected,
            } => {
                let matches = filter_palette_commands(&query);
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
                            let id = cmd.id;
                            self.state.modal = None;
                            self.execute_palette_command(id);
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
                        let _ = reduce(&mut self.state, AppEvent::SelectDevice(selected));
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
                        if let Some(pl) = self.state.library.playlists.get_mut(selected) {
                            let added = u32::try_from(track_ids.len()).unwrap_or(1);
                            pl.track_count = pl.track_count.saturating_add(added);
                            let name = pl.name.clone();
                            self.state.modal = None;
                            let _ = reduce(
                                &mut self.state,
                                AppEvent::ShowToast {
                                    level: ToastLevel::Info,
                                    text: format!("Added {added} track(s) to {name}"),
                                },
                            );
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
                    if !client_id_input.trim().is_empty() {
                        self.config.spotify.client_id = Some(client_id_input.trim().to_owned());
                    }
                    self.state.modal = None;
                    let _ = reduce(
                        &mut self.state,
                        AppEvent::ShowToast {
                            level: ToastLevel::Info,
                            text: String::from("Spotify Client ID saved"),
                        },
                    );
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
                let _ = reduce(&mut self.state, AppEvent::MoveSelection(-1));
            }
            KeyAction::MoveDown => {
                let _ = reduce(&mut self.state, AppEvent::MoveSelection(1));
                self.clamp_selections();
            }
            KeyAction::FocusLeft => {
                let _ = reduce(&mut self.state, AppEvent::SetFocus(PanelFocus::Sidebar));
            }
            KeyAction::FocusRight => {
                let _ = reduce(&mut self.state, AppEvent::SetFocus(PanelFocus::Main));
            }
            KeyAction::JumpTop => {
                let _ = reduce(&mut self.state, AppEvent::JumpSelection { top: true });
            }
            KeyAction::JumpBottom => {
                let _ = reduce(&mut self.state, AppEvent::JumpSelection { top: false });
                self.clamp_selections();
            }
            KeyAction::NextPanel => {
                let _ = reduce(&mut self.state, AppEvent::FocusNextPanel);
            }
            KeyAction::PrevPanel => {
                let _ = reduce(&mut self.state, AppEvent::FocusPrevPanel);
            }
            KeyAction::Select => self.activate_selection(),
            KeyAction::TogglePlayback => {
                let _ = reduce(&mut self.state, AppEvent::TogglePlayback);
            }
            KeyAction::NextTrack => {
                let _ = reduce(&mut self.state, AppEvent::NextTrack);
            }
            KeyAction::PrevTrack => {
                let _ = reduce(&mut self.state, AppEvent::PrevTrack);
            }
            KeyAction::ToggleShuffle => {
                let _ = reduce(&mut self.state, AppEvent::ToggleShuffle);
            }
            KeyAction::CycleRepeat => {
                let _ = reduce(&mut self.state, AppEvent::CycleRepeat);
            }
            KeyAction::VolumeUp => {
                let _ = reduce(&mut self.state, AppEvent::AdjustVolume(5));
            }
            KeyAction::VolumeDown => {
                let _ = reduce(&mut self.state, AppEvent::AdjustVolume(-5));
            }
            KeyAction::ToggleMute => {
                let _ = reduce(&mut self.state, AppEvent::ToggleMute);
            }
            KeyAction::SeekForward => {
                let _ = reduce(&mut self.state, AppEvent::SeekRelative(5_000));
            }
            KeyAction::SeekBackward => {
                let _ = reduce(&mut self.state, AppEvent::SeekRelative(-5_000));
            }
            KeyAction::OpenSearch => {
                let _ = reduce(&mut self.state, AppEvent::Navigate(Route::Search));
                let _ = reduce(&mut self.state, AppEvent::SetFocus(PanelFocus::Main));
            }
            KeyAction::OpenInlineFilter => {
                let _ = reduce(
                    &mut self.state,
                    AppEvent::SetInlineFilter(Some(String::new())),
                );
            }
            KeyAction::OpenCommandPalette => {
                let _ = reduce(
                    &mut self.state,
                    AppEvent::OpenModal(ActiveModal::CommandPalette {
                        query: String::new(),
                        selected: 0,
                    }),
                );
            }
            KeyAction::ToggleLike => {
                let _ = reduce(&mut self.state, AppEvent::ToggleCurrentLike);
            }
            KeyAction::AddToQueue => {
                let tracks = self.active_view_tracks();
                if let Some(track) = tracks.get(self.state.main_selected).cloned() {
                    let _ = reduce(&mut self.state, AppEvent::EnqueueTrack(track));
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
                    let _ = reduce(
                        &mut self.state,
                        AppEvent::OpenModal(ActiveModal::AddToPlaylist {
                            track_ids: ids,
                            selected: 0,
                        }),
                    );
                }
            }
            KeyAction::ToggleMultiSelect => {
                let idx = self.state.main_selected;
                let _ = reduce(&mut self.state, AppEvent::ToggleMultiSelect(idx));
            }
            KeyAction::OpenDevicePicker => {
                let _ = reduce(
                    &mut self.state,
                    AppEvent::OpenModal(ActiveModal::DevicePicker { selected: 0 }),
                );
            }
            KeyAction::ToggleQueue => {
                let _ = reduce(&mut self.state, AppEvent::ToggleQueuePanel);
            }
            KeyAction::ToggleZenMode => {
                let _ = reduce(&mut self.state, AppEvent::ToggleZenMode);
            }
            KeyAction::NavigateBack => {
                let _ = reduce(&mut self.state, AppEvent::NavigateBack);
            }
            KeyAction::OpenHelp => {
                let _ = reduce(&mut self.state, AppEvent::OpenModal(ActiveModal::Help));
            }
            KeyAction::Dismiss => {
                let _ = reduce(&mut self.state, AppEvent::DismissModal);
            }
            KeyAction::Quit => {
                let _ = reduce(&mut self.state, AppEvent::Quit);
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
        match id {
            "play_pause" => self.execute_key_action(KeyAction::TogglePlayback),
            "next_track" => self.execute_key_action(KeyAction::NextTrack),
            "prev_track" => self.execute_key_action(KeyAction::PrevTrack),
            "toggle_shuffle" => self.execute_key_action(KeyAction::ToggleShuffle),
            "cycle_repeat" => self.execute_key_action(KeyAction::CycleRepeat),
            "device_picker" => self.execute_key_action(KeyAction::OpenDevicePicker),
            "zen_mode" => self.execute_key_action(KeyAction::ToggleZenMode),
            "toggle_queue" => self.execute_key_action(KeyAction::ToggleQueue),
            "go_home" => {
                let _ = reduce(&mut self.state, AppEvent::Navigate(Route::Home));
            }
            "go_search" => self.execute_key_action(KeyAction::OpenSearch),
            "go_liked" => {
                let _ = reduce(&mut self.state, AppEvent::Navigate(Route::LikedSongs));
            }
            "go_podcasts" => {
                let _ = reduce(&mut self.state, AppEvent::Navigate(Route::Podcasts));
            }
            "theme_vynl_dark" => self.switch_theme("vynl-dark"),
            "theme_catppuccin" => self.switch_theme("catppuccin-mocha"),
            "theme_tokyo_night" => self.switch_theme("tokyo-night"),
            "theme_gruvbox" => self.switch_theme("gruvbox"),
            "theme_nord" => self.switch_theme("nord"),
            "toggle_adaptive" => {
                self.config.ui.adaptive_cover_colors = !self.config.ui.adaptive_cover_colors;
                self.refresh_adaptive_theme();
                let _ = reduce(
                    &mut self.state,
                    AppEvent::ShowToast {
                        level: ToastLevel::Info,
                        text: format!(
                            "Adaptive cover colors: {}",
                            if self.config.ui.adaptive_cover_colors {
                                "ON"
                            } else {
                                "OFF"
                            }
                        ),
                    },
                );
            }
            "open_help" => self.execute_key_action(KeyAction::OpenHelp),
            "quit" => self.execute_key_action(KeyAction::Quit),
            _ => {}
        }
    }

    fn switch_theme(&mut self, name: &str) {
        if let Some(theme) = Theme::builtin(name) {
            self.base_theme = theme;
            self.refresh_adaptive_theme();
            let _ = reduce(
                &mut self.state,
                AppEvent::ShowToast {
                    level: ToastLevel::Info,
                    text: format!("Theme switched to {name}"),
                },
            );
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
                        let pl_idx = idx.saturating_sub(5);
                        if let Some(pl) = self.state.library.playlists.get(pl_idx) {
                            Route::Playlist(pl.id.clone())
                        } else {
                            return;
                        }
                    }
                };
                let _ = reduce(&mut self.state, AppEvent::Navigate(route));
                let _ = reduce(&mut self.state, AppEvent::SetFocus(PanelFocus::Main));
            }
            PanelFocus::Main | PanelFocus::RightSidebar => {
                let tracks = self.active_view_tracks();
                if let Some(track) = tracks.get(self.state.main_selected).cloned() {
                    let _ = reduce(&mut self.state, AppEvent::PlayTrack(track));
                    self.refresh_adaptive_theme();
                    self.sync_os_bridges(true);
                }
            }
        }
    }

    fn clamp_selections(&mut self) {
        let sidebar_len = 5 + self.state.library.playlists.len();
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
        self.state.search_results = self.demo.search(&query);
        let _ = reduce(&mut self.state, AppEvent::SetSearchQuery(query));
    }

    /// Processes a decoded SGR mouse event (click, scroll wheel, drag scrubbing).
    pub fn handle_mouse(&mut self, event: MouseEvent, cols: u16, rows: u16) {
        if !self.config.ui.mouse_enabled {
            return;
        }
        let layout = FrameLayout::compute(cols, rows, self.state.show_right_sidebar);

        match event.kind {
            MouseEventKind::ScrollUp => {
                let _ = reduce(&mut self.state, AppEvent::MoveSelection(-1));
            }
            MouseEventKind::ScrollDown => {
                let _ = reduce(&mut self.state, AppEvent::MoveSelection(1));
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
                    let _ = reduce(&mut self.state, AppEvent::SeekAbsolute(target));
                    return;
                }

                // Click or drag on Volume Bar -> Set Volume!
                if layout.volume_bar.contains(event.col, event.row) && layout.volume_bar.width > 0 {
                    let rel = u32::from(event.col.saturating_sub(layout.volume_bar.x));
                    let pct = (rel * 100) / u32::from(layout.volume_bar.width);
                    if let Ok(v) = u8::try_from(pct.min(100)) {
                        let _ = reduce(&mut self.state, AppEvent::SetVolume(v));
                    }
                    return;
                }

                if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
                    if let Some(sb) = layout.sidebar
                        && sb.contains(event.col, event.row)
                    {
                        let _ = reduce(&mut self.state, AppEvent::SetFocus(PanelFocus::Sidebar));
                        let inner_row = usize::from(event.row.saturating_sub(sb.y + 1));
                        self.state.sidebar_selected = inner_row;
                        self.clamp_selections();
                        self.activate_selection();
                    } else if layout.main.contains(event.col, event.row) {
                        let _ = reduce(&mut self.state, AppEvent::SetFocus(PanelFocus::Main));
                    } else if let Some(rs) = layout.right_sidebar
                        && rs.contains(event.col, event.row)
                    {
                        let _ = reduce(
                            &mut self.state,
                            AppEvent::SetFocus(PanelFocus::RightSidebar),
                        );
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                let _ = reduce(
                    &mut self.state,
                    AppEvent::OpenModal(ActiveModal::CommandPalette {
                        query: String::new(),
                        selected: 0,
                    }),
                );
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
                let _ = reduce(&mut self.state, AppEvent::AdjustVolume(delta));
                IpcResponse::with_status(
                    format!("volume {}%", self.state.playback.volume_percent),
                    StatusSnapshot::from_playback_state(&self.state.playback),
                )
            }
            IpcCommand::OpenUri(uri) => {
                let _ = reduce(
                    &mut self.state,
                    AppEvent::ShowToast {
                        level: ToastLevel::Info,
                        text: format!("Opened {uri}"),
                    },
                );
                IpcResponse::ok(format!("opened {uri}"))
            }
        }
    }

    /// Advances playback interpolation and Zen spectrum bars on periodic ticks.
    pub fn on_tick(&mut self, elapsed_ms: u64) {
        self.tick_counter = self.tick_counter.saturating_add(1);
        let prev_id = self
            .state
            .playback
            .current_track
            .as_ref()
            .map(|t| t.id.clone());
        let _ = reduce(&mut self.state, AppEvent::Tick { elapsed_ms });
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
            let payload = NotificationPayload::from_track(track, None);
            let _ = self.notifier.notify(&payload);
        }
    }

    /// Runs the full interactive TUI event loop with raw mode, diff-based rendering,
    /// live local IPC socket server, and panic-safe terminal restoration.
    ///
    /// # Errors
    /// Returns an I/O error if terminal setup or stdout writes fail.
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

        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut stdin = io::stdin();
            let mut buf = [0u8; 64];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if input_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let mut stdout = io::stdout();
        let mut prev_frame: Option<Buffer> = None;
        let mut last_tick = Instant::now();
        let tick_interval = Duration::from_millis(250);

        loop {
            let (cols, rows) = query_terminal_size();

            // Drain any pending terminal input bytes
            while let Ok(bytes) = input_rx.try_recv() {
                let events = parse_input_bytes(&bytes);
                if let Ok(mut app) = shared.lock() {
                    for ev in events {
                        match ev {
                            TerminalInput::Key(k) => app.handle_key(k),
                            TerminalInput::Mouse(m) => app.handle_mouse(m, cols, rows),
                        }
                    }
                }
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

            // Wait briefly for next input chunk to keep idle CPU < 1%
            if let Ok(bytes) = input_rx.recv_timeout(Duration::from_millis(50)) {
                let events = parse_input_bytes(&bytes);
                if let Ok(mut app) = shared.lock() {
                    for ev in events {
                        match ev {
                            TerminalInput::Key(k) => app.handle_key(k),
                            TerminalInput::Mouse(m) => app.handle_mouse(m, cols, rows),
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use vynl_core::keymap::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
    use vynl_core::state::{ActiveModal, LayoutBreakpoint, Route};
    use vynl_images::ImageProtocol;
    use vynl_system::ipc::IpcCommand;

    use super::VynlApp;

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
}
