//! One terminal window: the shell session, what the view shows, and how
//! window events change it.

use std::time::{Duration, Instant};

use aurea::render::Canvas;
use aurea::{AureaResult, KeyCode, Modifiers, MouseButton, Window, WindowEvent};
use aurea::{clipboard_text, set_clipboard_text};
use vanta::{Cell, CellKind};

use crate::config::Config;
use crate::input::{self, TextInputNormalizer};
use crate::render::{self, CellMetrics, CellRowsView, RowFonts, SelectionRange};
use crate::terminal::TerminalSession;

const POLL_INTERVAL: Duration = Duration::from_millis(8);
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667); // 60 FPS render cap
pub const DEFAULT_TITLE: &str = "Glacia";

pub fn grid_size(width: u32, height: u32, metrics: &CellMetrics, padding: u32) -> (u16, u16) {
    let w = width.saturating_sub(2 * padding) as f32;
    let h = height.saturating_sub(2 * padding) as f32;
    let cols = (w / metrics.width).max(1.0) as u16;
    let rows = (h / metrics.height).max(1.0) as u16;
    (cols, rows)
}

/// Convert a pixel position to a (row, col) cell index, clamping to the grid.
fn px_to_cell(x: f64, y: f64, metrics: &CellMetrics, padding: f32) -> (usize, usize) {
    let col = ((x as f32 - padding) / metrics.width).max(0.0) as usize;
    let row = ((y as f32 - padding) / metrics.height).max(0.0) as usize;
    (row, col)
}

/// Clipboard text made safe to paste: no paste-end marker that would let
/// the rest run as typed commands, and line breaks as the CR Enter sends.
fn paste_text(text: &str) -> String {
    text.replace("\x1b[200~", "")
        .replace("\x1b[201~", "")
        .replace("\r\n", "\r")
        .replace('\n', "\r")
}

/// Extract plain text for the cells covered by `sel` from the given rows.
fn extract_selection_text(rows: &[Vec<Cell>], sel: SelectionRange) -> String {
    let mut result = String::new();
    let (r1, _) = sel.start;
    let (r2, _) = sel.end;
    for row_idx in r1..=r2 {
        let Some(row) = rows.get(row_idx) else { break };
        let Some((sc, ec)) = sel.cols_for_row(row_idx, row.len()) else {
            continue;
        };
        let mut line = String::new();
        let end = ec.min(row.len().saturating_sub(1));
        for cell in row.iter().skip(sc).take(end.saturating_sub(sc) + 1) {
            match &cell.kind {
                CellKind::Char(c) => line.push(*c),
                CellKind::Cluster(s) => line.push_str(s),
                CellKind::Empty => line.push(' '),
                CellKind::Continuation => {}
            }
        }
        result.push_str(line.trim_end_matches(' '));
        if row_idx < r2 {
            result.push('\n');
        }
    }
    result
}

/// `cmd.exe`'s default, un-customized console title is its own full exe
/// path (e.g. `C:\WINDOWS\system32\cmd.exe`) — collapse that to a bare file
/// name. Anything else (an explicit `title` call, WSL/zsh's `user@host: path`
/// prompts) doesn't match this shape and passes through unchanged, since
/// it's already meaningful.
fn display_shell_name(raw: &str) -> &str {
    let lower = raw.to_ascii_lowercase();
    if !raw.contains('\\') || !lower.ends_with(".exe") {
        return raw;
    }
    let base = raw.rsplit('\\').next().unwrap_or(raw);
    &base[..base.len() - 4]
}

/// Whether the wheel moved toward older lines. The sign differs per native
/// backend: NSEvent (natural scroll) and WM_MOUSEWHEEL report positive for
/// scrolling up, GTK reports negative for GDK_SCROLL_UP.
fn wheel_scrolls_up(delta_y: f64) -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        delta_y > 0.0
    } else {
        delta_y < 0.0
    }
}

pub struct App {
    config: Config,
    term: TerminalSession,
    metrics: CellMetrics,
    fonts: RowFonts,
    diagnostics: Vec<String>,
    window_size: (u32, u32),
    text_input: TextInputNormalizer,
    /// Rows scrolled back into history; 0 is the live screen.
    scroll_offset: usize,
    /// Mouse selection: where the drag started and where it is now.
    sel_anchor: Option<(usize, usize)>,
    sel_end: Option<(usize, usize)>,
    mouse_dragging: bool,
    cursor_visible: bool,
    last_blink: Instant,
    /// When output last arrived; the cursor holds still until the shell has
    /// been idle a full blink interval, so heavy output does not flicker it.
    last_output: Instant,
    last_render: Instant,
    last_title: Option<String>,
    needs_redraw: bool,
}

