//! Thin wrapper around `vanta::Terminal`.
//!
//! Glacia's calls into Vanta funnel through here so the migration tracked in
//! `PLAN.md`'s "Vanta API Contract" table only ever touches this one file.

use std::ffi::OsString;
use std::io;

use vanta::pty::SpawnConfig;
use vanta::{Cell, Terminal};

pub struct TerminalSession {
    term: Terminal,
    last_version: u64,
    scrollback: Vec<Vec<Cell>>,
    cells: Vec<Vec<Cell>>,
    cursor: (usize, usize),
    /// Cached terminal width so `sync` can clamp the cursor column when the
    /// VT is in "pending wrap" state (cx == cols after filling the last cell).
    screen_cols: usize,
    cursor_visible: bool,
    is_alt_screen: bool,
    bracketed_paste: bool,
}

/// Overrides for the spawned shell. Empty `shell`/`working_directory` mean
/// "use the platform default" / "inherit the current directory".
pub struct SpawnOverrides<'a> {
    pub cols: u16,
    pub rows: u16,
    pub shell: &'a str,
    pub working_directory: &'a str,
}

impl TerminalSession {
    pub fn spawn(overrides: SpawnOverrides<'_>) -> io::Result<Self> {
        let config = SpawnConfig {
            cols: overrides.cols,
            rows: overrides.rows,
            program: (!overrides.shell.is_empty()).then(|| OsString::from(overrides.shell)),
            cwd: (!overrides.working_directory.is_empty())
                .then(|| overrides.working_directory.into()),
            ..SpawnConfig::default()
        };
        let term = Terminal::spawn_with_config(&config)?;
        Ok(Self {
            term,
            last_version: 0,
            scrollback: Vec::new(),
            cells: Vec::new(),
            cursor: (0, 0),
            screen_cols: overrides.cols as usize,
            cursor_visible: true,
            is_alt_screen: false,
            bracketed_paste: false,
        })
    }

    /// Refresh the cached snapshot if the terminal produced new output.
    /// Returns whether the cache actually changed. Caches the visible screen
    /// only, not scrollback, with the cursor row rebased to screen-relative —
    /// scrollback viewport rendering is a near-term follow-up, not this slice.
    pub fn sync(&mut self) -> bool {
        let snapshot = self.term.snapshot();
        if snapshot.version == self.last_version {
            return false;
        }
        self.last_version = snapshot.version;
        let (line, col) = snapshot.cursor;
        // Vanta's cx reaches `cols` in "pending wrap" state (last column filled,
        // wrap not yet triggered). Clamp so the cursor block stays on-screen.
        let col = col.min(self.screen_cols.saturating_sub(1));
        self.cursor = (line.saturating_sub(snapshot.scrollback.len()), col);
        self.scrollback = snapshot.scrollback;
        self.cells = snapshot.screen;
        self.cursor_visible = snapshot.cursor_visible;
        self.is_alt_screen = snapshot.is_alt_screen;
        self.bracketed_paste = snapshot.bracketed_paste;
        true
    }

    pub fn cells(&self) -> &[Vec<Cell>] {
        &self.cells
    }

    pub fn scrollback_rows(&self) -> &[Vec<Cell>] {
        &self.scrollback
    }

    pub fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// Whether the app has requested the cursor be shown (DECTCEM).
    /// TUI apps like vim hide it to avoid a second cursor block.
    pub fn app_cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Whether the terminal app is on the alternate screen (DECSET 47/1049).
    /// When true, Glacia's scrollback viewport should be suppressed — the app
    /// manages its own scrolling (vim, htop, less, etc.).
    pub fn is_alt_screen(&self) -> bool {
        self.is_alt_screen
    }

    /// Whether bracketed paste mode (DECSET 2004) is enabled.
    /// When true, clipboard pastes must be wrapped in ESC[200~ … ESC[201~.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste
    }

    pub fn write_str(&self, s: &str) -> io::Result<()> {
        self.term.write_str(s)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        self.screen_cols = cols as usize;
        self.term.resize(cols, rows)
    }

    pub fn is_running(&self) -> bool {
        self.term.is_running()
    }

    /// The shell/app's OSC 0/2 title, if it has set one.
    pub fn title(&self) -> Option<String> {
        self.term.title()
    }
}
