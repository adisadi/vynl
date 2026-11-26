//! Elm-style (TEA) application state, responsive breakpoints, and pure reducer.

use std::collections::BTreeSet;

use crate::model::{
    ConnectDevice, LibraryFilter, LibraryState, PlaybackState, QueueState, SearchCategory,
    SearchResults, ToastLevel, ToastMessage, TrackSummary, UserProfile,
};

/// Responsive layout tier derived from terminal dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutBreakpoint {
    /// Wide 3-column desktop layout (`>= 140` cols).
    Wide,
    /// Medium 2-column layout (`90..=139` cols).
    Medium,
    /// Compact single-pane layout (`60..=89` cols).
    Compact,
    /// Mini-player mode (`< 60` cols or `< 15` rows).
    MiniPlayer,
}

impl LayoutBreakpoint {
    /// Computes the responsive layout tier from terminal columns and rows.
    #[must_use]
    pub const fn from_terminal_size(cols: u16, rows: u16) -> Self {
        if cols < 60 || rows < 15 {
            Self::MiniPlayer
        } else if cols < 90 {
            Self::Compact
        } else if cols < 140 {
            Self::Medium
        } else {
            Self::Wide
        }
    }
}

/// Active focus region in the 3-column desktop layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelFocus {
    /// Left navigation & library sidebar.
    Sidebar,
    /// Central content area.
    #[default]
    Main,
    /// Right Now Playing / Queue sidebar.
    RightSidebar,
}

impl PanelFocus {
    /// Cycles focus forward (`Tab`).
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Sidebar => Self::Main,
            Self::Main => Self::RightSidebar,
            Self::RightSidebar => Self::Sidebar,
        }
    }

    /// Cycles focus backward (`Shift+Tab`).
    #[must_use]
    pub const fn prev(self) -> Self {
        match self {
            Self::Sidebar => Self::RightSidebar,
            Self::Main => Self::Sidebar,
            Self::RightSidebar => Self::Main,
        }
    }
}

/// Right sidebar tab selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RightSidebarTab {
    /// Hero cover art + current track/artist info + Next in Queue preview.
    #[default]
    NowPlaying,
    /// Full Queue & Recently Played list.
    Queue,
}

/// Active navigation view in the main content area.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    /// Home dashboard with recently played cover grid.
    #[default]
    Home,
    /// Instant search across Spotify entities.
    Search,
    /// Saved Liked Songs view.
    LikedSongs,
    /// Podcasts & episodes view.
    Podcasts,
    /// Full-screen Zen Now Playing mode with large cover art & optional FFT visualizer.
    ZenNowPlaying,
    /// Album detail view by Spotify ID.
    Album(String),
    /// Playlist detail view by Spotify ID.
    Playlist(String),
    /// Artist detail view by Spotify ID.
    Artist(String),
}

/// Floating modal overlay active on top of the main layout.
///
/// Any active modal triggers **Modal-Aware Image Occlusion** in `vynl-images`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveModal {
    /// Fuzzy Command Palette (`Ctrl+P` or `:`).
    CommandPalette {
        /// Current fuzzy query string.
        query: String,
        /// Highlighted command index.
        selected: usize,
    },
    /// Spotify Connect Device Picker (`d`).
    DevicePicker {
        /// Highlighted device index.
        selected: usize,
    },
    /// Add selected track(s) to a playlist (`P`).
    AddToPlaylist {
        /// Target track IDs to add.
        track_ids: Vec<String>,
        /// Highlighted playlist index.
        selected: usize,
    },
    /// Keybinding help overlay (`?`).
    Help,
    /// First-run BYO Spotify Developer `client_id` setup wizard.
    SetupWizard {
        /// In-progress `client_id` text input.
        client_id_input: String,
    },
}