impl App {
    pub fn new(
        config: Config,
        term: TerminalSession,
        metrics: CellMetrics,
        fonts: RowFonts,
        diagnostics: Vec<String>,
    ) -> Self {
        let now = Instant::now();
        Self {
            window_size: (config.window.width, config.window.height),
            config,
            term,
            metrics,
            fonts,
            diagnostics,
            text_input: TextInputNormalizer::new(),
            scroll_offset: 0,
            sel_anchor: None,
            sel_end: None,
            mouse_dragging: false,
            cursor_visible: true,
            last_blink: now,
            last_output: now,
            last_render: now,
            last_title: None,
            needs_redraw: true,
        }
    }

    /// Runs until the window closes or the shell exits.
    pub fn run(mut self, window: &Window, canvas: &mut Canvas) -> AureaResult<()> {
        loop {
            // Input ends the wait at once; otherwise it lasts until the next
            // frame is due, or a poll interval for new shell output.
            let timeout = if self.needs_redraw {
                FRAME_INTERVAL.saturating_sub(self.last_render.elapsed())
            } else {
                POLL_INTERVAL
            };
            let mut close = false;
            for event in window.wait_events(Some(timeout)) {
                close |= self.on_event(&event);
            }
            if close {
                return Ok(());
            }
            self.follow_canvas_size(canvas);
            self.pump_shell(window);
            if !self.term.is_running() {
                return Ok(());
            }
            if self.needs_redraw && self.last_render.elapsed() >= FRAME_INTERVAL {
                self.draw(canvas)?;
            }
        }
    }

    fn padding(&self) -> u32 {
        self.config.window.padding
    }

    fn resize_grid(&mut self, width: u32, height: u32) {
        self.window_size = (width, height);
        let (cols, rows) = grid_size(width, height, &self.metrics, self.padding());
        let _ = self.term.resize(cols, rows);
        self.needs_redraw = true;
    }

    fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_end = None;
    }

    /// Shows the cursor solid again after the user typed.
    fn wake_cursor(&mut self) {
        self.cursor_visible = true;
        self.last_blink = Instant::now();
        self.needs_redraw = true;
    }

    /// Writes `text` to the shell, after any half-composed input.
    fn send(&mut self, text: &str) {
        let pending = self.text_input.flush();
        if !pending.is_empty() {
            let _ = self.term.write_str(&pending);
        }
        let _ = self.term.write_str(text);
        self.wake_cursor();
    }

    /// Returns true when the window asked to close.
    fn on_event(&mut self, event: &WindowEvent) -> bool {
        match event {
            WindowEvent::CloseRequested => return true,
            WindowEvent::Resized { width, height } => {
                self.resize_grid(*width, *height);
                self.scroll_offset = 0;
                self.clear_selection();
                // Draw the new size at once rather than a frame later.
                self.last_render = Instant::now()
                    .checked_sub(FRAME_INTERVAL)
                    .unwrap_or(Instant::now());
            }
            WindowEvent::ScaleFactorChanged { .. } => self.needs_redraw = true,
            WindowEvent::MouseButton {
                button: MouseButton::Left,
                pressed,
                x,
                y,
                ..
            } => self.on_left_button(*pressed, *x, *y),
            WindowEvent::MouseMove { x, y, .. } if self.mouse_dragging => {
                let pad = self.padding() as f32;
                let cell = px_to_cell(*x, *y, &self.metrics, pad);
                // a selection starts once the pointer leaves the cell it
                // was pressed in; a plain click selects nothing
                if self.sel_end.is_some() || self.sel_anchor != Some(cell) {
                    self.sel_end = Some(cell);
                    self.needs_redraw = true;
                }
            }
            WindowEvent::KeyInput {
                key,
                pressed: true,
                modifiers,
            } => self.on_key(*key, *modifiers),
            WindowEvent::MouseWheel { delta_y, .. } => self.on_wheel(*delta_y),
            WindowEvent::TextInput { text } => self.on_text(text),
            _ => {}
        }
        false
    }

    fn on_left_button(&mut self, pressed: bool, x: f64, y: f64) {
        if pressed {
            let cell = px_to_cell(x, y, &self.metrics, self.padding() as f32);
            self.sel_anchor = Some(cell);
            self.sel_end = None;
        }
        self.mouse_dragging = pressed;
        self.needs_redraw = true;
    }

    fn on_key(&mut self, key: KeyCode, modifiers: Modifiers) {
        let copy_paste = modifiers.ctrl && modifiers.shift;
        if copy_paste && key == KeyCode::C {
            self.copy_selection();
        } else if copy_paste && key == KeyCode::V {
            self.paste();
        } else if self.term.is_alt_screen() {
            // the app owns scrolling: every key goes to it
            self.send_key(key, modifiers);
        } else if !(modifiers.shift && self.on_history_key(key)) {
            // scrollback is shift+page/home/end; plain ones go to the shell
            self.send_key(key, modifiers);
        }
    }

    fn copy_selection(&self) {
        let Some((anchor, end)) = self.sel_anchor.zip(self.sel_end) else {
            return;
        };
        let text = extract_selection_text(self.term.cells(), SelectionRange::new(anchor, end));
        if !text.is_empty() {
            let _ = set_clipboard_text(&text);
        }
    }

    /// Pastes the clipboard, bracketed when the app asked for it.
    fn paste(&mut self) {
        let Some(text) = clipboard_text() else {
            return;
        };
        self.scroll_offset = 0;
        let text = paste_text(&text);
        if self.term.bracketed_paste_enabled() {
            self.send(&format!("\x1b[200~{text}\x1b[201~"));
        } else {
            self.send(&text);
        }
    }

    /// Scrollback keys on the normal screen. Returns false for a key that
    /// should go to the shell instead.
    fn on_history_key(&mut self, key: KeyCode) -> bool {
        let page = (self.visible_rows() / 2).max(1);
        let max_scroll = self.term.scrollback_rows().len();
        self.scroll_offset = match key {
            KeyCode::PageUp => (self.scroll_offset + page).min(max_scroll),
            KeyCode::PageDown => self.scroll_offset.saturating_sub(page),
            // Home jumps to the oldest line.
            KeyCode::Home if self.scroll_offset < max_scroll => max_scroll,
            // End goes back to the live view; when already live, End goes
            // to the shell.
            KeyCode::End if self.scroll_offset > 0 => 0,
            _ => return false,
        };
        if key != KeyCode::End {
            self.clear_selection();
        }
        self.needs_redraw = true;
        true
    }

    fn send_key(&mut self, key: KeyCode, modifiers: Modifiers) {
        let app_cursor = self.term.application_cursor_keys();
        let Some(bytes) = input::terminal_key_bytes(key, modifiers, app_cursor) else {
            return;
        };
        if !self.term.is_alt_screen() {
            self.scroll_offset = 0;
        }
        self.clear_selection();
        self.send(&bytes);
    }

    fn on_wheel(&mut self, delta_y: f64) {
        let up = wheel_scrolls_up(delta_y);
        let lines = (delta_y.abs() * 3.0).ceil() as usize;
        // the selection is in screen cells; it would point at other text
        self.clear_selection();
        if self.term.is_alt_screen() {
            // Wheel as arrow keys, the same sequences vim/less/htop get from
            // the keyboard.
            let seq = match (up, self.term.application_cursor_keys()) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            for _ in 0..lines {
                let _ = self.term.write_str(seq);
            }
        } else if up {
            let max_scroll = self.term.scrollback_rows().len();
            self.scroll_offset = (self.scroll_offset + lines).min(max_scroll);
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        }
        self.needs_redraw = true;
    }

    fn on_text(&mut self, text: &str) {
        let printable: String = self
            .text_input
            .normalize(text)
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        if printable.is_empty() {
            return;
        }
        self.scroll_offset = 0;
        self.clear_selection();
        let _ = self.term.write_str(&printable);
        self.wake_cursor();
    }

    /// Follows the canvas's real size. It can differ from the config and from
    /// Resized events by a few pixels (title bar, DPI rounding), and a stale
    /// size makes the grid too big, clipping the last row and column.
    fn follow_canvas_size(&mut self, canvas: &Canvas) {
        let actual = canvas.size();
        if actual.0 > 0 && actual.1 > 0 && actual != self.window_size {
            self.resize_grid(actual.0, actual.1);
        }
    }

    /// Takes new shell output, the title with it, and blinks the cursor
    /// while the shell is quiet.
    fn pump_shell(&mut self, window: &Window) {
        let blink = Duration::from_millis(self.config.cursor.blink_interval_ms);
        let history = self.term.scrollback_rows().len();
        if self.term.sync() {
            // scrolled back: stay on the same lines while new ones come in
            if self.scroll_offset > 0 {
                let grown = self.term.scrollback_rows().len().saturating_sub(history);
                let max_scroll = self.term.scrollback_rows().len();
                self.scroll_offset = (self.scroll_offset + grown).min(max_scroll);
            }
            self.wake_cursor();
            self.last_output = Instant::now();
            // A TUI app just started: show all of its screen.
            if self.term.is_alt_screen() {
                self.scroll_offset = 0;
            }
            self.update_title(window);
        } else if self.config.cursor.blink
            && self.last_output.elapsed() >= blink
            && self.last_blink.elapsed() >= blink
        {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = Instant::now();
            self.needs_redraw = true;
        }
        // TUI apps hide the cursor with DECTCEM and draw their own.
        if !self.term.app_cursor_visible() {
            self.cursor_visible = false;
        }
    }

    fn update_title(&mut self, window: &Window) {
        let title = self.term.title();
        if title == self.last_title {
            return;
        }
        let shown = match &title {
            Some(title) => format!("{DEFAULT_TITLE} - {}", display_shell_name(title)),
            None => DEFAULT_TITLE.to_owned(),
        };
        let _ = window.set_title(&shown);
        self.last_title = title;
    }

    /// Rows that fit inside the padding.
    fn visible_rows(&self) -> usize {
        let padding = self.padding();
        (self.window_size.1.saturating_sub(2 * padding) as f32 / self.metrics.height).max(1.0)
            as usize
    }

    fn draw(&mut self, canvas: &mut Canvas) -> AureaResult<()> {
        let padding = self.padding();
        let pad = padding as f32;
        let visible_rows = self.visible_rows();
        let selection = self
            .sel_anchor
            .zip(self.sel_end)
            .map(|(a, e)| SelectionRange::new(a, e));
        let term = &self.term;
        let (rows, cursor, cursor_visible, selection) = if self.scroll_offset == 0 {
            let rows = CellRowsView::from_screen(term.cells());
            (rows, term.cursor(), self.cursor_visible, selection)
        } else {
            let history = term.scrollback_rows();
            let screen = term.cells();
            let end = (history.len() + screen.len()).saturating_sub(self.scroll_offset);
            let start = end.saturating_sub(visible_rows);
            let rows = CellRowsView::from_split(history, screen, start, end);
            (rows, (usize::MAX, usize::MAX), false, None)
        };
        // `canvas.draw()` invalidates the native surface itself; an extra
        // `invalidate_all()` would repaint again through the background-only
        // draw callback and wipe this frame.
        canvas.draw(|ctx| {
            render::draw_grid(
                ctx,
                &rows,
                cursor,
                cursor_visible,
                &self.metrics,
                &self.fonts,
                &self.config.cursor.shape,
                pad,
                selection,
            )?;
            if let Some(message) = self.diagnostics.first() {
                render::draw_diagnostics_banner(
                    ctx,
                    message,
                    self.window_size.0 as f32,
                    self.window_size.1 as f32,
                    &self.fonts.primary,
                )?;
            }
            Ok(())
        })?;
        self.needs_redraw = false;
        self.last_render = Instant::now();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> CellMetrics {
        CellMetrics {
            width: 10.0,
            height: 20.0,
            baseline_offset: 16.0,
            ascent: 16.0,
        }
    }

    #[test]
    fn the_grid_fits_inside_the_padding() {
        assert_eq!(grid_size(808, 408, &metrics(), 4), (80, 20));
        assert_eq!(grid_size(0, 0, &metrics(), 4), (1, 1), "never empty");
    }

    #[test]
    fn pixels_map_to_cells_past_the_padding() {
        assert_eq!(px_to_cell(4.0, 4.0, &metrics(), 4.0), (0, 0));
        assert_eq!(px_to_cell(35.0, 45.0, &metrics(), 4.0), (2, 3));
        assert_eq!(px_to_cell(0.0, 0.0, &metrics(), 4.0), (0, 0), "clamped");
    }

    #[test]
    fn paste_cannot_end_bracketed_paste_early() {
        assert_eq!(paste_text("a\x1b[201~rm -rf x\r\nb\nc"), "arm -rf x\rb\rc");
    }

    #[test]
    fn cmd_titles_lose_their_path() {
        assert_eq!(display_shell_name(r"C:\WINDOWS\system32\cmd.exe"), "cmd");
        assert_eq!(display_shell_name("user@host: ~"), "user@host: ~");
    }
}
