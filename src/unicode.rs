const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;

pub fn compose_hangul_jamo(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;

    while i < chars.len() {
        let Some(l) = leading_index(chars[i]) else {
            out.push(chars[i]);
            i += 1;
            continue;
        };
        let Some(v) = chars.get(i + 1).and_then(|c| vowel_index(*c)) else {
            out.push(chars[i]);
            i += 1;
            continue;
        };

        let mut consumed = 2;
        let mut t = 0;
        if let Some(next) = chars.get(i + 2).copied()
            && chars.get(i + 3).is_none_or(|c| vowel_index(*c).is_none())
            && let Some(next_t) = trailing_index(next)
        {
            t = next_t;
            consumed = 3;
        }

        let syllable = S_BASE + (l * N_COUNT) + (v * T_COUNT) + t;
        if let Some(ch) = char::from_u32(syllable) {
            out.push(ch);
            i += consumed;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }

    out
}

pub fn is_hangul_jamo(c: char) -> bool {
    leading_index(c).is_some() || vowel_index(c).is_some() || trailing_index(c).is_some()
}

fn leading_index(c: char) -> Option<u32> {
    let n = c as u32;
    if (L_BASE..L_BASE + L_COUNT).contains(&n) {
        return Some(n - L_BASE);
    }
    match c {
        '\u{3131}' => Some(0),
        '\u{3132}' => Some(1),
        '\u{3134}' => Some(2),
        '\u{3137}' => Some(3),
        '\u{3138}' => Some(4),
        '\u{3139}' => Some(5),
        '\u{3141}' => Some(6),
        '\u{3142}' => Some(7),
        '\u{3143}' => Some(8),
        '\u{3145}' => Some(9),
        '\u{3146}' => Some(10),
        '\u{3147}' => Some(11),
        '\u{3148}' => Some(12),
        '\u{3149}' => Some(13),
        '\u{314A}' => Some(14),
        '\u{314B}' => Some(15),
        '\u{314C}' => Some(16),
        '\u{314D}' => Some(17),
        '\u{314E}' => Some(18),
        _ => None,
    }
}

fn vowel_index(c: char) -> Option<u32> {
    let n = c as u32;
    if (V_BASE..V_BASE + V_COUNT).contains(&n) {
        return Some(n - V_BASE);
    }
    match c {
        '\u{314F}' => Some(0),
        '\u{3150}' => Some(1),
        '\u{3151}' => Some(2),
        '\u{3152}' => Some(3),
        '\u{3153}' => Some(4),
        '\u{3154}' => Some(5),
        '\u{3155}' => Some(6),
        '\u{3156}' => Some(7),
        '\u{3157}' => Some(8),
        '\u{3158}' => Some(9),
        '\u{3159}' => Some(10),
        '\u{315A}' => Some(11),
        '\u{315B}' => Some(12),
        '\u{315C}' => Some(13),
        '\u{315D}' => Some(14),
        '\u{315E}' => Some(15),
        '\u{315F}' => Some(16),
        '\u{3160}' => Some(17),
        '\u{3161}' => Some(18),
        '\u{3162}' => Some(19),
        '\u{3163}' => Some(20),
        _ => None,
    }
}

fn trailing_index(c: char) -> Option<u32> {
    let n = c as u32;
    if (T_BASE + 1..T_BASE + T_COUNT).contains(&n) {
        return Some(n - T_BASE);
    }
    match c {
        '\u{3131}' => Some(1),
        '\u{3132}' => Some(2),
        '\u{3133}' => Some(3),
        '\u{3134}' => Some(4),
        '\u{3135}' => Some(5),
        '\u{3136}' => Some(6),
        '\u{3137}' => Some(7),
        '\u{3139}' => Some(8),
        '\u{313A}' => Some(9),
        '\u{313B}' => Some(10),
        '\u{313C}' => Some(11),
        '\u{313D}' => Some(12),
        '\u{313E}' => Some(13),
        '\u{313F}' => Some(14),
        '\u{3140}' => Some(15),
        '\u{3141}' => Some(16),
        '\u{3142}' => Some(17),
        '\u{3144}' => Some(18),
        '\u{3145}' => Some(19),
        '\u{3146}' => Some(20),
        '\u{3147}' => Some(21),
        '\u{3148}' => Some(22),
        '\u{314A}' => Some(23),
        '\u{314B}' => Some(24),
        '\u{314C}' => Some(25),
        '\u{314D}' => Some(26),
        '\u{314E}' => Some(27),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::compose_hangul_jamo;

    #[test]
    fn composes_conjoining_jamo() {
        assert_eq!(compose_hangul_jamo("\u{1100}\u{1161}"), "\u{AC00}");
        assert_eq!(compose_hangul_jamo("\u{1100}\u{1161}\u{11AB}"), "\u{AC04}");
    }

    #[test]
    fn composes_compatibility_jamo() {
        assert_eq!(compose_hangul_jamo("\u{3131}\u{314F}"), "\u{AC00}");
        assert_eq!(compose_hangul_jamo("\u{3131}\u{314F}\u{3134}"), "\u{AC04}");
        assert_eq!(
            compose_hangul_jamo("\u{3131}\u{314F}\u{3134}\u{314F}"),
            "\u{AC00}\u{B098}"
        );
    }
}