/// Pure UI input/domain events consumed by [`reduce`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppEvent {
    /// Terminal resized to `(cols, rows)`.
    Resize {
        /// Terminal width in character cells.
        cols: u16,
        /// Terminal height in character cells.
        rows: u16,
    },
    /// Periodic tick (`tick_rate_ms`) advancing playback interpolation and toast TTLs.
    Tick {
        /// Elapsed milliseconds since previous tick.
        elapsed_ms: u64,
    },
    /// Navigate to a new route, pushing the current route onto history.
    Navigate(Route),
    /// Navigate backward in history (`Alt+Left` / `Backspace`).
    NavigateBack,
    /// Change active panel focus.
    SetFocus(PanelFocus),
    /// Cycle panel focus forward (`Tab`).
    FocusNextPanel,
    /// Cycle panel focus backward (`Shift+Tab`).
    FocusPrevPanel,
    /// Move list cursor up/down in the currently focused panel.
    MoveSelection(isize),
    /// Jump cursor to top or bottom of the focused list.
    JumpSelection {
        /// `true` for top (`g`), `false` for bottom (`G`).
        top: bool,
    },
    /// Cycle the sidebar library filter pill.
    CycleLibraryFilter,
    /// Toggle Queue vs Now Playing in the right sidebar (`q`).
    ToggleQueuePanel,
    /// Toggle Zen Fullscreen Now Playing (`z`).
    ToggleZenMode,
    /// Update search query string.
    SetSearchQuery(String),
    /// Switch active Search category tab.
    SetSearchCategory(SearchCategory),
    /// Update inline track-list filter query (`f`).
    SetInlineFilter(Option<String>),
    /// Toggle multi-select mark on the currently highlighted track index.
    ToggleMultiSelect(usize),
    /// Start playing a concrete track immediately (optimistic UI).
    PlayTrack(TrackSummary),
    /// Toggle play/pause optimistically.
    TogglePlayback,
    /// Skip to next track.
    NextTrack,
    /// Skip to previous track.
    PrevTrack,
    /// Seek relative by signed milliseconds.
    SeekRelative(i64),
    /// Seek to absolute position in milliseconds (mouse scrub).
    SeekAbsolute(u64),
    /// Cycle repeat mode (`Off -> Context -> Track`).
    CycleRepeat,
    /// Toggle shuffle mode.
    ToggleShuffle,
    /// Adjust volume by a signed delta (`-100..=100`).
    AdjustVolume(i8),
    /// Set absolute volume (`0..=100`, e.g., mouse click on volume bar).
    SetVolume(u8),
    /// Toggle output mute.
    ToggleMute,
    /// Toggle like on the current track (optimistic UI).
    ToggleCurrentLike,
    /// Add a track to the explicit playback queue.
    EnqueueTrack(TrackSummary),
    /// Switch active Spotify Connect device by index.
    SelectDevice(usize),
    /// Open or toggle a floating modal.
    OpenModal(ActiveModal),
    /// Dismiss any open modal (`Esc`).
    DismissModal,
    /// Display a transient status toast.
    ShowToast {
        /// Severity level.
        level: ToastLevel,
        /// Short English message.
        text: String,
    },
    /// Request graceful shutdown.
    Quit,
}

/// Side-effect commands returned by [`reduce`] for background workers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// Start playback of a specific track ID.
    PlayTrackId(String),
    /// Send play/pause to the active player or Connect backend.
    SetPlaying(bool),
    /// Skip to next track on backend.
    NextTrack,
    /// Skip to previous track on backend.
    PrevTrack,
    /// Seek to absolute position in milliseconds.
    SeekMs(u64),
    /// Update Spotify shuffle state.
    SetShuffle(bool),
    /// Update Spotify repeat state (`"off"`, `"context"`, `"track"`).
    SetRepeat(&'static str),
    /// Update volume percentage (`0..=100`).
    SetVolume(u8),
    /// Persist like/unlike for track ID.
    SetTrackLiked {
        /// Spotify track ID.
        track_id: String,
        /// Desired saved state.
        liked: bool,
    },
    /// Queue a track ID on Spotify Connect.
    EnqueueTrackId(String),
    /// Transfer playback to a Spotify Connect device ID.
    TransferDevice(String),
    /// Trigger debounced remote search.
    ExecuteSearch(String),
}

