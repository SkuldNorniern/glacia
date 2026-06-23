//! Thin wrapper around `vanta::Terminal`.
//!
//! Glacia's calls into Vanta funnel through here so the migration tracked in
//! `PLAN.md`'s "Vanta API Contract" table only ever touches this one file.

use std::io;

use vanta::pty::SpawnConfig;
use vanta::{Cell, Terminal};

pub struct TerminalSession {
    term: Terminal,
    last_version: u64,
    scrollback: Vec<Vec<Cell>>,
    cells: Vec<Vec<Cell>>,
    cursor: (usize, usize),
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
            program: (!overrides.shell.is_empty()).then(|| overrides.shell.to_owned()),
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
        self.cursor = (line.saturating_sub(snapshot.scrollback.len()), col);
        self.scrollback = snapshot.scrollback;
        self.cells = snapshot.screen;
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

    pub fn write_str(&self, s: &str) -> io::Result<()> {
        self.term.write_str(s)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
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
