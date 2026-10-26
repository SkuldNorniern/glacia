//! One terminal window: the shell session, what the view shows, and how
//! window events change it.

use std::time::{Duration, Instant};

use aurea::render::{Canvas, Point, Rect};
use aurea::{AureaResult, KeyCode, Modifiers, MouseButton, Window, WindowEvent};
use aurea::{clipboard_text, set_clipboard_text};

use vanta::{Cell, CursorStyle, Event, MouseTracking};

use crate::config::{Config, CursorShape, MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::input::{self, TextInputNormalizer};
use crate::mouse::{self, Kind};
use crate::render::{self, CellMetrics, Layout, RowFonts, SelectionRange};
use crate::selection::{Selection, Spot, Unit};
use crate::terminal::TerminalSession;
use crate::theme::Colors;
use crate::{cell_metrics, row_fonts};

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
    /// The button an app is told is down, while it is, and the cell the
    /// last report named, so motion is only told when it changes cell.
    reported: u8,
    reported_cell: Option<(usize, usize)>,
    /// Where the pointer is, in window pixels, for wheel reports.
    pointer: (f64, f64),
    focused: bool,
    /// What the frame draws with, and the vanta palette version it is from.
    colors: Colors,
    palette_version: u64,
    /// The configured font size, for ctrl+0, and a new one a zoom key
    /// asked for, applied once the loop has the canvas to measure with.
    base_font_size: f32,
    zoom_to: Option<f32>,
    /// What an input method is still composing, drawn at the cursor.
    preedit: String,
    /// Where the cursor was drawn, for the input method's candidate window.
    caret: Option<Rect>,
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
            base_font_size: config.font.size,
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
            reported: 0,
            reported_cell: None,
            pointer: (0.0, 0.0),
            focused: true,
            colors: Colors::theme(),
            palette_version: 0,
            zoom_to: None,
            preedit: String::new(),
            caret: None,
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
            // a synchronized update that never ends still shows, on time
            let timeout = match (timeout, self.term.sync_deadline()) {
                (t, Some(end)) => {
                    let left = end.saturating_duration_since(Instant::now());
                    Some(t.map_or(left, |t| t.min(left)))
                }
                (t, None) => t,
            };
            let mut close = false;
            for event in window.wait_events(timeout) {
                close |= self.on_event(&event);
            }
            if close {
                return Ok(());
            }
            if let Some(size) = self.zoom_to.take() {
                self.set_font_size(size, canvas);
            }
            self.follow_canvas_size(canvas);
            self.pump_shell(window);
            if !self.term.is_running() {
                return Ok(());
            }
            if self.needs_redraw && self.last_render.elapsed() >= FRAME_INTERVAL {
                self.draw(canvas)?;
                if let Some(caret) = self.caret {
                    let _ = window.set_ime_cursor_area(caret);
                }
            }
        }
    }

    /// How long until the cursor blinks next, or none when it doesn't.
    fn next_blink(&self) -> Option<Duration> {
        if !self.blinks() {
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
        // rewrapping can shrink scrollback under the view
        self.scroll_offset = self.scroll_offset.min(self.term.scrollback_len());
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
            (self.term.rows().len().checked_sub(1)?, usize::MAX)
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
            WindowEvent::Focused | WindowEvent::Unfocused => {
                self.focused = matches!(event, WindowEvent::Focused);
                if !self.focused {
                    // an input method may not say its composition ended
                    self.preedit.clear();
                }
                if self.term.focus_tracking() {
                    let _ = self
                        .term
                        .write_str(if self.focused { "\x1b[I" } else { "\x1b[O" });
                }
                self.wake_cursor();
            }
            WindowEvent::MouseButton {
                button,
                pressed,
                x,
                y,
                modifiers,
                click_count,
                ..
            } => {
                let code = match button {
                    MouseButton::Left => Some(mouse::LEFT),
                    MouseButton::Middle => Some(mouse::MIDDLE),
                    MouseButton::Right => Some(mouse::RIGHT),
                    MouseButton::Other(_) => None,
                };
                let kind = if *pressed { Kind::Press } else { Kind::Release };
                let told = code.is_some_and(|code| {
                    let bit = 1 << code;
                    // a release goes where its press went; a press in the
                    // margin hits no cell
                    let to_app = if *pressed {
                        self.app_takes_mouse(*modifiers)
                            && self.layout.contains(*x, *y, &self.metrics)
                    } else {
                        self.reported & bit != 0
                    };
                    let told = to_app && self.report_mouse(code, kind, *modifiers, *x, *y);
                    if told && *pressed {
                        self.reported |= bit;
                    } else if !*pressed {
                        self.reported &= !bit;
                    }
                    told
                });
                // a drag selection always ends on its release
                let ends_drag = !*pressed && self.mouse_dragging;
                if *button == MouseButton::Left && (!told || ends_drag) {
                    self.on_left_button(*pressed, *x, *y, *click_count);
                }
            }
            WindowEvent::MouseMove {
                x, y, modifiers, ..
            } => {
                self.pointer = (*x, *y);
                if self.mouse_dragging {
                    self.on_drag(*x, *y);
                } else if self.reported != 0 || self.app_takes_mouse(*modifiers) {
                    // motion carries the lowest button held, or none
                    let button = (0..3)
                        .find(|b| self.reported & (1 << b) != 0)
                        .unwrap_or(mouse::NONE);
                    self.report_mouse(button, Kind::Motion, *modifiers, *x, *y);
                }
            }
            WindowEvent::KeyInput {
                key,
                pressed: true,
                modifiers,
            } => self.on_key(*key, *modifiers),
            WindowEvent::MouseWheel {
                delta_y, modifiers, ..
            } => self.on_wheel(*delta_y, *modifiers),
            WindowEvent::TextInput { text } => {
                self.preedit.clear();
                self.on_text(text);
            }
            WindowEvent::ImePreedit { text, .. } if *text != self.preedit => {
                text.clone_into(&mut self.preedit);
                self.needs_redraw = true;
            }
            _ => {}
        }
        false
    }

    /// Whether mouse input goes to the app: it asked for it and shift,
    /// which always selects instead, isn't held.
    fn app_takes_mouse(&self, mods: Modifiers) -> bool {
        // scrolled back, the rows on view aren't the app's: select instead
        self.term.mouse().0 != MouseTracking::Off && !mods.shift && self.scroll_offset == 0
    }

    /// Tells the app about the mouse when its mode wants this. True when
    /// the app took it.
    fn report_mouse(&mut self, button: u8, kind: Kind, mods: Modifiers, x: f64, y: f64) -> bool {
        let (tracking, protocol) = self.term.mouse();
        if !mouse::wanted(tracking, kind, self.reported != 0) {
            return tracking != MouseTracking::Off && kind == Kind::Motion;
        }
        let (row, col) = self.layout.cell_at(x, y, &self.metrics);
        if kind == Kind::Motion && self.reported_cell == Some((row, col)) {
            return true;
        }
        self.reported_cell = Some((row, col));
        if let Some(bytes) = mouse::report(protocol, button, kind, mods, (col, row)) {
            let _ = self.term.write_bytes(&bytes);
        }
        true
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
        }
        // the view catches up with the offset in `pump_shell`
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
        let zoom = modifiers.ctrl && !modifiers.shift && !modifiers.alt;
        // two presses before the loop applies the first add up
        let size = self.zoom_to.unwrap_or(self.config.font.size);
        if zoom && matches!(key, KeyCode::Equals | KeyCode::Minus | KeyCode::Key0) {
            let next = match key {
                KeyCode::Equals => size + 1.0,
                KeyCode::Minus => size - 1.0,
                _ => self.base_font_size,
            };
            self.zoom_to = Some(next.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
        } else if copy_paste && key == KeyCode::C {
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

    fn on_wheel(&mut self, delta_y: f64, mods: Modifiers) {
        let up = wheel_scrolls_up(delta_y);
        let lines = (delta_y.abs() * 3.0).ceil() as usize;
        if self.app_takes_mouse(mods) {
            // tmux, vim and the rest with mouse on scroll themselves
            let (_, protocol) = self.term.mouse();
            let button = if up {
                mouse::WHEEL_UP
            } else {
                mouse::WHEEL_DOWN
            };
            let at = self
                .layout
                .cell_at(self.pointer.0, self.pointer.1, &self.metrics);
            for _ in 0..lines.min(10) {
                if let Some(bytes) =
                    mouse::report(protocol, button, Kind::Press, mods, (at.1, at.0))
                {
                    let _ = self.term.write_bytes(&bytes);
                }
            }
        } else if self.term.is_alt_screen() {
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
            self.take_events();
            if self.term.palette_version() != self.palette_version {
                self.palette_version = self.term.palette_version();
                self.colors = Colors::from_palette(&self.term.palette());
            }
            // A TUI app just started: show all of its screen.
            if self.term.is_alt_screen() && self.scroll_offset > 0 {
                self.scroll_offset = 0;
                self.term.sync(0);
            }
            self.update_title(window);
        } else if self.blinks()
            && self.last_output.elapsed() >= blink
            && self.last_blink.elapsed() >= blink
        {
            self.cursor_visible = !self.cursor_visible;
            self.last_blink = Instant::now();
            self.needs_redraw = true;
        }
        if !self.blinks() && !self.cursor_visible {
            // a blink stopped half way (focus left, the app set a steady
            // shape): show it solid
            self.cursor_visible = true;
            self.needs_redraw = true;
        }
    }

    /// Remeasures the cells for a new font size and refits the grid.
    fn set_font_size(&mut self, size: f32, canvas: &Canvas) {
        if size == self.config.font.size {
            return;
        }
        self.config.font.size = size;
        self.fonts = row_fonts(&self.config);
        self.metrics = cell_metrics(&self.config, canvas, &mut self.fonts);
        let (w, h) = self.window_size;
        self.resize_grid(w, h);
        self.clear_selection();
    }

    /// Clipboard writes from apps (OSC 52: yank in a remote vim lands
    /// here) when the config allows them.
    fn take_events(&mut self) {
        for event in self.term.take_events() {
            if let Event::Clipboard(text) = event
                && self.config.terminal.clipboard_write
            {
                let _ = set_clipboard_text(&text);
            }
        }
    }

    /// The app's DECSCUSR shape, else the configured one; an outline
    /// while another window has the keyboard.
    fn cursor_shape(&self) -> CursorShape {
        if !self.focused {
            return CursorShape::Hollow;
        }
        match self.term.cursor_style() {
            CursorStyle::Default => self.config.cursor.shape.clone(),
            CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => CursorShape::Block,
            CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => CursorShape::Underline,
            CursorStyle::BlinkingBar | CursorStyle::SteadyBar => CursorShape::Beam,
        }
    }

    /// Whether the cursor blinks now: the config or the app says so, it
    /// is shown at all, and the window has the keyboard.
    fn blinks(&self) -> bool {
        let wanted = match self.term.cursor_style() {
            CursorStyle::Default => self.config.cursor.blink,
            CursorStyle::BlinkingBlock
            | CursorStyle::BlinkingUnderline
            | CursorStyle::BlinkingBar => true,
            _ => false,
        };
        wanted && self.focused && self.term.app_cursor_visible()
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
        let cursor = self
            .term
            .cursor()
            .filter(|_| self.cursor_visible && self.term.app_cursor_visible());
        let shape = self.cursor_shape();
        // `canvas.draw()` invalidates the native surface itself; an extra
        // `invalidate_all()` would repaint again through the background-only
        // draw callback and wipe this frame.
        let mut caret = None;
        // the margins follow an app that changed the background
        canvas.set_background_color(self.colors.background);
        canvas.draw(|ctx| {
            render::draw_grid(
                ctx,
                rows,
                cursor,
                &self.metrics,
                &self.fonts,
                &shape,
                &self.layout,
                selection,
                &self.colors,
            )?;
            if let Some((row, col)) = self.term.cursor() {
                let x = self.layout.cell_x(col, &self.metrics);
                let y = self.layout.row_y(row, &self.metrics);
                if !self.preedit.is_empty() {
                    render::draw_preedit(
                        ctx,
                        &self.preedit,
                        Point::new(x, y),
                        &self.metrics,
                        &self.fonts,
                        &self.colors,
                    )?;
                }
                caret = Some(Rect::new(x, y, self.metrics.width, self.metrics.height));
            }
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
        self.caret = caret;
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