/// Root synchronous state owned by the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    /// Current user profile (populated after auth or in demo mode).
    pub user: Option<UserProfile>,
    /// Current main view route.
    pub route: Route,
    /// Back-navigation stack.
    pub history: Vec<Route>,
    /// Active focus panel.
    pub focus: PanelFocus,
    /// Active responsive layout tier.
    pub breakpoint: LayoutBreakpoint,
    /// Whether the right sidebar is toggled visible.
    pub show_right_sidebar: bool,
    /// Active right sidebar tab.
    pub right_tab: RightSidebarTab,
    /// Active library sidebar filter pill.
    pub library_filter: LibraryFilter,
    /// Selected index in the sidebar list.
    pub sidebar_selected: usize,
    /// Selected index in the main content list/grid.
    pub main_selected: usize,
    /// Selected indices for batch playlist operations.
    pub multi_selected: BTreeSet<usize>,
    /// Active inline filter query (`Some("")` while typing).
    pub inline_filter: Option<String>,
    /// Current global search query.
    pub search_query: String,
    /// Active search category tab.
    pub search_category: SearchCategory,
    /// Latest search results.
    pub search_results: SearchResults,
    /// User library collections.
    pub library: LibraryState,
    /// Available Spotify Connect devices.
    pub devices: Vec<ConnectDevice>,
    /// Playback queue & recently played history.
    pub queue: QueueState,
    /// Currently visible floating modal (triggers Modal-Aware Image Occlusion).
    pub modal: Option<ActiveModal>,
    /// Current playback state snapshot.
    pub playback: PlaybackState,
    /// Active transient status toast.
    pub toast: Option<ToastMessage>,
    /// Signals the event loop to exit cleanly.
    pub should_quit: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            user: None,
            route: Route::Home,
            history: Vec::new(),
            focus: PanelFocus::Main,
            breakpoint: LayoutBreakpoint::Wide,
            show_right_sidebar: true,
            right_tab: RightSidebarTab::NowPlaying,
            library_filter: LibraryFilter::All,
            sidebar_selected: 0,
            main_selected: 0,
            multi_selected: BTreeSet::new(),
            inline_filter: None,
            search_query: String::new(),
            search_category: SearchCategory::All,
            search_results: SearchResults::default(),
            library: LibraryState::default(),
            devices: Vec::new(),
            queue: QueueState::default(),
            modal: None,
            playback: PlaybackState {
                volume_percent: 80,
                speed_percent: 100,
                ..PlaybackState::default()
            },
            toast: None,
            should_quit: false,
        }
    }
}

