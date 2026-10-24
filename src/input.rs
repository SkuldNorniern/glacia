//! Maps Aurea key events to PTY-bound byte sequences.
//!
//! Returns `Option<String>` so callers can forward the bytes unchanged with a
//! single `write_str` call.

use aurea::{KeyCode, Modifiers};
#[cfg(windows)]
use std::ptr::null_mut;

use crate::unicode::compose_hangul_jamo;
#[cfg(not(windows))]
use crate::unicode::is_hangul_jamo;

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
        KeyCode::Tab if mods.shift => return Some("\x1b[Z".to_owned()),
        KeyCode::Tab => "\t".to_owned(),
        KeyCode::Escape => "\x1b".to_owned(),
        KeyCode::Space if mods.ctrl => "\0".to_owned(),
        _ => {
            let c = letter(key)?;
            if mods.ctrl {
                // Ctrl+[A-Z] = codepoint - 64
                char::from(c.to_ascii_uppercase() as u8 - b'@').to_string()
            } else if mods.alt {
                // Alt+letter sends ESC + the lowercase letter regardless of
                // shift, like xterm in its default meta mode
                c.to_string()
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

#[derive(Default)]
pub struct TextInputNormalizer {
    #[cfg(windows)]
    pending_ansi: Vec<u8>,
    #[cfg(not(windows))]
    pending_jamo: String,
}

impl TextInputNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn normalize(&mut self, text: &str) -> String {
        #[cfg(windows)]
        {
            normalize_windows_text_input(&mut self.pending_ansi, text)
        }
        #[cfg(not(windows))]
        {
            normalize_unix_text_input(&mut self.pending_jamo, text)
        }
    }

    pub fn flush(&mut self) -> String {
        #[cfg(windows)]
        {
            // a lone byte held for a trail that never came goes out as it is
            self.pending_ansi.drain(..).map(char::from).collect()
        }
        #[cfg(not(windows))]
        {
            let out = compose_hangul_jamo(&self.pending_jamo).into_owned();
            self.pending_jamo.clear();
            out
        }
    }
}

#[cfg(test)]
pub fn normalize_text_input(text: &str) -> String {
    #[cfg(windows)]
    {
        repair_windows_ansi_mojibake(text).unwrap_or_else(|| text.to_owned())
    }
    #[cfg(not(windows))]
    {
        compose_hangul_jamo(text).into_owned()
    }
}

#[cfg(not(windows))]
fn normalize_unix_text_input(pending_jamo: &mut String, text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    if text.chars().all(is_hangul_jamo) {
        pending_jamo.push_str(text);
        let composed = compose_hangul_jamo(pending_jamo);
        if composed != pending_jamo.as_str() {
            pending_jamo.clear();
            return composed.into_owned();
        }
        return String::new();
    }

    let mut out = String::new();
    if !pending_jamo.is_empty() {
        out.push_str(&compose_hangul_jamo(pending_jamo));
        pending_jamo.clear();
    }
    out.push_str(&compose_hangul_jamo(text));
    out
}

#[cfg(windows)]
fn normalize_windows_text_input(pending: &mut Vec<u8>, text: &str) -> String {
    let preferred_pages = preferred_ansi_code_pages();
    let mut out = String::new();

    for ch in text.chars() {
        let code = ch as u32;
        if code > 0xFF || is_strong_script_char(ch) {
            push_pending_original(&mut out, pending);
            out.push(ch);
            continue;
        }

        let byte = code as u8;
        if byte < 0x80 && pending.is_empty() {
            out.push(ch);
            continue;
        }

        pending.push(byte);

        if pending.len() == 1 && should_wait_for_ansi_trail_byte(pending[0], &preferred_pages) {
            continue;
        }

        if let Some(decoded) = repair_windows_ansi_bytes(pending, &candidate_ansi_code_pages()) {
            out.push_str(&decoded);
            pending.clear();
            continue;
        }

        if pending.len() >= 2 {
            push_pending_original(&mut out, pending);
        }
    }

    compose_hangul_jamo(&out).into_owned()
}

#[cfg(all(windows, test))]
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

    repair_windows_ansi_bytes(&bytes, &candidate_ansi_code_pages())
}

#[cfg(windows)]
fn repair_windows_ansi_bytes(bytes: &[u8], code_pages: &[u32]) -> Option<String> {
    let original: String = bytes.iter().copied().map(char::from).collect();
    let mut best: Option<(u32, String)> = None;

    for &code_page in code_pages {
        let Some(decoded) = decode_code_page(code_page, bytes) else {
            continue;
        };
        if decoded == original {
            continue;
        }

        let score = decoded_score(code_page, &decoded, bytes.len());
        if score == 0 {
            continue;
        }

        let should_replace = best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score);
        if should_replace {
            best = Some((score, decoded));
        }
    }

    best.map(|(_, decoded)| decoded)
}

