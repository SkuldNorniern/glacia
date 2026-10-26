//! Thin wrapper around `vanta::Terminal`.
//!
//! Glacia's calls into Vanta funnel through here, so a Vanta API change
//! only touches this one file.

use std::ffi::OsString;
use std::io;
use std::time::Instant;

use vanta::pty::SpawnConfig;
use vanta::{
    Cell, CellKind, CursorStyle, Event, Found, MouseProtocol, MouseTracking, Palette, Terminal,
    Window,
};

pub struct TerminalSession {
    term: Terminal,
    last_version: u64,
    /// What the view shows: `screen_rows` lines ending `back` lines above
    /// the bottom. Only these lines are copied out of Vanta, never all of
    /// scrollback.
    view: Window,
    back: usize,
    screen_cols: usize,
    screen_rows: usize,
}

/// Overrides for the spawned shell. Empty `shell`/`working_directory` mean
/// "use the platform default" / "inherit the current directory".
pub struct SpawnOverrides<'a> {
    pub cols: u16,
    pub rows: u16,
    pub shell: &'a str,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub working_directory: &'a str,
}

impl TerminalSession {
    /// Spawns the shell. `wake` runs on Vanta's reader thread after each
    /// chunk of output and at the end, so the event loop can sleep until then.
    pub fn spawn(
        overrides: SpawnOverrides<'_>,
        wake: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let config = SpawnConfig {
            cols: overrides.cols,
            rows: overrides.rows,
            program: (!overrides.shell.is_empty()).then(|| OsString::from(overrides.shell)),
            args: overrides.args,
            env: overrides.env,
            cwd: (!overrides.working_directory.is_empty())
                .then(|| overrides.working_directory.into()),
            ..SpawnConfig::default()
        };
        let term = Terminal::spawn_notify(&config, wake)?;
        let view = term.view(0, overrides.rows as usize);
        Ok(Self {
            term,
            last_version: 0,
            view,
            back: 0,
            screen_cols: overrides.cols as usize,
            screen_rows: overrides.rows as usize,
        })
    }

    /// Reads the view again when the shell wrote something or the view
    /// moved. `back` is how far it is scrolled up; it is held inside
    /// scrollback. Returns whether there was new output.
    pub fn sync(&mut self, back: usize) -> bool {
        let version = self.term.version();
        let fresh = version != self.last_version;
        if fresh || back != self.back {
            self.view = self.term.view(back, self.screen_rows);
            self.back = back.min(self.scrollback_len());
            self.last_version = self.view.version;
        }
        fresh
    }

    /// The lines on view, top first.
    pub fn rows(&self) -> &[Vec<Cell>] {
        &self.view.rows
    }

    /// Whether row `row` on view runs on into the next (it wrapped).
    pub fn wrapped(&self, row: usize) -> bool {
        self.view.wrapped.get(row).copied().unwrap_or(false)
    }

    /// The line number of the top row on view.
    pub fn first(&self) -> usize {
        self.view.first
    }

    /// Lines in scrollback, above the screen.
    pub fn scrollback_len(&self) -> usize {
        self.view.lines.saturating_sub(self.screen_rows)
    }

    /// The cursor's `(row, col)` on view, when it is on view. A cursor
    /// waiting to wrap past the last column shows on that column.
    pub fn cursor(&self) -> Option<(usize, usize)> {
        let (line, col) = self.view.cursor;
        let row = line.checked_sub(self.view.first)?;
        (row < self.view.rows.len().max(self.screen_rows))
            .then(|| (row, col.min(self.screen_cols.saturating_sub(1))))
    }

    /// Text of lines `first..=last`, from `start_col` on the first and up
    /// to `end_col` on the last. Rows that wrapped join without a break.
    pub fn text(
        &self,
        (first, start_col): (usize, usize),
        (last, end_col): (usize, usize),
    ) -> String {
        let count = last.saturating_sub(first) + 1;
        let window = self.term.window(first, count);
        let mut out = String::new();
        for (i, row) in window.rows.iter().enumerate() {
            let from = if i == 0 { start_col } else { 0 };
            let to = if i + 1 == count {
                end_col.saturating_add(1)
            } else {
                usize::MAX
            };
            let mut line = String::new();
            for cell in row.iter().take(to).skip(from) {
                match &cell.kind {
                    CellKind::Char(c) => line.push(*c),
                    CellKind::Cluster(s) => line.push_str(s),
                    CellKind::Empty => line.push(' '),
                    CellKind::Continuation => {}
                }
            }
            let wrapped = window.wrapped.get(i).copied().unwrap_or(false);
            if wrapped {
                out.push_str(&line);
            } else {
                out.push_str(line.trim_end_matches(' '));
                if i + 1 < count {
                    out.push('\n');
                }
            }
        }
        out
    }

