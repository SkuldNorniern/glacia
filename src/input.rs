//! Maps Aurea key events to PTY-bound byte sequences.
//!
//! Returns `Option<String>` so callers can forward the bytes unchanged with a
//! single `write_str` call.

use aurea::{KeyCode, Modifiers};

use crate::unicode::{compose_hangul_jamo, is_hangul_jamo};

/// Return the bytes to write to the PTY for a key press, or `None` if the key
/// should be handled elsewhere (e.g. printable text via `TextInput`).
///
/// Follows xterm: modified keys carry a modifier code (`1 + shift + 2·alt +
/// 4·ctrl`), and plain arrows, Home and End use SS3 (`ESC O x`) while the
/// app has asked for application cursor keys (DECCKM: vim, less, htop).
pub fn terminal_key_bytes(key: KeyCode, mods: Modifiers, app_cursor: bool) -> Option<String> {
    let code = 1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);
    if let Some(letter) = cursor_letter(key) {
        return Some(if code > 1 {
            format!("\x1b[1;{code}{letter}")
        } else if app_cursor {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[{letter}")
        });
    }
    if let Some(n) = tilde_number(key) {
        return Some(if code > 1 {
            format!("\x1b[{n};{code}~")
        } else {
            format!("\x1b[{n}~")
        });
    }
    if let Some(letter) = f1_to_f4(key) {
        return Some(if code > 1 {
            format!("\x1b[1;{code}{letter}")
        } else {
            format!("\x1bO{letter}")
        });
    }
    let base = match key {
        KeyCode::Enter => "\r".to_owned(),
        // Ctrl+Backspace is ^H, the word rub-out PowerShell and some shells use
        KeyCode::Backspace if mods.ctrl => "\x08".to_owned(),
        KeyCode::Backspace => "\x7f".to_owned(),
        // back-tab; with ctrl it carries the modifier code like the others
        KeyCode::Tab if mods.shift && mods.ctrl => return Some(format!("\x1b[1;{code}Z")),
        KeyCode::Tab if mods.shift => "\x1b[Z".to_owned(),
        KeyCode::Tab => "\t".to_owned(),
        KeyCode::Escape => "\x1b".to_owned(),
        KeyCode::Space if mods.ctrl => "\0".to_owned(),
        _ if mods.ctrl && control_punct(key, mods.shift).is_some() => {
            control_punct(key, mods.shift)?.to_string()
        }
        _ => {
            let c = letter(key)?;
            if mods.ctrl {
                // Ctrl+[A-Z] = codepoint - 64
                char::from(c.to_ascii_uppercase() as u8 - b'@').to_string()
            } else if mods.alt {
                // Alt+letter is ESC + the letter as typed: shift makes it
                // upper case, like xterm's meta-sends-escape
                if mods.shift {
                    c.to_ascii_uppercase().to_string()
                } else {
                    c.to_string()
                }
            } else {
                return None;
            }
        }
    };
    // Alt is ESC in front (meta-sends-escape): Alt+Backspace rubs out a word
    if mods.alt {
        Some(format!("\x1b{base}"))
    } else {
        Some(base)
    }
}

fn cursor_letter(key: KeyCode) -> Option<char> {
    Some(match key {
        KeyCode::Up => 'A',
        KeyCode::Down => 'B',
        KeyCode::Right => 'C',
        KeyCode::Left => 'D',
        KeyCode::Home => 'H',
        KeyCode::End => 'F',
        _ => return None,
    })
}

fn tilde_number(key: KeyCode) -> Option<u8> {
    Some(match key {
        KeyCode::Insert => 2,
        KeyCode::Delete => 3,
        KeyCode::PageUp => 5,
        KeyCode::PageDown => 6,
        KeyCode::F5 => 15,
        KeyCode::F6 => 17,
        KeyCode::F7 => 18,
        KeyCode::F8 => 19,
        KeyCode::F9 => 20,
        KeyCode::F10 => 21,
        KeyCode::F11 => 23,
        KeyCode::F12 => 24,
        _ => return None,
    })
}

