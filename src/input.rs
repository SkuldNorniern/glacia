//! Maps Aurea key events to PTY-bound byte sequences.
//!
//! Returns `Option<String>` so callers can forward the bytes unchanged with a
//! single `write_str` call. Most paths are promoted `&'static str`; only
//! `alt_sequence` for letter keys allocates a two-byte heap string.

use aurea::{KeyCode, Modifiers};

/// Return the bytes to write to the PTY for a key press, or `None` if the key
/// should be handled elsewhere (e.g. printable text via `TextInput`).
pub fn terminal_key_bytes(key: KeyCode, mods: Modifiers) -> Option<String> {
    if mods.alt && !mods.ctrl {
        return alt_sequence(key, mods.shift);
    }
    if mods.ctrl {
        return ctrl_sequence(key, mods.shift).map(str::to_owned);
    }
    base_sequence(key, mods.shift).map(str::to_owned)
}

fn ctrl_sequence(key: KeyCode, shift: bool) -> Option<&'static str> {
    // Ctrl+Shift+Arrow: modifier code 6
    if shift {
        return match key {
            KeyCode::Up => Some("\x1b[1;6A"),
            KeyCode::Down => Some("\x1b[1;6B"),
            KeyCode::Right => Some("\x1b[1;6C"),
            KeyCode::Left => Some("\x1b[1;6D"),
            _ => None,
        };
    }
    match key {
        // Standard C0 control characters: Ctrl+[A-Z] = codepoint − 64.
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
        KeyCode::S => Some("\x13"), // XOFF / forward incremental search
        KeyCode::T => Some("\x14"), // transpose characters
        KeyCode::U => Some("\x15"), // kill to beginning of line
        KeyCode::V => Some("\x16"), // literal-next (Ctrl+Shift+V is paste)
        KeyCode::W => Some("\x17"), // kill word backward
        KeyCode::X => Some("\x18"), // prefix / cancel
        KeyCode::Y => Some("\x19"), // yank from kill ring
        KeyCode::Z => Some("\x1a"), // SIGTSTP
        // Ctrl+Arrow: modifier code 5
        KeyCode::Up => Some("\x1b[1;5A"),
        KeyCode::Down => Some("\x1b[1;5B"),
        KeyCode::Right => Some("\x1b[1;5C"),
        KeyCode::Left => Some("\x1b[1;5D"),
        _ => None,
    }
}

/// Alt+key → ESC-prefix sequences (meta / escape-prefix convention).
/// Alt+letter sends ESC + the lowercase letter regardless of shift state,
/// matching the behavior of xterm and most modern terminal emulators.
fn alt_sequence(key: KeyCode, _shift: bool) -> Option<String> {
    let letter = match key {
        KeyCode::A => Some('a'),
        KeyCode::B => Some('b'),
        KeyCode::C => Some('c'),
        KeyCode::D => Some('d'),
        KeyCode::E => Some('e'),
        KeyCode::F => Some('f'),
        KeyCode::G => Some('g'),
        KeyCode::H => Some('h'),
        KeyCode::I => Some('i'),
        KeyCode::J => Some('j'),
        KeyCode::K => Some('k'),
        KeyCode::L => Some('l'),
        KeyCode::M => Some('m'),
        KeyCode::N => Some('n'),
        KeyCode::O => Some('o'),
        KeyCode::P => Some('p'),
        KeyCode::Q => Some('q'),
        KeyCode::R => Some('r'),
        KeyCode::S => Some('s'),
        KeyCode::T => Some('t'),
        KeyCode::U => Some('u'),
        KeyCode::V => Some('v'),
        KeyCode::W => Some('w'),
        KeyCode::X => Some('x'),
        KeyCode::Y => Some('y'),
        KeyCode::Z => Some('z'),
        _ => None,
    };
    if let Some(c) = letter {
        return Some(format!("\x1b{c}"));
    }
    // Alt+Arrow: CSI 1;3 direction
    match key {
        KeyCode::Up => Some("\x1b[1;3A".to_owned()),
        KeyCode::Down => Some("\x1b[1;3B".to_owned()),
        KeyCode::Right => Some("\x1b[1;3C".to_owned()),
        KeyCode::Left => Some("\x1b[1;3D".to_owned()),
        _ => None,
    }
}

fn base_sequence(key: KeyCode, shift: bool) -> Option<&'static str> {
    match key {
        KeyCode::Enter => Some("\r"),
        KeyCode::Backspace => Some("\x7f"),
        // Shift+Tab → reverse-tab (CSI Z); plain Tab → \t
        KeyCode::Tab => Some(if shift { "\x1b[Z" } else { "\t" }),
        KeyCode::Escape => Some("\x1b"),
        // Shift+Arrow → CSI 1;2 modifier; plain arrow → standard CSI
        KeyCode::Up => Some(if shift { "\x1b[1;2A" } else { "\x1b[A" }),
        KeyCode::Down => Some(if shift { "\x1b[1;2B" } else { "\x1b[B" }),
        KeyCode::Right => Some(if shift { "\x1b[1;2C" } else { "\x1b[C" }),
        KeyCode::Left => Some(if shift { "\x1b[1;2D" } else { "\x1b[D" }),
        KeyCode::Home => Some("\x1b[H"),
        KeyCode::End => Some("\x1b[F"),
        KeyCode::Delete => Some("\x1b[3~"),
        KeyCode::Insert => Some("\x1b[2~"),
        KeyCode::PageUp => Some("\x1b[5~"),
        KeyCode::PageDown => Some("\x1b[6~"),
        // F1-F4 use SS3 sequences; F5-F12 use CSI ~ sequences (xterm convention).
        KeyCode::F1 => Some("\x1bOP"),
        KeyCode::F2 => Some("\x1bOQ"),
        KeyCode::F3 => Some("\x1bOR"),
        KeyCode::F4 => Some("\x1bOS"),
        KeyCode::F5 => Some("\x1b[15~"),
        KeyCode::F6 => Some("\x1b[17~"),
        KeyCode::F7 => Some("\x1b[18~"),
        KeyCode::F8 => Some("\x1b[19~"),
        KeyCode::F9 => Some("\x1b[20~"),
        KeyCode::F10 => Some("\x1b[21~"),
        KeyCode::F11 => Some("\x1b[23~"),
        KeyCode::F12 => Some("\x1b[24~"),
        _ => None,
    }
}
