//! Cross-platform terminal raw-mode guard (`crossterm`), panic-safe restoration hook,
//! and keyboard/mouse input decoding (`#[forbid(unsafe_code)]`).

use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode as CtKeyCode,
    KeyEvent as CtKeyEvent, KeyEventKind, KeyModifiers as CtKeyModifiers,
    MouseButton as CtMouseButton, MouseEvent as CtMouseEvent, MouseEventKind as CtMouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use vynl_core::keymap::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// High-level terminal input event decoded from `crossterm` or raw VT100 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalInput {
    /// Keyboard press.
    Key(KeyEvent),
    /// Mouse interaction.
    Mouse(MouseEvent),
    /// Terminal resized to `(cols, rows)`.
    Resize(u16, u16),
}

/// Restores terminal state cleanly even if the application panics.
pub fn restore_terminal_state() {
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, DisableMouseCapture, Show, LeaveAlternateScreen);
    let _ = stdout.write_all(b"\x1b[0m");
    let _ = stdout.flush();
}

/// RAII guard owning native `crossterm` alternate screen, raw mode, mouse capture,
/// and panic-safe terminal restoration hook across Linux, macOS, and Windows.
#[derive(Debug)]
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Enters alternate screen, raw mode, and optional mouse capture,
    /// installing a panic hook that always restores the user's terminal.
    ///
    /// # Errors
    /// Returns an I/O error if enabling raw mode or writing terminal setup sequences fails.
    pub fn enter(mouse_enabled: bool) -> io::Result<Self> {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_state();
            prev_hook(info);
        }));

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide, Clear(ClearType::All))?;
        if mouse_enabled {
            execute!(stdout, EnableMouseCapture)?;
        }
        stdout.flush()?;

        Ok(Self { active: true })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.active {
            restore_terminal_state();
            self.active = false;
        }
    }
}

/// Queries current terminal dimensions `(cols, rows)` natively via `crossterm::terminal::size`,
/// falling back to `$COLUMNS`/`$LINES` or `(140, 40)` when detached from a TTY.
#[must_use]
pub fn query_terminal_size() -> (u16, u16) {
    if let Ok((cols, rows)) = crossterm::terminal::size()
        && cols > 0
        && rows > 0
    {
        return (cols, rows);
    }

    let cols = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(140);
    let rows = std::env::var("LINES")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|&r| r > 0)
        .unwrap_or(40);
    (cols, rows)
}

/// Polls for the next native `crossterm` terminal event up to `timeout`.
///
/// # Errors
/// Returns an I/O error if polling or reading from the terminal fails.
pub fn poll_terminal_event(timeout: Duration) -> io::Result<Option<TerminalInput>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }
    let raw = event::read()?;
    Ok(from_crossterm_event(&raw))
}

/// Translates a native [`crossterm::event::Event`] into a [`TerminalInput`].
///
/// Filters out key release events so Windows terminals never double-fire bindings.
#[must_use]
pub fn from_crossterm_event(event: &Event) -> Option<TerminalInput> {
    match event {
        Event::Key(key) => from_crossterm_key(*key).map(TerminalInput::Key),
        Event::Mouse(mouse) => from_crossterm_mouse(*mouse).map(TerminalInput::Mouse),
        Event::Resize(cols, rows) => Some(TerminalInput::Resize(*cols, *rows)),
        Event::FocusGained | Event::FocusLost | Event::Paste(_) => None,
    }
}

fn from_crossterm_key(key: CtKeyEvent) -> Option<KeyEvent> {
    if key.kind == KeyEventKind::Release {
        return None;
    }

    let code = match key.code {
        CtKeyCode::Char(c) => KeyCode::Char(c),
        CtKeyCode::Enter => KeyCode::Enter,
        CtKeyCode::Esc => KeyCode::Esc,
        CtKeyCode::Backspace => KeyCode::Backspace,
        CtKeyCode::Tab => KeyCode::Tab,
        CtKeyCode::BackTab => KeyCode::BackTab,
        CtKeyCode::Up => KeyCode::Up,
        CtKeyCode::Down => KeyCode::Down,
        CtKeyCode::Left => KeyCode::Left,
        CtKeyCode::Right => KeyCode::Right,
        CtKeyCode::Home => KeyCode::Home,
        CtKeyCode::End => KeyCode::End,
        _ => return None,
    };

    let modifiers = KeyModifiers {
        ctrl: key.modifiers.contains(CtKeyModifiers::CONTROL),
        alt: key.modifiers.contains(CtKeyModifiers::ALT),
        shift: key.modifiers.contains(CtKeyModifiers::SHIFT),
    };

    Some(KeyEvent { code, modifiers })
}

