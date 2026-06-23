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
        // Standard C0 control characters: Ctrl+[A-Z] = codepoint − 64.
        // These cover every common readline/emacs binding (Ctrl+P for history,
        // Ctrl+R for reverse-search, Ctrl+A/E for line navigation, etc.).
        return match key {
            KeyCode::A => Some("\x01"), // beginning of line
            KeyCode::B => Some("\x02"), // backward char
            KeyCode::C => Some("\x03"), // SIGINT
            KeyCode::D => Some("\x04"), // EOF / delete forward
            KeyCode::E => Some("\x05"), // end of line
            KeyCode::F => Some("\x06"), // forward char
            KeyCode::G => Some("\x07"), // bell / abort incremental search
            KeyCode::H => Some("\x08"), // backspace (alternate)
            KeyCode::I => Some("\x09"), // horizontal tab
            KeyCode::J => Some("\x0a"), // line feed
            KeyCode::K => Some("\x0b"), // kill to end of line
            KeyCode::L => Some("\x0c"), // clear / redraw
            KeyCode::M => Some("\x0d"), // carriage return
            KeyCode::N => Some("\x0e"), // next history entry
            KeyCode::O => Some("\x0f"), // accept-and-infer-next
            KeyCode::P => Some("\x10"), // previous history entry
            KeyCode::Q => Some("\x11"), // XON — resume output
            KeyCode::R => Some("\x12"), // reverse incremental search
            KeyCode::S => Some("\x13"), // forward incremental search / XOFF
            KeyCode::T => Some("\x14"), // transpose characters
            KeyCode::U => Some("\x15"), // kill to beginning of line
            KeyCode::V => Some("\x16"), // literal-next (Ctrl+Shift+V is paste)
            KeyCode::W => Some("\x17"), // kill word backward
            KeyCode::X => Some("\x18"), // prefix / cancel
            KeyCode::Y => Some("\x19"), // yank from kill ring
            KeyCode::Z => Some("\x1a"), // SIGTSTP
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
