//! Glacia's own keys, one aurea `Keymap`. Plain Ctrl is the shell's on every
//! system, so the keys that are bare Command on a mac take Shift elsewhere.

use aurea::{Chord, KeyCode, Keymap};

const APPLE: bool = cfg!(any(target_os = "macos", target_os = "ios"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ZoomIn,
    ZoomOut,
    ZoomReset,
    NewTab,
    ClosePane,
    /// The pane fills the tab, and back.
    ZoomPane,
    EvenSplits,
    SplitRight,
    SplitDown,
    /// Moves the line between panes that way; the arrow key says which.
    NudgeLine(KeyCode),
    /// Goes to the pane that way.
    FocusPane(KeyCode),
    /// Changes tab, `true` to the one before.
    CycleTab(bool),
    /// Swaps the tab with its neighbour, `true` to the one before.
    MoveTab(bool),
    /// Picks a tab by index; the last digit means the last tab.
    PickTab(usize),
    Find,
    Copy,
    Paste,
}

/// Picks tab 1 to 9.
const DIGITS: [KeyCode; 9] = [
    KeyCode::Key1,
    KeyCode::Key2,
    KeyCode::Key3,
    KeyCode::Key4,
    KeyCode::Key5,
    KeyCode::Key6,
    KeyCode::Key7,
    KeyCode::Key8,
    KeyCode::Key9,
];
const ARROWS: [KeyCode; 4] = [KeyCode::Left, KeyCode::Right, KeyCode::Up, KeyCode::Down];

/// The chord for an app key: cmd on a mac, ctrl+shift elsewhere.
const fn app(key: KeyCode) -> Chord {
    if APPLE {
        Chord::command(key)
    } else {
        Chord::command(key).shift()
    }
}

pub fn keymap() -> Keymap<Action> {
    let mut map = Keymap::new();
    map.bind(Chord::ZOOM_IN, Action::ZoomIn)
        .bind(Chord::ZOOM_OUT, Action::ZoomOut)
        .bind(Chord::command(KeyCode::Key0), Action::ZoomReset)
        .bind(app(KeyCode::T), Action::NewTab)
        .bind(app(KeyCode::W), Action::ClosePane)
        .bind(app(KeyCode::F), Action::Find)
        .bind(app(KeyCode::C), Action::Copy)
        .bind(app(KeyCode::V), Action::Paste)
        .bind(Chord::command(KeyCode::Z).shift(), Action::ZoomPane)
        .bind(
            Chord::command(KeyCode::PageUp).shift(),
            Action::MoveTab(true),
        )
        .bind(
            Chord::command(KeyCode::PageDown).shift(),
            Action::MoveTab(false),
        )
        .bind(Chord::key(KeyCode::Tab).ctrl(), Action::CycleTab(false))
        .bind(
            Chord::key(KeyCode::Tab).ctrl().shift(),
            Action::CycleTab(true),
        )
        .bind(Chord::key(KeyCode::Key0).alt().shift(), Action::EvenSplits)
        .bind(
            Chord::key(KeyCode::Equals).alt().shift(),
            Action::SplitRight,
        )
        .bind(Chord::key(KeyCode::Minus).alt().shift(), Action::SplitDown);
    for key in ARROWS {
        map.bind(Chord::key(key).alt().shift(), Action::NudgeLine(key))
            .bind(Chord::key(key).alt(), Action::FocusPane(key));
    }
    for (i, key) in DIGITS.into_iter().enumerate() {
        let chord = if APPLE {
            Chord::command(key)
        } else {
            Chord::key(key).ctrl().alt()
        };
        map.bind(chord, Action::PickTab(i));
    }
    if APPLE {
        // the mac's own way to change tabs
        map.bind(
            Chord::command(KeyCode::LeftBracket).shift(),
            Action::CycleTab(true),
        )
        .bind(
            Chord::command(KeyCode::RightBracket).shift(),
            Action::CycleTab(false),
        );
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chord_is_bound_once() {
        let map = keymap();
        let chords: Vec<Chord> = map.iter().map(|(chord, _)| chord).collect();
        for (i, chord) in chords.iter().enumerate() {
            assert!(!chords[i + 1..].contains(chord), "{chord:?}");
        }
    }

    #[test]
    fn plain_ctrl_stays_the_shells() {
        let map = keymap();
        let plain = map.action(Chord::key(KeyCode::C).ctrl());
        assert_eq!(plain, None, "ctrl+c is ^C on every system");
        assert_eq!(map.action(app(KeyCode::C)), Some(&Action::Copy));
        assert_eq!(map.action(Chord::key(KeyCode::C)), None);
    }

    #[test]
    fn the_app_key_follows_the_system() {
        let copy = app(KeyCode::C).modifiers;
        assert_eq!(copy.meta, APPLE);
        assert_eq!(copy.ctrl, !APPLE);
        assert_eq!(copy.shift, !APPLE);
    }
}