#[cfg(windows)]
fn preferred_ansi_code_pages() -> Vec<u32> {
    let mut pages = Vec::new();
    if let Some(page) = keyboard_layout_ansi_code_page() {
        push_unique(&mut pages, page);
    }
    push_unique(&mut pages, unsafe { GetACP() });
    pages
}

#[cfg(windows)]
fn candidate_ansi_code_pages() -> Vec<u32> {
    let mut pages = preferred_ansi_code_pages();
    for &page in COMMON_TEXT_INPUT_CODEPAGES {
        push_unique(&mut pages, page);
    }
    pages
}

#[cfg(windows)]
fn should_wait_for_ansi_trail_byte(byte: u8, preferred_pages: &[u32]) -> bool {
    preferred_pages
        .iter()
        .any(|&code_page| is_dbcs_lead_byte(code_page, byte))
}

#[cfg(windows)]
fn is_dbcs_lead_byte(code_page: u32, byte: u8) -> bool {
    match code_page {
        932 => matches!(byte, 0x81..=0x9F | 0xE0..=0xFC),
        936 | 949 | 950 => matches!(byte, 0x81..=0xFE),
        _ => false,
    }
}

#[cfg(windows)]
fn push_pending_original(out: &mut String, pending: &mut Vec<u8>) {
    out.extend(pending.drain(..).map(char::from));
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
fn decoded_score(code_page: u32, decoded: &str, byte_len: usize) -> u32 {
    let mut score: u32 = decoded.chars().map(|c| script_score(code_page, c)).sum();
    let char_count = decoded.chars().count();
    if score > 0 && byte_len > char_count {
        score += (byte_len - char_count) as u32 * 8;
    }
    score
}

#[cfg(windows)]
fn script_score(code_page: u32, c: char) -> u32 {
    match code_page {
        932 if is_kana(c) => 6,
        932 if is_cjk(c) => 7,
        936 | 950 if is_cjk(c) => 7,
        949 if is_hangul(c) => 8,
        874 if is_thai(c) => 7,
        1251 if is_cyrillic(c) => 7,
        1253 if is_greek(c) => 7,
        1255 if is_hebrew(c) => 7,
        1256 if is_arabic(c) => 7,
        _ if is_strong_script_char(c) => 1,
        _ => 0,
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

#[cfg(test)]
mod tests {
    use super::{TextInputNormalizer, normalize_text_input, terminal_key_bytes};
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
        assert_eq!(key(KeyCode::X, false, true, true, false), "\x1b\x18");
        assert_eq!(key(KeyCode::Space, false, true, false, false), "\0");
        assert_eq!(
            key(KeyCode::A, false, false, false, false),
            "",
            "letters come as text"
        );
    }

    #[test]
    #[cfg(windows)]
    fn flush_lets_a_held_byte_out() {
        let mut normalizer = TextInputNormalizer::new();
        let held = normalizer.normalize("\u{00B0}");
        let flushed = normalizer.flush();
        assert_eq!(format!("{held}{flushed}"), "\u{00B0}");
        assert_eq!(normalizer.flush(), "");
    }

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
    #[cfg(windows)]
    fn repairs_split_korean_ansi_text_input_mojibake() {
        let mut normalizer = TextInputNormalizer::new();
        let events = [
            "\u{00B0}", "\u{00A1}", "\u{00B3}", "\u{00AA}", "\u{00B4}", "\u{00DE}", " ",
            "\u{00A4}", "\u{00BF}",
        ];
        let repaired: String = events
            .into_iter()
            .map(|event| normalizer.normalize(event))
            .collect();

        assert_eq!(repaired, "\u{AC00}\u{B098}\u{B2EC} \u{314F}");
    }

    #[test]
    fn leaves_valid_text_input_unchanged() {
        assert_eq!(normalize_text_input("abc"), "abc");
        assert_eq!(normalize_text_input("\u{AC00}\u{B098}"), "\u{AC00}\u{B098}");
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
    #[cfg(not(windows))]
    fn buffers_split_hangul_jamo_text_input() {
        let mut normalizer = TextInputNormalizer::new();
        assert_eq!(normalizer.normalize("\u{1100}"), "");
        assert_eq!(normalizer.normalize("\u{1161}"), "\u{AC00}");
        assert_eq!(normalizer.normalize("\u{1102}"), "");
        assert_eq!(normalizer.normalize("\u{1161}"), "\u{B098}");
    }

    #[test]
    #[cfg(not(windows))]
    fn flushes_pending_hangul_jamo_text_input() {
        let mut normalizer = TextInputNormalizer::new();
        assert_eq!(normalizer.normalize("\u{1100}"), "");
        assert_eq!(normalizer.flush(), "\u{1100}");
    }
}