    /// Whether the app has requested the cursor be shown (DECTCEM).
    /// TUI apps like vim hide it to avoid a second cursor block.
    pub fn app_cursor_visible(&self) -> bool {
        self.view.cursor_visible
    }

    /// The cursor shape the app asked for (DECSCUSR).
    pub fn cursor_style(&self) -> CursorStyle {
        self.view.cursor_style
    }

    /// Whether the terminal app is on the alternate screen (DECSET 47/1049).
    /// When true, Glacia's scrollback viewport should be suppressed — the app
    /// manages its own scrolling (vim, htop, less, etc.).
    pub fn is_alt_screen(&self) -> bool {
        self.view.is_alt_screen
    }

    /// Whether bracketed paste mode (DECSET 2004) is enabled.
    /// When true, clipboard pastes must be wrapped in ESC[200~ … ESC[201~.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.view.bracketed_paste
    }

    /// Lines dropped off the top of scrollback so far. With a line number
    /// it makes a position that stays on the same text as lines drop.
    pub fn dropped(&self) -> u64 {
        self.view.dropped
    }

    /// Whether the app asked for application cursor keys (DECCKM): arrows
    /// then go as `ESC O x`.
    pub fn application_cursor_keys(&self) -> bool {
        self.view.application_cursor_keys
    }

    pub fn mouse(&self) -> (MouseTracking, MouseProtocol) {
        (self.view.mouse_tracking, self.view.mouse_protocol)
    }

    /// Whether the app wants to hear about focus in and out (DECSET 1004).
    pub fn focus_tracking(&self) -> bool {
        self.view.focus_tracking
    }

    /// Bumped when an app changes colours (OSC 4 / 10 / 11).
    pub fn palette_version(&self) -> u64 {
        self.view.palette_version
    }

    pub fn palette(&self) -> Palette {
        self.term.palette()
    }

    /// The colours Glacia draws with, for apps that ask (vim does, over ssh,
    /// to pick light or dark).
    pub fn set_palette(&self, palette: Palette) {
        self.term.set_palette(palette);
    }

    /// Searches for `needle` on a thread; `done` gets every match and
    /// the dropped count they were found at.
    pub fn find_in_background(
        &self,
        needle: String,
        done: impl FnOnce((u64, Vec<Found>)) + Send + 'static,
    ) {
        self.term.find_all_in_background(needle, done);
    }

    /// Bells and clipboard writes from the app.
    pub fn take_events(&self) -> Vec<Event> {
        self.term.take_events()
    }

    /// When a synchronized update stops holding the screen back; the loop
    /// should wake by then.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.term.sync_deadline()
    }

    pub fn write_str(&self, s: &str) -> io::Result<()> {
        self.term.write_str(s)
    }

    pub fn write_bytes(&self, bytes: &[u8]) -> io::Result<()> {
        self.term.write_bytes(bytes)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        // the same size again would rewrap and tell the app for nothing
        if (cols as usize, rows as usize) == (self.screen_cols, self.screen_rows) {
            return Ok(());
        }
        self.screen_cols = cols as usize;
        self.screen_rows = rows as usize;
        let result = self.term.resize(cols, rows);
        self.view = self.term.view(self.back, self.screen_rows);
        self.last_version = self.view.version;
        result
    }

    /// The shell's output ended: it exited, or is about to.
    pub fn closed(&self) -> bool {
        self.term.is_closed()
    }

    pub fn is_running(&self) -> bool {
        self.term.is_running()
    }

    /// The shell's directory, when it reports it (osc 7).
    pub fn cwd(&self) -> Option<String> {
        self.term.cwd()
    }

    /// The shell/app's OSC 0/2 title, if it has set one.
    pub fn title(&self) -> Option<String> {
        self.term.title()
    }
}