fn f1_to_f4(key: KeyCode) -> Option<char> {
    Some(match key {
        KeyCode::F1 => 'P',
        KeyCode::F2 => 'Q',
        KeyCode::F3 => 'R',
        KeyCode::F4 => 'S',
        _ => return None,
    })
}

/// Ctrl with a punctuation key: `^[` `^\` `^]` `^^` `^_` and `^@`, the way
/// xterm sends them on a US layout.
fn control_punct(key: KeyCode, shift: bool) -> Option<char> {
    Some(match key {
        KeyCode::LeftBracket => '\x1b',
        KeyCode::Backslash => '\x1c',
        KeyCode::RightBracket => '\x1d',
        KeyCode::Key6 => '\x1e',
        KeyCode::Slash => '\x1f',
        // ctrl+minus alone is zoom; with shift it is `^_`, emacs's undo
        KeyCode::Minus if shift => '\x1f',
        KeyCode::Key2 => '\0',
        _ => return None,
    })
}

fn letter(key: KeyCode) -> Option<char> {
    Some(match key {
        KeyCode::A => 'a',
        KeyCode::B => 'b',
        KeyCode::C => 'c',
        KeyCode::D => 'd',
        KeyCode::E => 'e',
        KeyCode::F => 'f',
        KeyCode::G => 'g',
        KeyCode::H => 'h',
        KeyCode::I => 'i',
        KeyCode::J => 'j',
        KeyCode::K => 'k',
        KeyCode::L => 'l',
        KeyCode::M => 'm',
        KeyCode::N => 'n',
        KeyCode::O => 'o',
        KeyCode::P => 'p',
        KeyCode::Q => 'q',
        KeyCode::R => 'r',
        KeyCode::S => 's',
        KeyCode::T => 't',
        KeyCode::U => 'u',
        KeyCode::V => 'v',
        KeyCode::W => 'w',
        KeyCode::X => 'x',
        KeyCode::Y => 'y',
        KeyCode::Z => 'z',
        _ => return None,
    })
}

/// Composes conjoining hangul jamo that an input method hands over one at
/// a time into syllables. Other text passes through as it is.
#[derive(Default)]
pub struct TextInputNormalizer {
    pending_jamo: String,
}

