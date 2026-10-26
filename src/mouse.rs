//! Mouse reports for apps that asked for them (tmux, vim, htop, mc), in
//! the encodings xterm offers.

use aurea::Modifiers;
use vanta::{MouseProtocol, MouseTracking};

/// What happened to the button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Press,
    Release,
    Motion,
}

/// Button codes as reports carry them.
pub const LEFT: u8 = 0;
pub const MIDDLE: u8 = 1;
pub const RIGHT: u8 = 2;
/// Motion with no button down.
pub const NONE: u8 = 3;
pub const WHEEL_UP: u8 = 64;
pub const WHEEL_DOWN: u8 = 65;

/// Whether an app in `tracking` mode wants to hear about this.
pub fn wanted(tracking: MouseTracking, kind: Kind, held: bool) -> bool {
    match (tracking, kind) {
        (MouseTracking::Off, _) => false,
        (_, Kind::Press | Kind::Release) => true,
        (MouseTracking::Drag, Kind::Motion) => held,
        (MouseTracking::Any, Kind::Motion) => true,
        (MouseTracking::Press, Kind::Motion) => false,
    }
}

/// The bytes for one report at 0-based `(col, row)`; none when the old
/// encodings can't reach that far.
pub fn report(
    protocol: MouseProtocol,
    button: u8,
    kind: Kind,
    mods: Modifiers,
    (col, row): (usize, usize),
) -> Option<Vec<u8>> {
    let mut code =
        button + 4 * u8::from(mods.shift) + 8 * u8::from(mods.alt) + 16 * u8::from(mods.ctrl);
    if kind == Kind::Motion {
        code += 32;
    }
    let (x, y) = (col + 1, row + 1);
    if protocol == MouseProtocol::Sgr {
        // the only one that says which button went up
        let end = if kind == Kind::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    if kind == Kind::Release {
        // the rest only know "a button went up"
        code = (code & !3) | NONE;
    }
    match protocol {
        MouseProtocol::Urxvt => {
            Some(format!("\x1b[{};{x};{y}M", u32::from(code) + 32).into_bytes())
        }
        MouseProtocol::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for v in [usize::from(code), x, y] {
                let c = char::from_u32(u32::try_from(v + 32).ok()?).filter(|_| v + 32 < 2048)?;
                let mut buf = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
        _ => {
            let byte = |v: usize| u8::try_from(v + 32).ok();
            Some(vec![
                0x1b,
                b'[',
                b'M',
                byte(code.into())?,
                byte(x)?,
                byte(y)?,
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> Modifiers {
        Modifiers::default()
    }

    #[test]
    fn sgr_reports_press_release_and_drag() {
        let p = MouseProtocol::Sgr;
        assert_eq!(
            report(p, LEFT, Kind::Press, plain(), (0, 0)).unwrap(),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            report(p, LEFT, Kind::Release, plain(), (9, 4)).unwrap(),
            b"\x1b[<0;10;5m"
        );
        assert_eq!(
            report(p, LEFT, Kind::Motion, plain(), (2, 3)).unwrap(),
            b"\x1b[<32;3;4M"
        );
        let ctrl = Modifiers {
            ctrl: true,
            ..plain()
        };
        assert_eq!(
            report(p, WHEEL_UP, Kind::Press, ctrl, (0, 0)).unwrap(),
            b"\x1b[<80;1;1M"
        );
        assert_eq!(
            report(p, RIGHT, Kind::Press, plain(), (999, 999)).unwrap(),
            b"\x1b[<2;1000;1000M"
        );
    }

    #[test]
    fn old_encodings_say_release_and_stop_at_their_edge() {
        let p = MouseProtocol::X10;
        assert_eq!(
            report(p, LEFT, Kind::Press, plain(), (0, 0)).unwrap(),
            b"\x1b[M\x20\x21\x21"
        );
        assert_eq!(
            report(p, RIGHT, Kind::Release, plain(), (0, 0)).unwrap(),
            b"\x1b[M\x23\x21\x21"
        );
        assert_eq!(report(p, LEFT, Kind::Press, plain(), (300, 0)), None);
        let u = MouseProtocol::Utf8;
        assert_eq!(
            report(u, LEFT, Kind::Press, plain(), (300, 0)).unwrap(),
            "\x1b[M\u{20}\u{14d}\u{21}".as_bytes()
        );
        let r = MouseProtocol::Urxvt;
        assert_eq!(
            report(r, MIDDLE, Kind::Press, plain(), (4, 5)).unwrap(),
            b"\x1b[33;5;6M"
        );
    }

    #[test]
    fn each_mode_wants_its_own_events() {
        use MouseTracking::*;
        assert!(!wanted(Off, Kind::Press, false));
        assert!(wanted(Press, Kind::Release, false));
        assert!(!wanted(Press, Kind::Motion, true));
        assert!(wanted(Drag, Kind::Motion, true));
        assert!(!wanted(Drag, Kind::Motion, false));
        assert!(wanted(Any, Kind::Motion, false));
    }
}