const fn from_crossterm_mouse(mouse: CtMouseEvent) -> Option<MouseEvent> {
    let kind = match mouse.kind {
        CtMouseEventKind::Down(btn) => MouseEventKind::Down(from_crossterm_button(btn)),
        CtMouseEventKind::Up(btn) => MouseEventKind::Up(from_crossterm_button(btn)),
        CtMouseEventKind::Drag(btn) => MouseEventKind::Drag(from_crossterm_button(btn)),
        CtMouseEventKind::ScrollUp => MouseEventKind::ScrollUp,
        CtMouseEventKind::ScrollDown => MouseEventKind::ScrollDown,
        CtMouseEventKind::Moved | CtMouseEventKind::ScrollLeft | CtMouseEventKind::ScrollRight => {
            return None;
        }
    };

    Some(MouseEvent {
        kind,
        col: mouse.column,
        row: mouse.row,
    })
}

const fn from_crossterm_button(btn: CtMouseButton) -> MouseButton {
    match btn {
        CtMouseButton::Left => MouseButton::Left,
        CtMouseButton::Right => MouseButton::Right,
        CtMouseButton::Middle => MouseButton::Middle,
    }
}

/// Parses a chunk of raw VT100/SGR-1006 stdin bytes into [`TerminalInput`] events.
#[must_use]
pub fn parse_input_bytes(bytes: &[u8]) -> Vec<TerminalInput> {
    let mut events = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            if i + 1 >= bytes.len() {
                events.push(TerminalInput::Key(KeyEvent::plain(KeyCode::Esc)));
                i += 1;
                continue;
            }
            if bytes[i + 1] == b'[' {
                if let Some((ev, consumed)) = parse_csi(&bytes[i + 2..]) {
                    events.push(ev);
                    i += 2 + consumed;
                    continue;
                }
                events.push(TerminalInput::Key(KeyEvent::plain(KeyCode::Esc)));
                i += 1;
                continue;
            }
            let ch = char::from(bytes[i + 1]);
            events.push(TerminalInput::Key(KeyEvent::alt(KeyCode::Char(ch))));
            i += 2;
            continue;
        }

        if let Some(key) = decode_single_byte(b) {
            events.push(TerminalInput::Key(key));
        }
        i += 1;
    }

    events
}

fn decode_single_byte(b: u8) -> Option<KeyEvent> {
    match b {
        b'\r' | b'\n' => Some(KeyEvent::plain(KeyCode::Enter)),
        b'\t' => Some(KeyEvent::plain(KeyCode::Tab)),
        0x7f | 0x08 => Some(KeyEvent::plain(KeyCode::Backspace)),
        0x03 => Some(KeyEvent::ctrl('c')),
        0x06 => Some(KeyEvent::ctrl('f')),
        0x10 => Some(KeyEvent::ctrl('p')),
        0x20..=0x7e => Some(KeyEvent::plain(KeyCode::Char(char::from(b)))),
        _ => None,
    }
}