impl TextInputNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn normalize(&mut self, text: &str) -> String {
        let pending = &mut self.pending_jamo;
        if text.is_empty() {
            return String::new();
        }
        if text.chars().all(is_hangul_jamo) {
            pending.push_str(text);
            let composed = compose_hangul_jamo(pending);
            if composed != pending.as_str() {
                let out = composed.into_owned();
                pending.clear();
                return out;
            }
            return String::new();
        }
        let mut out = String::new();
        if !pending.is_empty() {
            out.push_str(&compose_hangul_jamo(pending));
            pending.clear();
        }
        out.push_str(&compose_hangul_jamo(text));
        out
    }

    /// Jamo still waiting for the rest of their syllable, as they are.
    pub fn flush(&mut self) -> String {
        let out = compose_hangul_jamo(&self.pending_jamo).into_owned();
        self.pending_jamo.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{TextInputNormalizer, terminal_key_bytes};
    use aurea::{KeyCode, Modifiers};

    fn key(key: KeyCode, shift: bool, ctrl: bool, alt: bool, app: bool) -> String {
        let mods = Modifiers {
            shift,
            ctrl,
            alt,
            ..Modifiers::default()
        };
        terminal_key_bytes(key, mods, app).unwrap_or_default()
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        assert_eq!(key(KeyCode::Up, false, false, false, false), "\x1b[A");
        assert_eq!(key(KeyCode::Up, false, false, false, true), "\x1bOA");
        assert_eq!(key(KeyCode::Home, false, false, false, true), "\x1bOH");
        assert_eq!(key(KeyCode::Left, false, true, false, true), "\x1b[1;5D");
        assert_eq!(key(KeyCode::Right, true, true, false, false), "\x1b[1;6C");
    }

    #[test]
    fn modified_editing_and_function_keys() {
        assert_eq!(
            key(KeyCode::Backspace, false, false, true, false),
            "\x1b\x7f"
        );
        assert_eq!(key(KeyCode::Backspace, false, true, false, false), "\x08");
        assert_eq!(key(KeyCode::Delete, false, true, false, false), "\x1b[3;5~");
        assert_eq!(key(KeyCode::PageUp, true, false, false, false), "\x1b[5;2~");
        assert_eq!(key(KeyCode::F1, false, false, false, false), "\x1bOP");
        assert_eq!(key(KeyCode::F5, true, false, false, false), "\x1b[15;2~");
        assert_eq!(key(KeyCode::Enter, false, false, true, false), "\x1b\r");
        assert_eq!(key(KeyCode::Tab, true, false, false, false), "\x1b[Z");
    }

    #[test]
    fn ctrl_and_alt_letters() {
        assert_eq!(key(KeyCode::C, false, true, false, false), "\x03");
        assert_eq!(key(KeyCode::B, false, false, true, false), "\x1bb");
        assert_eq!(key(KeyCode::B, true, false, true, false), "\x1bB");
        assert_eq!(key(KeyCode::Tab, true, false, true, false), "\x1b\x1b[Z");
        assert_eq!(key(KeyCode::Tab, true, true, false, false), "\x1b[1;6Z");
        assert_eq!(key(KeyCode::X, false, true, true, false), "\x1b\x18");
        assert_eq!(key(KeyCode::Space, false, true, false, false), "\0");
        assert_eq!(
            key(KeyCode::A, false, false, false, false),
            "",
            "letters come as text"
        );
    }

    #[test]
    fn latin_text_is_not_held_back() {
        let mut normalizer = TextInputNormalizer::new();
        assert_eq!(normalizer.normalize("é"), "é");
        assert_eq!(normalizer.normalize("°"), "°");
        assert_eq!(normalizer.flush(), "");
    }

    #[test]
    fn leaves_valid_text_input_unchanged() {
        let mut normalizer = TextInputNormalizer::new();
        assert_eq!(normalizer.normalize("abc"), "abc");
        assert_eq!(normalizer.normalize("\u{AC00}\u{B098}"), "\u{AC00}\u{B098}");
    }

    #[test]
    fn composes_hangul_jamo_text_input() {
        let mut normalizer = TextInputNormalizer::new();
        assert_eq!(
            normalizer.normalize("\u{1100}\u{1161}\u{1102}\u{1161}"),
            "\u{AC00}\u{B098}"
        );
    }

    #[test]
    fn buffers_split_hangul_jamo_text_input() {
        let mut normalizer = TextInputNormalizer::new();
        assert_eq!(normalizer.normalize("\u{1100}"), "");
        assert_eq!(normalizer.normalize("\u{1161}"), "\u{AC00}");
        assert_eq!(normalizer.normalize("\u{1102}"), "");
        assert_eq!(normalizer.normalize("\u{1161}"), "\u{B098}");
    }

    #[test]
    fn flushes_pending_hangul_jamo_text_input() {
        let mut normalizer = TextInputNormalizer::new();
        assert_eq!(normalizer.normalize("\u{1100}"), "");
        assert_eq!(normalizer.flush(), "\u{1100}");
    }

    #[test]
    fn ctrl_punctuation_sends_control_codes() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let key = |k| terminal_key_bytes(k, ctrl, false);
        assert_eq!(key(KeyCode::LeftBracket).as_deref(), Some("\x1b"));
        assert_eq!(key(KeyCode::Backslash).as_deref(), Some("\x1c"));
        assert_eq!(key(KeyCode::RightBracket).as_deref(), Some("\x1d"));
        assert_eq!(key(KeyCode::Slash).as_deref(), Some("\x1f"));
        assert_eq!(key(KeyCode::Minus), None, "zoom");
        let alt = Modifiers { alt: true, ..ctrl };
        assert_eq!(
            terminal_key_bytes(KeyCode::RightBracket, alt, false).as_deref(),
            Some("\x1b\x1d")
        );
    }
}
