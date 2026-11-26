//! Terminal raw-mode guard, panic-safe restoration hook, and VT100 / SGR-1006
//! keyboard & mouse input parser (`#[forbid(unsafe_code)]`).

use std::io::{self, Write};
use std::process::Command;

use vynl_core::keymap::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// High-level terminal input event decoded from stdin bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalInput {
    /// Keyboard press.
    Key(KeyEvent),
    /// SGR mouse interaction.
    Mouse(MouseEvent),
}

/// Restores terminal state cleanly even if the application panics.
pub fn restore_terminal_state() {
    let mut stdout = io::stdout();
    // Disable SGR mouse tracking, show cursor, leave alternate screen, reset SGR.
    let _ = stdout.write_all(b"\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l\x1b[0m");
    let _ = stdout.flush();
    #[cfg(unix)]
    {
        let _ = Command::new("stty").arg("sane").status();
    }
}

/// RAII guard owning alternate screen, raw mode, mouse tracking, and panic hook.
#[derive(Debug)]
pub struct TerminalGuard {
    active: bool,
}

impl TerminalGuard {
    /// Enters alternate screen, raw mode, and optional SGR mouse capture,
    /// installing a panic hook that always restores the user's terminal.
    ///
    /// # Errors
    /// Returns an I/O error if writing terminal setup sequences fails.
    pub fn enter(mouse_enabled: bool) -> io::Result<Self> {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_state();
            prev_hook(info);
        }));

        #[cfg(unix)]
        {
            let _ = Command::new("stty").args(["raw", "-echo"]).status();
        }

        let mut stdout = io::stdout();
        stdout.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H")?;
        if mouse_enabled {
            stdout.write_all(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h")?;
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

/// Queries current terminal dimensions `(cols, rows)` via `stty size` or `$COLUMNS`/`$LINES`,
/// falling back to `(140, 40)`.
#[must_use]
pub fn query_terminal_size() -> (u16, u16) {
    #[cfg(unix)]
    if let Ok(output) = Command::new("stty").arg("size").output()
        && output.status.success()
        && let Ok(text) = String::from_utf8(output.stdout)
    {
        let mut parts = text.split_whitespace();
        if let (Some(rows_s), Some(cols_s)) = (parts.next(), parts.next())
            && let (Ok(rows), Ok(cols)) = (rows_s.parse::<u16>(), cols_s.parse::<u16>())
            && cols > 0
            && rows > 0
        {
            return (cols, rows);
        }
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

/// Parses a chunk of raw stdin bytes into one or more [`TerminalInput`] events.
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
            // Alt + char
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

    // SGR Mouse sequence: `<btn;col;rowM` or `<btn;col;rowm`
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

    // Modified arrows: `1;2C` (Shift+Right), `1;3D` (Alt+Left), etc.
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
    use vynl_core::keymap::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    use super::{TerminalInput, parse_input_bytes};

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
}
