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
    cells: Vec<Vec<Cell>>,
    cursor: (usize, usize),
}

impl TerminalSession {
    pub fn spawn(cols: u16, rows: u16) -> io::Result<Self> {
        let config = SpawnConfig {
            cols,
            rows,
            ..SpawnConfig::default()
        };
        let term = Terminal::spawn_with_config(&config)?;
        Ok(Self {
            term,
            last_version: 0,
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
        self.cells = snapshot.screen;
        true
    }

    pub fn cells(&self) -> &[Vec<Cell>] {
        &self.cells
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
}
