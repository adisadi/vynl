//! Hybrid keyboard and mouse input model (Arrows + Vim + Mouse + Command Palette).

/// Logical key codes decoded from terminal input streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Printable character.
    Char(char),
    /// Enter / Return.
    Enter,
    /// Escape.
    Esc,
    /// Backspace.
    Backspace,
    /// Tab.
    Tab,
    /// Shift+Tab (`BackTab`).
    BackTab,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
}

/// Keyboard modifier bitflags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct KeyModifiers {
    /// Control modifier held.
    pub ctrl: bool,
    /// Alt / Meta modifier held.
    pub alt: bool,
    /// Shift modifier held.
    pub shift: bool,
}

/// Decoded key press event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// Key code.
    pub code: KeyCode,
    /// Active modifier flags.
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    /// Convenience constructor for an unmodified key press.
    #[must_use]
    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers {
                ctrl: false,
                alt: false,
                shift: false,
            },
        }
    }

    /// Convenience constructor for `Ctrl+<char>`.
    #[must_use]
    pub const fn ctrl(ch: char) -> Self {
        Self {
            code: KeyCode::Char(ch),
            modifiers: KeyModifiers {
                ctrl: true,
                alt: false,
                shift: false,
            },
        }
    }

    /// Convenience constructor for `Alt+<key>`.
    #[must_use]
    pub const fn alt(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers {
                ctrl: false,
                alt: true,
                shift: false,
            },
        }
    }
}

/// Mouse button kinds supported by SGR mouse tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Primary left click.
    Left,
    /// Secondary right click (context menu).
    Right,
    /// Middle button.
    Middle,
}

/// Mouse interaction kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventKind {
    /// Button pressed.
    Down(MouseButton),
    /// Button dragged while held (scrubbing / volume drag).
    Drag(MouseButton),
    /// Button released.
    Up(MouseButton),
    /// Scroll wheel up.
    ScrollUp,
    /// Scroll wheel down.
    ScrollDown,
}

/// Decoded terminal mouse event (0-indexed cell coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// Interaction kind.
    pub kind: MouseEventKind,
    /// 0-indexed terminal column.
    pub col: u16,
    /// 0-indexed terminal row.
    pub row: u16,
}

/// High-level semantic action resolved from hybrid keybindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Move selection up (`Up` or `k`).
    MoveUp,
    /// Move selection down (`Down` or `j`).
    MoveDown,
    /// Focus left panel (`Left` or `h`).
    FocusLeft,
    /// Focus right panel (`Right` or `l`).
    FocusRight,
    /// Jump to top of list (`g` or `Home`).
    JumpTop,
    /// Jump to bottom of list (`G` or `End`).
    JumpBottom,
    /// Cycle panel focus forward (`Tab`).
    NextPanel,
    /// Cycle panel focus backward (`Shift+Tab`).
    PrevPanel,
    /// Activate / play selected item (`Enter`).
    Select,
    /// Toggle play / pause (`Space`).
    TogglePlayback,
    /// Next track (`n` or `>`).
    NextTrack,
    /// Previous track (`p` or `<`).
    PrevTrack,
    /// Toggle shuffle (`s`).
    ToggleShuffle,
    /// Cycle repeat mode (`r`).
    CycleRepeat,
    /// Volume up (`+` or `=`).
    VolumeUp,
    /// Volume down (`-`).
    VolumeDown,
    /// Mute toggle (`m`).
    ToggleMute,
    /// Seek forward (`Shift+Right`).
    SeekForward,
    /// Seek backward (`Shift+Left`).
    SeekBackward,
    /// Open global search (`/` or `Ctrl+F`).
    OpenSearch,
    /// Open inline list filter (`f`).
    OpenInlineFilter,
    /// Open Command Palette (`Ctrl+P` or `:`).
    OpenCommandPalette,
    /// Toggle like on selected/current track (`L`).
    ToggleLike,
    /// Add selected track to queue (`a`).
    AddToQueue,
    /// Open Add-to-Playlist modal (`P`).
    OpenPlaylistModal,
    /// Toggle multi-select mark on track (`x`).
    ToggleMultiSelect,
    /// Open Spotify Connect Device Picker (`d`).
    OpenDevicePicker,
    /// Toggle Queue panel (`q`).
    ToggleQueue,
    /// Toggle Zen Fullscreen Now Playing mode (`z`).
    ToggleZenMode,
    /// Navigate back in history (`Alt+Left` or `Backspace`).
    NavigateBack,
    /// Open help overlay (`?`).
    OpenHelp,
    /// Dismiss modal / clear filter (`Esc`).
    Dismiss,
    /// Quit application (`Ctrl+C` or `Q`).
    Quit,
}