/// Pure state transition function (Elm architecture).
#[expect(clippy::too_many_lines)]
pub fn reduce(state: &mut AppState, event: AppEvent) -> Vec<AppCommand> {
    let mut commands = Vec::new();

    match event {
        AppEvent::Resize { cols, rows } => {
            state.breakpoint = LayoutBreakpoint::from_terminal_size(cols, rows);
        }
        AppEvent::Tick { elapsed_ms } => {
            if state.playback.is_playing
                && let Some(ref track) = state.playback.current_track
            {
                let next_pos = state.playback.position_ms.saturating_add(elapsed_ms);
                if next_pos >= track.duration_ms && track.duration_ms > 0 {
                    state.playback.position_ms = 0;
                    if let Some(next_track) = state.queue.up_next.first().cloned() {
                        state.queue.up_next.remove(0);
                        let prev = state.playback.current_track.replace(next_track.clone());
                        if let Some(finished) = prev {
                            state.queue.recently_played.insert(0, finished);
                        }
                        commands.push(AppCommand::PlayTrackId(next_track.id));
                    }
                } else {
                    state.playback.position_ms = next_pos;
                }
            }
            if let Some(ref mut toast) = state.toast {
                toast.ttl_ticks = toast.ttl_ticks.saturating_sub(1);
                if toast.ttl_ticks == 0 {
                    state.toast = None;
                }
            }
        }
        AppEvent::Navigate(next_route) => {
            if state.route != next_route {
                let previous = std::mem::replace(&mut state.route, next_route);
                state.history.push(previous);
                state.main_selected = 0;
                state.multi_selected.clear();
                state.inline_filter = None;
            }
        }
        AppEvent::NavigateBack => {
            if let Some(previous) = state.history.pop() {
                state.route = previous;
                state.main_selected = 0;
                state.multi_selected.clear();
            }
        }
        AppEvent::SetFocus(focus) => state.focus = focus,
        AppEvent::FocusNextPanel => state.focus = state.focus.next(),
        AppEvent::FocusPrevPanel => state.focus = state.focus.prev(),
        AppEvent::MoveSelection(delta) => {
            let cursor = match state.focus {
                PanelFocus::Sidebar => &mut state.sidebar_selected,
                PanelFocus::Main | PanelFocus::RightSidebar => &mut state.main_selected,
            };
            if delta < 0 {
                *cursor = cursor.saturating_sub(delta.unsigned_abs());
            } else {
                *cursor = cursor.saturating_add(delta.unsigned_abs());
            }
        }
        AppEvent::JumpSelection { top } => {
            let cursor = match state.focus {
                PanelFocus::Sidebar => &mut state.sidebar_selected,
                PanelFocus::Main | PanelFocus::RightSidebar => &mut state.main_selected,
            };
            *cursor = if top { 0 } else { usize::MAX / 2 };
        }
        AppEvent::CycleLibraryFilter => {
            state.library_filter = state.library_filter.next();
            state.sidebar_selected = 0;
        }
        AppEvent::ToggleQueuePanel => {
            state.show_right_sidebar = true;
            state.right_tab = match state.right_tab {
                RightSidebarTab::NowPlaying => RightSidebarTab::Queue,
                RightSidebarTab::Queue => RightSidebarTab::NowPlaying,
            };
        }
        AppEvent::ToggleZenMode => {
            if state.route == Route::ZenNowPlaying {
                if let Some(prev) = state.history.pop() {
                    state.route = prev;
                } else {
                    state.route = Route::Home;
                }
            } else {
                let prev = std::mem::replace(&mut state.route, Route::ZenNowPlaying);
                state.history.push(prev);
            }
        }
        AppEvent::SetSearchQuery(query) => {
            state.search_query.clone_from(&query);
            commands.push(AppCommand::ExecuteSearch(query));
        }
        AppEvent::SetSearchCategory(cat) => {
            state.search_category = cat;
            state.main_selected = 0;
        }
        AppEvent::SetInlineFilter(filter) => {
            state.inline_filter = filter;
            state.main_selected = 0;
        }
        AppEvent::ToggleMultiSelect(idx) => {
            if !state.multi_selected.remove(&idx) {
                state.multi_selected.insert(idx);
            }
        }
        AppEvent::PlayTrack(track) => {
            let track_id = track.id.clone();
            if let Some(prev) = state.playback.current_track.replace(track) {
                state.queue.recently_played.insert(0, prev);
            }
            state.playback.is_playing = true;
            state.playback.position_ms = 0;
            commands.push(AppCommand::PlayTrackId(track_id));
        }
        AppEvent::TogglePlayback => {
            state.playback.is_playing = !state.playback.is_playing;
            commands.push(AppCommand::SetPlaying(state.playback.is_playing));
        }
        AppEvent::NextTrack => {
            if let Some(next_track) = state.queue.up_next.first().cloned() {
                state.queue.up_next.remove(0);
                if let Some(prev) = state.playback.current_track.replace(next_track) {
                    state.queue.recently_played.insert(0, prev);
                }
                state.playback.position_ms = 0;
                state.playback.is_playing = true;
            }
            commands.push(AppCommand::NextTrack);
        }
        AppEvent::PrevTrack => {
            if state.playback.position_ms > 3_000 {
                state.playback.position_ms = 0;
                commands.push(AppCommand::SeekMs(0));
            } else {
                if let Some(prev_track) = state.queue.recently_played.first().cloned() {
                    state.queue.recently_played.remove(0);
                    if let Some(curr) = state.playback.current_track.replace(prev_track) {
                        state.queue.up_next.insert(0, curr);
                    }
                    state.playback.position_ms = 0;
                }
                commands.push(AppCommand::PrevTrack);
            }
        }
        AppEvent::SeekRelative(delta_ms) => {
            let max_ms = state
                .playback
                .current_track
                .as_ref()
                .map_or(0, |t| t.duration_ms);
            let next = if delta_ms < 0 {
                state
                    .playback
                    .position_ms
                    .saturating_sub(delta_ms.unsigned_abs())
            } else {
                state
                    .playback
                    .position_ms
                    .saturating_add(delta_ms.unsigned_abs())
                    .min(max_ms)
            };
            state.playback.position_ms = next;
            commands.push(AppCommand::SeekMs(next));
        }
        AppEvent::SeekAbsolute(pos_ms) => {
            let max_ms = state
                .playback
                .current_track
                .as_ref()
                .map_or(pos_ms, |t| t.duration_ms);
            let clamped = pos_ms.min(max_ms);
            state.playback.position_ms = clamped;
            commands.push(AppCommand::SeekMs(clamped));
        }
        AppEvent::CycleRepeat => {
            state.playback.repeat = state.playback.repeat.cycle();
            commands.push(AppCommand::SetRepeat(state.playback.repeat.as_api_str()));
        }
        AppEvent::ToggleShuffle => {
            state.playback.shuffle = !state.playback.shuffle;
            commands.push(AppCommand::SetShuffle(state.playback.shuffle));
        }
        AppEvent::AdjustVolume(delta) => {
            let current = i16::from(state.playback.volume_percent);
            let clamped = (current + i16::from(delta)).clamp(0, 100);
            if let Ok(next_vol) = u8::try_from(clamped) {
                state.playback.volume_percent = next_vol;
                state.playback.muted = next_vol == 0;
                commands.push(AppCommand::SetVolume(next_vol));
            }
        }
        AppEvent::SetVolume(vol) => {
            let clamped = vol.min(100);
            state.playback.volume_percent = clamped;
            state.playback.muted = clamped == 0;
            commands.push(AppCommand::SetVolume(clamped));
        }
        AppEvent::ToggleMute => {
            state.playback.muted = !state.playback.muted;
            let effective = if state.playback.muted {
                0
            } else {
                state.playback.volume_percent
            };
            commands.push(AppCommand::SetVolume(effective));
        }
        AppEvent::ToggleCurrentLike => {
            if let Some(ref mut track) = state.playback.current_track {
                track.is_liked = !track.is_liked;
                let liked = track.is_liked;
                let track_id = track.id.clone();
                state.toast = Some(ToastMessage {
                    level: ToastLevel::Info,
                    text: if liked {
                        String::from("Added to Liked Songs")
                    } else {
                        String::from("Removed from Liked Songs")
                    },
                    ttl_ticks: 12,
                });
                commands.push(AppCommand::SetTrackLiked { track_id, liked });
            }
        }
        AppEvent::EnqueueTrack(track) => {
            let id = track.id.clone();
            let title = track.title.clone();
            state.queue.up_next.push(track);
            state.toast = Some(ToastMessage {
                level: ToastLevel::Info,
                text: format!("Queued \"{title}\""),
                ttl_ticks: 12,
            });
            commands.push(AppCommand::EnqueueTrackId(id));
        }
        AppEvent::SelectDevice(idx) => {
            if let Some(device) = state.devices.get(idx).cloned() {
                for (i, d) in state.devices.iter_mut().enumerate() {
                    d.is_active = i == idx;
                }
                state.playback.active_device = Some(device.clone());
                state.modal = None;
                state.toast = Some(ToastMessage {
                    level: ToastLevel::Info,
                    text: format!("Switched playback to {}", device.name),
                    ttl_ticks: 12,
                });
                commands.push(AppCommand::TransferDevice(device.id));
            }
        }
        AppEvent::OpenModal(target) => {
            state.modal = Some(target);
        }
        AppEvent::DismissModal => {
            if state.modal.is_some() {
                state.modal = None;
            } else if state.inline_filter.is_some() {
                state.inline_filter = None;
            }
        }
        AppEvent::ShowToast { level, text } => {
            state.toast = Some(ToastMessage {
                level,
                text,
                ttl_ticks: 16,
            });
        }
        AppEvent::Quit => {
            state.should_quit = true;
        }
    }

    commands
}

