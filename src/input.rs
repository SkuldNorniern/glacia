//! Maps Aurea key events to PTY-bound byte sequences.
//!
//! Returns `Option<String>` so callers can forward the bytes unchanged with a
//! single `write_str` call. Most paths are promoted `&'static str`; only
//! `alt_sequence` for letter keys allocates a two-byte heap string.

use aurea::{KeyCode, Modifiers};
#[cfg(windows)]
use std::ptr::null_mut;

#[cfg(windows)]
const MB_ERR_INVALID_CHARS: u32 = 0x0000_0008;
#[cfg(windows)]
const LOCALE_IDEFAULTANSICODEPAGE: u32 = 0x0000_1004;
#[cfg(windows)]
const COMMON_TEXT_INPUT_CODEPAGES: &[u32] = &[932, 936, 949, 950, 874, 1251, 1253, 1255, 1256];

#[cfg(windows)]
unsafe extern "system" {
    fn GetACP() -> u32;
    fn GetKeyboardLayout(id_thread: u32) -> isize;
    fn GetLocaleInfoW(locale: u32, lc_type: u32, data: *mut u16, data_len: i32) -> i32;
    fn MultiByteToWideChar(
        code_page: u32,
        flags: u32,
        multi_byte_str: *const u8,
        multi_byte_len: i32,
        wide_char_str: *mut u16,
        wide_char_len: i32,
    ) -> i32;
}

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

pub fn normalize_text_input(text: &str) -> String {
    #[cfg(windows)]
    {
        repair_windows_ansi_mojibake(text).unwrap_or_else(|| text.to_owned())
    }
    #[cfg(not(windows))]
    {
        text.to_owned()
    }
}

#[cfg(windows)]
fn repair_windows_ansi_mojibake(text: &str) -> Option<String> {
    if text.chars().any(is_strong_script_char) {
        return None;
    }

    let mut saw_high_byte = false;
    let mut bytes = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let code = ch as u32;
        if code > 0xFF {
            return None;
        }
        saw_high_byte |= code >= 0x80;
        bytes.push(code as u8);
    }
    if !saw_high_byte {
        return None;
    }

    for code_page in candidate_ansi_code_pages() {
        let Some(decoded) = decode_code_page(code_page, &bytes) else {
            continue;
        };
        if decoded != text && decoded_is_plausible(code_page, &decoded) {
            return Some(decoded);
        }
    }
    None
}

#[cfg(windows)]
fn candidate_ansi_code_pages() -> Vec<u32> {
    let mut pages = Vec::new();
    if let Some(page) = keyboard_layout_ansi_code_page() {
        push_unique(&mut pages, page);
    }
    push_unique(&mut pages, unsafe { GetACP() });
    for &page in COMMON_TEXT_INPUT_CODEPAGES {
        push_unique(&mut pages, page);
    }
    pages
}

#[cfg(windows)]
fn keyboard_layout_ansi_code_page() -> Option<u32> {
    let hkl = unsafe { GetKeyboardLayout(0) };
    let lang_id = (hkl as usize & 0xFFFF) as u32;
    if lang_id == 0 {
        return None;
    }

    let mut buf = [0u16; 16];
    let len = unsafe {
        GetLocaleInfoW(
            lang_id,
            LOCALE_IDEFAULTANSICODEPAGE,
            buf.as_mut_ptr(),
            buf.len() as i32,
        )
    };
    if len <= 1 {
        return None;
    }

    String::from_utf16(&buf[..(len as usize - 1)])
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|page| *page != 0 && *page != 65001)
}

#[cfg(windows)]
fn push_unique(pages: &mut Vec<u32>, page: u32) {
    if page != 0 && page != 65001 && !pages.contains(&page) {
        pages.push(page);
    }
}

#[cfg(windows)]
fn decode_code_page(code_page: u32, bytes: &[u8]) -> Option<String> {
    let len = i32::try_from(bytes.len()).ok()?;
    let needed = unsafe {
        MultiByteToWideChar(
            code_page,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            len,
            null_mut(),
            0,
        )
    };
    if needed <= 0 {
        return None;
    }

    let mut wide = vec![0u16; needed as usize];
    let written = unsafe {
        MultiByteToWideChar(
            code_page,
            MB_ERR_INVALID_CHARS,
            bytes.as_ptr(),
            len,
            wide.as_mut_ptr(),
            needed,
        )
    };
    if written != needed {
        return None;
    }
    String::from_utf16(&wide).ok()
}

#[cfg(windows)]
fn decoded_is_plausible(code_page: u32, decoded: &str) -> bool {
    match code_page {
        932 => decoded.chars().any(|c| is_kana(c) || is_cjk(c)),
        936 | 950 => decoded.chars().any(is_cjk),
        949 => decoded.chars().any(is_hangul),
        874 => decoded.chars().any(is_thai),
        1251 => decoded.chars().any(is_cyrillic),
        1253 => decoded.chars().any(is_greek),
        1255 => decoded.chars().any(is_hebrew),
        1256 => decoded.chars().any(is_arabic),
        _ => decoded.chars().any(is_strong_script_char),
    }
}

#[cfg(windows)]
fn is_strong_script_char(c: char) -> bool {
    is_hangul(c)
        || is_kana(c)
        || is_cjk(c)
        || is_thai(c)
        || is_cyrillic(c)
        || is_greek(c)
        || is_hebrew(c)
        || is_arabic(c)
}

#[cfg(windows)]
fn is_hangul(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7A3)
}

#[cfg(windows)]
fn is_kana(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F)
}

#[cfg(windows)]
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF
    )
}

#[cfg(windows)]
fn is_thai(c: char) -> bool {
    matches!(c as u32, 0x0E00..=0x0E7F)
}

#[cfg(windows)]
fn is_cyrillic(c: char) -> bool {
    matches!(c as u32, 0x0400..=0x052F)
}

#[cfg(windows)]
fn is_greek(c: char) -> bool {
    matches!(c as u32, 0x0370..=0x03FF)
}

#[cfg(windows)]
fn is_hebrew(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x05FF)
}

#[cfg(windows)]
fn is_arabic(c: char) -> bool {
    matches!(c as u32, 0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF)
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

#[cfg(test)]
mod tests {
    use super::normalize_text_input;

    #[test]
    #[cfg(windows)]
    fn repairs_korean_ansi_text_input_mojibake() {
        let mojibake = "\u{00B0}\u{00A1}\u{00B3}\u{00AA}\u{00B4}\u{00D9}\u{00B6}\u{00F3}";
        assert_eq!(
            normalize_text_input(mojibake),
            "\u{AC00}\u{B098}\u{B2E4}\u{B77C}"
        );
    }

    #[test]
    fn leaves_valid_text_input_unchanged() {
        assert_eq!(normalize_text_input("abc"), "abc");
        assert_eq!(normalize_text_input("\u{AC00}\u{B098}"), "\u{AC00}\u{B098}");
    }
}