fn parse_csi(rest: &[u8]) -> Option<(TerminalInput, usize)> {
    if rest.is_empty() {
        return None;
    }

    if rest[0] == b'<' {
        let end = rest.iter().position(|&c| c == b'M' || c == b'm')?;
        let body = std::str::from_utf8(&rest[1..end]).ok()?;
        let mut parts = body.split(';');
        let code: u16 = parts.next()?.parse().ok()?;
        let col_1: u16 = parts.next()?.parse().ok()?;
        let row_1: u16 = parts.next()?.parse().ok()?;
        let released = rest[end] == b'm';

        let col = col_1.saturating_sub(1);
        let row = row_1.saturating_sub(1);
        let kind = match code {
            64 => MouseEventKind::ScrollUp,
            65 => MouseEventKind::ScrollDown,
            c if (c & 32) != 0 => MouseEventKind::Drag(button_from_bits(c & 3)),
            c if released => MouseEventKind::Up(button_from_bits(c & 3)),
            c => MouseEventKind::Down(button_from_bits(c & 3)),
        };

        return Some((TerminalInput::Mouse(MouseEvent { kind, col, row }), end + 1));
    }

    if rest.len() >= 4 && rest[0] == b'1' && rest[1] == b';' {
        let mod_digit = rest[2];
        let dir = rest[3];
        let code = match dir {
            b'A' => KeyCode::Up,
            b'B' => KeyCode::Down,
            b'C' => KeyCode::Right,
            b'D' => KeyCode::Left,
            _ => return None,
        };
        let modifiers = match mod_digit {
            b'2' => KeyModifiers {
                shift: true,
                ..KeyModifiers::default()
            },
            b'3' => KeyModifiers {
                alt: true,
                ..KeyModifiers::default()
            },
            b'5' => KeyModifiers {
                ctrl: true,
                ..KeyModifiers::default()
            },
            _ => KeyModifiers::default(),
        };
        return Some((TerminalInput::Key(KeyEvent { code, modifiers }), 4));
    }

    let key = match rest[0] {
        b'A' => KeyEvent::plain(KeyCode::Up),
        b'B' => KeyEvent::plain(KeyCode::Down),
        b'C' => KeyEvent::plain(KeyCode::Right),
        b'D' => KeyEvent::plain(KeyCode::Left),
        b'H' => KeyEvent::plain(KeyCode::Home),
        b'F' => KeyEvent::plain(KeyCode::End),
        b'Z' => KeyEvent::plain(KeyCode::BackTab),
        _ => return None,
    };
    Some((TerminalInput::Key(key), 1))
}

const fn button_from_bits(bits: u16) -> MouseButton {
    match bits {
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => MouseButton::Left,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{
        Event, KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyEventKind, KeyEventState,
        KeyModifiers as CtKeyModifiers, MouseButton as CtMouseButton, MouseEvent as CtMouseEvent,
        MouseEventKind as CtMouseEventKind,
    };
    use vynl_core::keymap::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::{TerminalInput, from_crossterm_event, parse_input_bytes};

    #[test]
    fn parses_arrows_ctrl_and_sgr_mouse_sequences() {
        assert_eq!(
            parse_input_bytes(b"\x1b[A\x10\x1b[1;3D"),
            vec![
                TerminalInput::Key(KeyEvent::plain(KeyCode::Up)),
                TerminalInput::Key(KeyEvent::ctrl('p')),
                TerminalInput::Key(KeyEvent {
                    code: KeyCode::Left,
                    modifiers: KeyModifiers {
                        ctrl: false,
                        alt: true,
                        shift: false,
                    },
                }),
            ]
        );

        assert_eq!(
            parse_input_bytes(b"\x1b[<0;15;8M\x1b[<64;10;5M"),
            vec![
                TerminalInput::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    col: 14,
                    row: 7,
                }),
                TerminalInput::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    col: 9,
                    row: 4,
                }),
            ]
        );
    }

    #[test]
    fn translates_crossterm_key_mouse_and_resize_events_and_filters_releases() {
        let press = Event::Key(CtKeyEvent {
            code: CtKeyCode::Char('p'),
            modifiers: CtKeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(
            from_crossterm_event(&press),
            Some(TerminalInput::Key(KeyEvent::ctrl('p')))
        );

        let release = Event::Key(CtKeyEvent {
            code: CtKeyCode::Char('p'),
            modifiers: CtKeyModifiers::CONTROL,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        });
        assert_eq!(from_crossterm_event(&release), None);

        let mouse = Event::Mouse(CtMouseEvent {
            kind: CtMouseEventKind::Down(CtMouseButton::Left),
            column: 12,
            row: 5,
            modifiers: CtKeyModifiers::NONE,
        });
        assert_eq!(
            from_crossterm_event(&mouse),
            Some(TerminalInput::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                col: 12,
                row: 5,
            }))
        );

        assert_eq!(
            from_crossterm_event(&Event::Resize(160, 48)),
            Some(TerminalInput::Resize(160, 48))
        );
    }
}
