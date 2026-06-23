//! Maps Aurea key events to PTY-bound byte sequences.

use aurea::{KeyCode, Modifiers};

/// Control-key and escape-sequence bytes for keys that don't arrive as
/// `WindowEvent::TextInput`. Printable text is routed separately so IME and
/// composed text keep working.
pub fn terminal_key_bytes(key: KeyCode, mods: Modifiers) -> Option<&'static str> {
    if mods.alt {
        return None;
    }
    if mods.ctrl {
        return match key {
            KeyCode::C => Some("\u{3}"),  // SIGINT
            KeyCode::D => Some("\u{4}"),  // EOF
            KeyCode::Z => Some("\u{1a}"), // SIGTSTP
            KeyCode::L => Some("\u{c}"),  // clear
            _ => None,
        };
    }
    match key {
        KeyCode::Enter => Some("\r"),
        KeyCode::Backspace => Some("\u{7f}"),
        KeyCode::Tab => Some("\t"),
        KeyCode::Escape => Some("\u{1b}"),
        KeyCode::Up => Some("\u{1b}[A"),
        KeyCode::Down => Some("\u{1b}[B"),
        KeyCode::Right => Some("\u{1b}[C"),
        KeyCode::Left => Some("\u{1b}[D"),
        KeyCode::Home => Some("\u{1b}[H"),
        KeyCode::End => Some("\u{1b}[F"),
        KeyCode::Delete => Some("\u{1b}[3~"),
        _ => None,
    }
}
