//! Elm-style (TEA) application state, responsive breakpoints, and pure reducer.

use crate::model::PlaybackState;

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

/// Active navigation view in the main content area.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    /// Home dashboard with recently played cover grid.
    #[default]
    Home,
    /// Instant search across Spotify entities.
    Search,
    /// Full-screen Zen Now Playing mode with large cover art.
    ZenNowPlaying,
    /// Album detail view by Spotify ID.
    Album(String),
    /// Playlist detail view by Spotify ID.
    Playlist(String),
    /// Artist detail view by Spotify ID.
    Artist(String),
}

/// Floating modal overlay active on top of the main layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveModal {
    /// Fuzzy Command Palette (`Ctrl+P` or `:`).
    CommandPalette,
    /// Spotify Connect Device Picker (`d`).
    DevicePicker,
    /// Keybinding help overlay (`?`).
    Help,
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
    /// Navigate to a new route, pushing the current route onto history.
    Navigate(Route),
    /// Navigate backward in history (`Alt+Left`).
    NavigateBack,
    /// Toggle play/pause optimistically.
    TogglePlayback,
    /// Cycle repeat mode (`Off -> Context -> Track`).
    CycleRepeat,
    /// Toggle shuffle mode.
    ToggleShuffle,
    /// Adjust volume by a signed delta (`-100..=100`).
    AdjustVolume(i8),
    /// Open or toggle a floating modal.
    ToggleModal(ActiveModal),
    /// Dismiss any open modal (`Esc`).
    DismissModal,
    /// Request graceful shutdown.
    Quit,
}

/// Side-effect commands returned by [`reduce`] for background workers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    /// Send play/pause to the active player or Connect backend.
    SetPlaying(bool),
    /// Update Spotify shuffle state.
    SetShuffle(bool),
    /// Update volume percentage (`0..=100`).
    SetVolume(u8),
}

/// Root synchronous state owned by the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppState {
    /// Current main view route.
    pub route: Route,
    /// Back-navigation stack.
    pub history: Vec<Route>,
    /// Active responsive layout tier.
    pub breakpoint: LayoutBreakpoint,
    /// Currently visible floating modal (triggers Modal-Aware Image Occlusion).
    pub modal: Option<ActiveModal>,
    /// Current playback state snapshot.
    pub playback: PlaybackState,
    /// Signals the event loop to exit cleanly.
    pub should_quit: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            route: Route::Home,
            history: Vec::new(),
            breakpoint: LayoutBreakpoint::Wide,
            modal: None,
            playback: PlaybackState {
                volume_percent: 80,
                ..PlaybackState::default()
            },
            should_quit: false,
        }
    }
}

/// Pure state transition function (Elm architecture).
pub fn reduce(state: &mut AppState, event: AppEvent) -> Vec<AppCommand> {
    let mut commands = Vec::new();

    match event {
        AppEvent::Resize { cols, rows } => {
            state.breakpoint = LayoutBreakpoint::from_terminal_size(cols, rows);
        }
        AppEvent::Navigate(next_route) => {
            if state.route != next_route {
                let previous = std::mem::replace(&mut state.route, next_route);
                state.history.push(previous);
            }
        }
        AppEvent::NavigateBack => {
            if let Some(previous) = state.history.pop() {
                state.route = previous;
            }
        }
        AppEvent::TogglePlayback => {
            state.playback.is_playing = !state.playback.is_playing;
            commands.push(AppCommand::SetPlaying(state.playback.is_playing));
        }
        AppEvent::CycleRepeat => {
            state.playback.repeat = state.playback.repeat.cycle();
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
                commands.push(AppCommand::SetVolume(next_vol));
            }
        }
        AppEvent::ToggleModal(target) => {
            state.modal = if state.modal == Some(target) {
                None
            } else {
                Some(target)
            };
        }
        AppEvent::DismissModal => {
            state.modal = None;
        }
        AppEvent::Quit => {
            state.should_quit = true;
        }
    }

    commands
}

#[cfg(test)]
mod tests {
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
    fn navigation_history_and_volume_clamping_work() {
        let mut state = AppState::default();
        reduce(&mut state, AppEvent::Navigate(Route::Search));
        assert_eq!(state.route, Route::Search);
        assert_eq!(state.history, vec![Route::Home]);

        reduce(&mut state, AppEvent::NavigateBack);
        assert_eq!(state.route, Route::Home);
        assert_eq!(state.history, []);

        let cmds = reduce(&mut state, AppEvent::AdjustVolume(50));
        assert_eq!(state.playback.volume_percent, 100);
        assert_eq!(cmds, vec![AppCommand::SetVolume(100)]);

        reduce(
            &mut state,
            AppEvent::ToggleModal(ActiveModal::CommandPalette),
        );
        assert_eq!(state.modal, Some(ActiveModal::CommandPalette));
        reduce(&mut state, AppEvent::DismissModal);
        assert_eq!(state.modal, None);
    }
}
