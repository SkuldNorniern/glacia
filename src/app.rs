//! One terminal window: the shell session, what the view shows, and how
//! window events change it.

use std::time::{Duration, Instant};

use aurea::render::Canvas;
use aurea::{AureaResult, KeyCode, Modifiers, MouseButton, Window, WindowEvent};
use aurea::{clipboard_text, set_clipboard_text};

use vanta::Cell;

use crate::config::Config;
use crate::input::{self, TextInputNormalizer};
use crate::render::{self, CellMetrics, Layout, RowFonts, SelectionRange};
use crate::selection::{Selection, Spot, Unit};
use crate::terminal::TerminalSession;

const FRAME_INTERVAL: Duration = Duration::from_micros(16_667); // 60 FPS render cap
pub const DEFAULT_TITLE: &str = "Glacia";

pub fn grid_size(width: u32, height: u32, metrics: &CellMetrics, padding: u32) -> (u16, u16) {
    let layout = Layout::new(width, height, metrics, padding);
    (layout.cols as u16, layout.rows as u16)
}

/// Clipboard text made safe to paste: no paste-end marker that would let
/// the rest run as typed commands, and line breaks as the CR Enter sends.
fn paste_text(text: &str) -> String {
    // no escape at all: taking out one marker could join the pieces around
    // it into another (`ESC[2 ESC[201~ 01~`)
    text.chars()
        .filter(|c| !matches!(c, '\x1b' | '\u{9b}'))
        .collect::<String>()
        .replace("\r\n", "\r")
        .replace('\n', "\r")
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
    layout: Layout,
    text_input: TextInputNormalizer,
    /// Rows scrolled back into history; 0 is the live screen.
    scroll_offset: usize,
    selection: Option<Selection>,
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
            layout: Layout::new(
                config.window.width,
                config.window.height,
                &metrics,
                config.window.padding,
            ),
            config,
            term,
            metrics,
            fonts,
            diagnostics,
            text_input: TextInputNormalizer::new(),
            scroll_offset: 0,
            selection: None,
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
            // Input and shell output (the reader wakes the window) end the
            // wait at once; otherwise it lasts until the next frame is due or
            // the cursor's next blink, and with nothing coming, for good.
            let timeout = if self.needs_redraw {
                Some(FRAME_INTERVAL.saturating_sub(self.last_render.elapsed()))
            } else {
                self.next_blink()
            };
            let mut close = false;
            for event in window.wait_events(timeout) {
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

    /// How long until the cursor blinks next, or none when it doesn't.
    fn next_blink(&self) -> Option<Duration> {
        if !self.config.cursor.blink {
            return None;
        }
        let blink = Duration::from_millis(self.config.cursor.blink_interval_ms);
        let quiet = blink.saturating_sub(self.last_output.elapsed());
        let flip = blink.saturating_sub(self.last_blink.elapsed());
        Some(quiet.max(flip))
    }

    fn padding(&self) -> u32 {
        self.config.window.padding
    }

    fn resize_grid(&mut self, width: u32, height: u32) {
        self.window_size = (width, height);
        self.layout = Layout::new(width, height, &self.metrics, self.padding());
        let _ = self
            .term
            .resize(self.layout.cols as u16, self.layout.rows as u16);
        self.needs_redraw = true;
    }

    fn clear_selection(&mut self) {
        if self.selection.take().is_some() {
            self.needs_redraw = true;
        }
    }

    /// The text spot under a window point, and that line's cells.
    fn spot_at(&self, x: f64, y: f64) -> (Spot, &[Cell]) {
        let (row, col) = self.layout.cell_at(x, y, &self.metrics);
        let line = self.term.dropped() + (self.term.first() + row) as u64;
        let cells = self.term.rows().get(row).map_or(&[][..], Vec::as_slice);
        ((line, col), cells)
    }

    /// The selection as rows of the view, cut to what is on view.
    fn selection_on_view(&self) -> Option<SelectionRange> {
        let sel = self.selection.filter(|s| s.shown)?;
        let ((start, start_col), (end, end_col)) = sel.range();
        let top = self.term.dropped() + self.term.first() as u64;
        let bottom = top + self.term.rows().len() as u64;
        if end < top || start >= bottom {
            return None;
        }
        let from = if start < top {
            (0, 0)
        } else {
            ((start - top) as usize, start_col)
        };
        let to = if end >= bottom {
            (self.term.rows().len() - 1, usize::MAX)
        } else {
            ((end - top) as usize, end_col)
        };
        Some(SelectionRange::new(from, to))
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
                click_count,
                ..
            } => self.on_left_button(*pressed, *x, *y, *click_count),
            WindowEvent::MouseMove { x, y, .. } if self.mouse_dragging => self.on_drag(*x, *y),
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

    /// A press starts a selection: by cell, by word on a double click,
    /// by line on a triple one.
    fn on_left_button(&mut self, pressed: bool, x: f64, y: f64, clicks: u8) {
        if pressed {
            let unit = match clicks {
                0 | 1 => Unit::Cell,
                2 => Unit::Word,
                _ => Unit::Line,
            };
            let (spot, cells) = self.spot_at(x, y);
            self.selection = Some(Selection::start(unit, spot, cells));
        }
        self.mouse_dragging = pressed;
        self.needs_redraw = true;
    }

    fn on_drag(&mut self, x: f64, y: f64) {
        // past the top or bottom edge the view scrolls along
        let top = self.layout.y as f64;
        let bottom = top + self.layout.rows as f64 * self.metrics.height as f64;
        if !self.term.is_alt_screen() {
            if y < top {
                self.scroll_offset = (self.scroll_offset + 1).min(self.term.scrollback_len());
            } else if y >= bottom {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            self.term.sync(self.scroll_offset);
        }
        let (spot, cells) = self.spot_at(x, y);
        let cells = cells.to_vec();
        if let Some(sel) = &mut self.selection {
            let before = *sel;
            sel.extend(spot, &cells);
            if *sel != before {
                self.needs_redraw = true;
            }
        }
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
        let Some(sel) = self.selection.filter(|s| s.shown) else {
            return;
        };
        let ((start, start_col), (end, end_col)) = sel.range();
        let dropped = self.term.dropped();
        if end < dropped {
            // all of it fell off the top of scrollback
            return;
        }
        let from = if start < dropped {
            (0, 0)
        } else {
            ((start - dropped) as usize, start_col)
        };
        let text = self.term.text(from, ((end - dropped) as usize, end_col));
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
        let max_scroll = self.term.scrollback_len();
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
            let max_scroll = self.term.scrollback_len();
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
        let history = self.term.scrollback_len();
        let dropped = self.term.dropped();
        if self.term.sync(self.scroll_offset) {
            // scrolled back: stay on the same lines while new ones come in;
            // at the scrollback cap they show as dropped, not as growth
            if self.scroll_offset > 0 && !self.term.is_alt_screen() {
                let gone = usize::try_from(self.term.dropped() - dropped).unwrap_or(usize::MAX);
                let grown = (self.term.scrollback_len() + gone).saturating_sub(history);
                self.scroll_offset = (self.scroll_offset + grown).min(self.term.scrollback_len());
                self.term.sync(self.scroll_offset);
            }
            self.wake_cursor();
            self.last_output = Instant::now();
            // A TUI app just started: show all of its screen.
            if self.term.is_alt_screen() && self.scroll_offset > 0 {
                self.scroll_offset = 0;
                self.term.sync(0);
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
        self.layout.rows
    }

    fn draw(&mut self, canvas: &mut Canvas) -> AureaResult<()> {
        let selection = self.selection_on_view();
        let rows = self.term.rows();
        let cursor = self.term.cursor().filter(|_| self.cursor_visible);
        // `canvas.draw()` invalidates the native surface itself; an extra
        // `invalidate_all()` would repaint again through the background-only
        // draw callback and wipe this frame.
        canvas.draw(|ctx| {
            render::draw_grid(
                ctx,
                rows,
                cursor,
                &self.metrics,
                &self.fonts,
                &self.config.cursor.shape,
                &self.layout,
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
    fn spare_room_is_split_evenly_around_the_grid() {
        let layout = Layout::new(815, 415, &metrics(), 4);
        assert_eq!((layout.cols, layout.rows), (80, 20));
        // 7 px over on each axis: 3 more before, 4 more after
        assert_eq!((layout.x, layout.y), (7.0, 7.0));
        let tiny = CellMetrics {
            width: 0.01,
            height: 0.01,
            ..metrics()
        };
        let huge = Layout::new(100_000, 100_000, &tiny, 0);
        assert!(huge.cols <= 1000 && huge.rows <= 1000, "capped");
    }

    #[test]
    fn pixels_map_to_cells_past_the_padding() {
        let layout = Layout::new(808, 408, &metrics(), 4);
        assert_eq!(layout.cell_at(4.0, 4.0, &metrics()), (0, 0));
        assert_eq!(layout.cell_at(35.0, 45.0, &metrics()), (2, 3));
        assert_eq!(layout.cell_at(0.0, 0.0, &metrics()), (0, 0), "clamped");
        assert_eq!(
            layout.cell_at(5000.0, 5000.0, &metrics()),
            (19, 79),
            "clamped"
        );
    }

    #[test]
    fn snapped_cells_meet_with_no_gap() {
        let m = CellMetrics {
            width: 8.43,
            ..metrics()
        };
        let layout = Layout::new(808, 408, &m, 4);
        for col in 0..layout.cols {
            let (a, b) = (layout.cell_x(col, &m), layout.cell_x(col + 1, &m));
            assert_eq!(a, a.round());
            assert!(b - a >= 8.0 && b - a <= 9.0);
        }
    }

    #[test]
    fn paste_cannot_end_bracketed_paste_early() {
        assert_eq!(
            paste_text("a\x1b[201~rm -rf x\r\nb\nc"),
            "a[201~rm -rf x\rb\rc"
        );
        assert!(!paste_text("\x1b[2\x1b[201~01~").contains('\x1b'));
    }

    #[test]
    fn cmd_titles_lose_their_path() {
        assert_eq!(display_shell_name(r"C:\WINDOWS\system32\cmd.exe"), "cmd");
        assert_eq!(display_shell_name("user@host: ~"), "user@host: ~");
    }
}