#[cfg(test)]
mod tests {
    use crate::model::TrackSummary;

    use super::{ActiveModal, AppCommand, AppEvent, AppState, LayoutBreakpoint, Route, reduce};

    #[test]
    fn breakpoints_match_specification() {
        assert_eq!(
            LayoutBreakpoint::from_terminal_size(160, 40),
            LayoutBreakpoint::Wide
        );
        assert_eq!(
            LayoutBreakpoint::from_terminal_size(110, 35),
            LayoutBreakpoint::Medium
        );
        assert_eq!(
            LayoutBreakpoint::from_terminal_size(75, 24),
            LayoutBreakpoint::Compact
        );
        assert_eq!(
            LayoutBreakpoint::from_terminal_size(50, 24),
            LayoutBreakpoint::MiniPlayer
        );
        assert_eq!(
            LayoutBreakpoint::from_terminal_size(140, 12),
            LayoutBreakpoint::MiniPlayer
        );
    }

    #[test]
    fn navigation_queue_and_playback_reducer_work() {
        let mut state = AppState::default();
        reduce(&mut state, AppEvent::Navigate(Route::Search));
        assert_eq!(state.route, Route::Search);
        assert_eq!(state.history, vec![Route::Home]);

        reduce(&mut state, AppEvent::NavigateBack);
        assert_eq!(state.route, Route::Home);
        assert_eq!(state.history, []);

        let track = TrackSummary {
            id: String::from("t1"),
            title: String::from("Instant Crush"),
            artists: vec![String::from("Daft Punk")],
            album: String::from("Random Access Memories"),
            duration_ms: 337_000,
            explicit: false,
            is_liked: false,
            cover: None,
        };
        let cmds = reduce(&mut state, AppEvent::PlayTrack(track));
        assert_eq!(cmds, vec![AppCommand::PlayTrackId(String::from("t1"))]);
        assert!(state.playback.is_playing);

        let like_cmds = reduce(&mut state, AppEvent::ToggleCurrentLike);
        assert_eq!(
            like_cmds,
            vec![AppCommand::SetTrackLiked {
                track_id: String::from("t1"),
                liked: true,
            }]
        );

        reduce(&mut state, AppEvent::OpenModal(ActiveModal::Help));
        assert_eq!(state.modal, Some(ActiveModal::Help));
        reduce(&mut state, AppEvent::DismissModal);
        assert_eq!(state.modal, None);
    }
}