/// Resolves a [`KeyEvent`] using `vynl`'s hybrid Arrows + Vim keymap.
#[must_use]
pub const fn resolve_hybrid_key(event: KeyEvent) -> Option<KeyAction> {
    if event.modifiers.ctrl {
        return match event.code {
            KeyCode::Char('c') => Some(KeyAction::Quit),
            KeyCode::Char('p') => Some(KeyAction::OpenCommandPalette),
            KeyCode::Char('f') => Some(KeyAction::OpenSearch),
            _ => None,
        };
    }

    if event.modifiers.alt {
        return match event.code {
            KeyCode::Left => Some(KeyAction::NavigateBack),
            _ => None,
        };
    }

    if event.modifiers.shift {
        match event.code {
            KeyCode::Right => return Some(KeyAction::SeekForward),
            KeyCode::Left => return Some(KeyAction::SeekBackward),
            KeyCode::BackTab => return Some(KeyAction::PrevPanel),
            _ => {}
        }
    }

    match event.code {
        KeyCode::Up | KeyCode::Char('k') => Some(KeyAction::MoveUp),
        KeyCode::Down | KeyCode::Char('j') => Some(KeyAction::MoveDown),
        KeyCode::Left | KeyCode::Char('h') => Some(KeyAction::FocusLeft),
        KeyCode::Right | KeyCode::Char('l') => Some(KeyAction::FocusRight),
        KeyCode::Home | KeyCode::Char('g') => Some(KeyAction::JumpTop),
        KeyCode::End | KeyCode::Char('G') => Some(KeyAction::JumpBottom),
        KeyCode::Tab => Some(KeyAction::NextPanel),
        KeyCode::BackTab => Some(KeyAction::PrevPanel),
        KeyCode::Enter => Some(KeyAction::Select),
        KeyCode::Char(' ') => Some(KeyAction::TogglePlayback),
        KeyCode::Char('n' | '>') => Some(KeyAction::NextTrack),
        KeyCode::Char('p' | '<') => Some(KeyAction::PrevTrack),
        KeyCode::Char('s') => Some(KeyAction::ToggleShuffle),
        KeyCode::Char('r') => Some(KeyAction::CycleRepeat),
        KeyCode::Char('+' | '=') => Some(KeyAction::VolumeUp),
        KeyCode::Char('-') => Some(KeyAction::VolumeDown),
        KeyCode::Char('m') => Some(KeyAction::ToggleMute),
        KeyCode::Char('/') => Some(KeyAction::OpenSearch),
        KeyCode::Char('f') => Some(KeyAction::OpenInlineFilter),
        KeyCode::Char(':') => Some(KeyAction::OpenCommandPalette),
        KeyCode::Char('L') => Some(KeyAction::ToggleLike),
        KeyCode::Char('a') => Some(KeyAction::AddToQueue),
        KeyCode::Char('P') => Some(KeyAction::OpenPlaylistModal),
        KeyCode::Char('x') => Some(KeyAction::ToggleMultiSelect),
        KeyCode::Char('d') => Some(KeyAction::OpenDevicePicker),
        KeyCode::Char('q') => Some(KeyAction::ToggleQueue),
        KeyCode::Char('z') => Some(KeyAction::ToggleZenMode),
        KeyCode::Backspace => Some(KeyAction::NavigateBack),
        KeyCode::Char('?') => Some(KeyAction::OpenHelp),
        KeyCode::Esc => Some(KeyAction::Dismiss),
        KeyCode::Char('Q') => Some(KeyAction::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{KeyAction, KeyCode, KeyEvent, resolve_hybrid_key};

    #[test]
    fn arrows_and_vim_keys_work_side_by_side() {
        assert_eq!(
            resolve_hybrid_key(KeyEvent::plain(KeyCode::Down)),
            Some(KeyAction::MoveDown)
        );
        assert_eq!(
            resolve_hybrid_key(KeyEvent::plain(KeyCode::Char('j'))),
            Some(KeyAction::MoveDown)
        );
        assert_eq!(
            resolve_hybrid_key(KeyEvent::ctrl('p')),
            Some(KeyAction::OpenCommandPalette)
        );
        assert_eq!(
            resolve_hybrid_key(KeyEvent::alt(KeyCode::Left)),
            Some(KeyAction::NavigateBack)
        );
    }
}
