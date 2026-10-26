//! One terminal window: the shell session, what the view shows, and how
//! window events change it.

use std::mem::take;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use aurea::render::{Canvas, Color, DrawingContext, Paint, PaintStyle, Point, Rect};
use aurea::{AureaResult, KeyCode, Modifiers, MouseButton, Window, WindowEvent};
use aurea::{clipboard_text, reduced_motion, set_clipboard_text};

use vanta::{Cell, CursorStyle, Event, MouseTracking};

use crate::config::{self, Config, CursorShape, MAX_FONT_SIZE, MIN_FONT_SIZE};
use crate::input::{self, TextInputNormalizer};
use crate::mouse::{self, Kind};
use crate::render::{self, CellMetrics, Layout, RowFonts, SelectionRange, TabLabel};
use crate::search::{RESEARCH_EVERY, Search};
use crate::selection::{Selection, Spot, Unit};
use crate::tabs::{self, Area, Bar, BarHit, Dir, Divider, Node, Pane, Spawner};
use crate::terminal::TerminalSession;
use crate::theme::{self, Colors};
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

fn solid(color: Color) -> Paint {
    Paint {
        color,
        style: PaintStyle::Fill,
        ..Default::default()
    }
}

/// An arrow key as a step: `(dx, dy)`.
fn arrow(key: KeyCode) -> Option<(i8, i8)> {
    Some(match key {
        KeyCode::Left => (-1, 0),
        KeyCode::Right => (1, 0),
        KeyCode::Up => (0, -1),
        KeyCode::Down => (0, 1),
        _ => return None,
    })
}

/// Ctrl+Alt+1..9 picks a tab: the digit's index.
fn tab_digit(key: KeyCode) -> Option<usize> {
    let keys = [
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
    keys.iter().position(|k| *k == key)
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

/// Wakes the event loop from another thread.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Takes new output into the view. Scrolled back, the view stays on the
/// same lines while new ones come in; at the scrollback cap they show as
/// dropped, not as growth. A full screen app coming up shows all of it.
/// True when output came.
fn follow(term: &mut TerminalSession, scroll: &mut usize) -> bool {
    let history = term.scrollback_len();
    let dropped = term.dropped();
    if !term.sync(*scroll) {
        return false;
    }
    if *scroll > 0 && !term.is_alt_screen() {
        let gone = usize::try_from(term.dropped() - dropped).unwrap_or(usize::MAX);
        let grown = (term.scrollback_len() + gone).saturating_sub(history);
        *scroll = (*scroll + grown).min(term.scrollback_len());
        term.sync(*scroll);
    }
    if term.is_alt_screen() && *scroll > 0 {
        *scroll = 0;
        term.sync(0);
    }
    true
}

/// How long the visual bell shows.
const FLASH_FOR: Duration = Duration::from_millis(150);
/// How long a config problem stays on screen.
const DIAGNOSTICS_FOR: Duration = Duration::from_secs(10);

/// How often a shell whose output ended is checked for its exit.
const REAP_EVERY: Duration = Duration::from_millis(50);

/// The line between split panes.
const PANE_GAP: f32 = 1.0;

pub struct App {
    config: Config,
    term: TerminalSession,
    /// Every pane by id. The one with the keyboard has an empty slot; its
    /// state is in this struct's own fields.
    panes: Vec<Option<Pane>>,
    focused: usize,
    /// Each tab's panes, and which last had the keyboard there.
    tabs: Vec<(Node, usize)>,
    active: usize,
    /// The panes on show: id, area, and cell layout.
    areas: Vec<(usize, Area, Layout)>,
    /// A line between panes being dragged.
    dragging: Option<Divider>,
    /// What the pointer is over on the tab bar.
    bar_hover: Option<BarHit>,
    /// A tab being dragged: its index, and where in it it was grabbed.
    tab_drag: Option<(usize, f32)>,
    /// Where each tab is drawn; tabs slide to their slot after a move.
    tab_x: Vec<f32>,
    spawn: Spawner,
    wake: Wake,
    /// The window title wants another look: output came, or the tab or
    /// pane changed.
    retitle: bool,
    metrics: CellMetrics,
    fonts: RowFonts,
    diagnostics: Vec<String>,
    /// When the banner goes away.
    diagnostics_until: Option<Instant>,
    /// New configs from the file watcher.
    config_changes: Receiver<(Config, Option<String>)>,
    /// The font in the config changed; remeasure with the canvas.
    refont: bool,
    /// When the visual bell began, while it shows.
    flash: Option<Instant>,
    window_size: (u32, u32),
    layout: Layout,
    text_input: TextInputNormalizer,
    /// Rows scrolled back into history; 0 is the live screen.
    scroll_offset: usize,
    selection: Option<Selection>,
    /// The find bar, while it is open.
    search: Option<Search>,
    mouse_dragging: bool,
    /// The button an app is told is down, while it is, and the cell the
    /// last report named, so motion is only told when it changes cell.
    reported: u8,
    reported_cell: Option<(usize, usize)>,
    /// Where the pointer is, in window pixels, for wheel reports.
    pointer: (f64, f64),
    window_focused: bool,
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
        spawn: Spawner,
        wake: Wake,
        metrics: CellMetrics,
        fonts: RowFonts,
        diagnostics: Vec<String>,
    ) -> Self {
        let now = Instant::now();
        let config_changes = config::watch({
            let wake = Arc::clone(&wake);
            move || wake()
        });
        let mut app = Self {
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
            panes: vec![None],
            focused: 0,
            tabs: vec![(Node::Leaf(0), 0)],
            active: 0,
            areas: Vec::new(),
            dragging: None,
            bar_hover: None,
            tab_drag: None,
            tab_x: vec![0.0],
            spawn,
            wake,
            retitle: true,
            metrics,
            fonts,
            diagnostics_until: (!diagnostics.is_empty()).then(|| now + DIAGNOSTICS_FOR),
            diagnostics,
            config_changes,
            refont: false,
            flash: None,
            text_input: TextInputNormalizer::new(),
            scroll_offset: 0,
            selection: None,
            search: None,
            mouse_dragging: false,
            reported: 0,
            reported_cell: None,
            pointer: (0.0, 0.0),
            window_focused: true,
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
        };
        // the first pane's place in the window
        app.refit();
        app
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
            // a config problem leaves on time, idle or not
            let timeout = match self.diagnostics_until {
                Some(until) => {
                    let left = until.saturating_duration_since(Instant::now());
                    Some(timeout.map_or(left, |t| t.min(left)))
                }
                None => timeout,
            };
            // a shell whose output ended but isn't reaped yet: look again soon
            let dying = self.term.closed() || self.panes.iter().flatten().any(|p| p.term.closed());
            let timeout = if dying {
                Some(timeout.map_or(REAP_EVERY, |t| t.min(REAP_EVERY)))
            } else {
                timeout
            };
            // matches behind new output are looked for again, on a timer
            let timeout = match self
                .search
                .as_ref()
                .filter(|s| s.stale && s.pending.is_none())
            {
                Some(search) => {
                    let left = RESEARCH_EVERY.saturating_sub(search.done_at.elapsed());
                    Some(timeout.map_or(left, |t| t.min(left)))
                }
                None => timeout,
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
            self.take_config();
            if self.slide_tabs() {
                self.needs_redraw = true;
            }
            if let Some(size) = self.zoom_to.take() {
                self.set_font_size(size, canvas);
            }
            if take(&mut self.refont) {
                self.remeasure(canvas);
            }
            // the bell's flash fades out over a few frames
            if let Some(start) = self.flash {
                if start.elapsed() < FLASH_FOR {
                    self.needs_redraw = true;
                } else {
                    self.flash = None;
                    self.needs_redraw = true;
                }
            }
            if self.diagnostics_until.is_some_and(|t| Instant::now() >= t) {
                self.diagnostics_until = None;
                self.diagnostics.clear();
                self.needs_redraw = true;
            }
            self.follow_canvas_size(canvas);
            self.pump_shell();
            self.pump_panes();
            // a shell that exited takes its pane with it; the last one, the window
            if !self.term.is_running() && !self.close_pane(self.focused) {
                return Ok(());
            }
            self.poll_search();
            if take(&mut self.retitle) {
                self.update_title(window);
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
        let bar = self.bar_height();
        let area = (
            0.0,
            bar as f32,
            width as f32,
            height.saturating_sub(bar) as f32,
        );
        let tree = &self.tabs[self.active].0;
        let padding = self.padding();
        self.areas = tree
            .areas(area, PANE_GAP)
            .into_iter()
            .map(|(id, (x, y, w, h))| {
                let mut layout = Layout::new(w as u32, h as u32, &self.metrics, padding);
                layout.x += x;
                layout.y += y;
                (id, (x, y, w, h), layout)
            })
            .collect();
        for &(id, _, layout) in &self.areas {
            let (cols, rows) = (layout.cols as u16, layout.rows as u16);
            if id == self.focused {
                self.layout = layout;
                let _ = self.term.resize(cols, rows);
                // rewrapping can shrink scrollback under the view
                self.scroll_offset = self.scroll_offset.min(self.term.scrollback_len());
            } else if let Some(Some(pane)) = self.panes.get_mut(id) {
                let _ = pane.term.resize(cols, rows);
                pane.scroll_offset = pane.scroll_offset.min(pane.term.scrollback_len());
            }
        }
        // rewrapping moved the text the matches point at
        if let Some(search) = &mut self.search {
            search.stale = true;
        }
        self.needs_redraw = true;
    }

    fn refit(&mut self) {
        let (w, h) = self.window_size;
        self.resize_grid(w, h);
    }

    /// The tab bar's height: none for a single tab.
    fn bar_height(&self) -> u32 {
        if self.tabs.len() > 1 || self.config.window.always_show_tabs {
            (self.metrics.height + 12.0).round() as u32
        } else {
            0
        }
    }

    /// Gives pane `id` the keyboard: its state swaps into this struct.
    fn focus(&mut self, id: usize) {
        if id == self.focused {
            return;
        }
        let Some(mut pane) = self.panes.get_mut(id).and_then(Option::take) else {
            return;
        };
        // half typed hangul goes to the pane it was typed in
        let pending = self.text_input.flush();
        if !pending.is_empty() {
            let _ = self.term.write_str(&pending);
        }
        {
            use std::mem::swap;
            swap(&mut self.term, &mut pane.term);
            swap(&mut self.scroll_offset, &mut pane.scroll_offset);
            swap(&mut self.selection, &mut pane.selection);
            swap(&mut self.search, &mut pane.search);
            swap(&mut self.colors, &mut pane.colors);
            swap(&mut self.palette_version, &mut pane.palette_version);
        }
        pane.bell = false;
        self.panes[self.focused] = Some(pane);
        self.focused = id;
        if let Some(&(_, _, layout)) = self.areas.iter().find(|a| a.0 == id) {
            self.layout = layout;
        }
        self.reported = 0;
        self.reported_cell = None;
        self.mouse_dragging = false;
        self.preedit.clear();
        self.term.sync(self.scroll_offset);
        self.retitle = true;
        self.wake_cursor();
    }

    /// Shows tab `index`, with the pane that last had the keyboard there.
    fn switch_tab(&mut self, index: usize) {
        if index == self.active || index >= self.tabs.len() {
            return;
        }
        // a held line belongs to the tab being left
        self.dragging = None;
        self.tabs[self.active].1 = self.focused;
        self.active = index;
        self.focus(self.tabs[index].1);
        self.refit();
    }

    /// Starts a shell in this pane's directory, when its shell told it
    /// (osc 7). The new pane's id.
    fn spawn_pane(&mut self) -> Option<usize> {
        let dir = self.term.cwd();
        let (cols, rows) = (self.layout.cols as u16, self.layout.rows as u16);
        // a directory a remote shell named may not be here: start anyway
        let term = (self.spawn)(cols, rows, dir.as_deref())
            .or_else(|_| (self.spawn)(cols, rows, None))
            .ok()?;
        term.set_palette(Colors::theme().palette());
        self.panes.push(Some(Pane::new(term)));
        Some(self.panes.len() - 1)
    }

    /// A new tab right of this one.
    fn new_tab(&mut self) {
        let Some(id) = self.spawn_pane() else {
            return;
        };
        self.tab_drag = None;
        self.tabs.insert(self.active + 1, (Node::Leaf(id), id));
        let from = self.tab_x.get(self.active).copied().unwrap_or(0.0);
        self.tab_x
            .insert((self.active + 1).min(self.tab_x.len()), from);
        self.switch_tab(self.active + 1);
        // the bar may have just shown up
        self.refit();
    }

    /// Splits the pane with the keyboard; the new one gets it.
    fn split(&mut self, dir: Dir) {
        // too small to hold two usable panes
        let (cols, rows) = (self.layout.cols, self.layout.rows);
        if (dir == Dir::Right && cols < 20) || (dir == Dir::Down && rows < 6) {
            return;
        }
        let Some(id) = self.spawn_pane() else {
            return;
        };
        self.tabs[self.active].0.split(self.focused, id, dir);
        self.refit();
        self.focus(id);
    }

    /// Closes pane `id` and its shell; a tab with no panes left closes
    /// too. False when it was the very last pane.
    fn close_pane(&mut self, id: usize) -> bool {
        // the tree a held line points into is about to change
        self.dragging = None;
        let Some(t) = self.tabs.iter().position(|(tree, _)| tree.contains(id)) else {
            return true;
        };
        let rest = self.tabs[t].0.clone().remove(id);
        if rest.is_none() && self.tabs.len() == 1 {
            return false;
        }
        match rest {
            Some(tree) => {
                let next = tree.panes()[0];
                if self.tabs[t].1 == id {
                    self.tabs[t].1 = next;
                }
                self.tabs[t].0 = tree;
                if id == self.focused {
                    self.focus(next);
                }
            }
            None => {
                if t == self.active {
                    let next = if t + 1 < self.tabs.len() {
                        t + 1
                    } else {
                        t - 1
                    };
                    self.switch_tab(next);
                }
                self.tabs.remove(t);
                if t < self.tab_x.len() {
                    self.tab_x.remove(t);
                }
                // the pointer and a drag were over the old order
                self.bar_hover = None;
                self.tab_drag = None;
                if t < self.active {
                    self.active -= 1;
                }
            }
        }
        // dropping the pane kills its shell
        if let Some(slot) = self.panes.get_mut(id) {
            *slot = None;
        }
        // free slots at the end go; ids before them stay as they are
        while self.panes.len() > self.focused + 1 && matches!(self.panes.last(), Some(None)) {
            self.panes.pop();
        }
        self.refit();
        true
    }

    /// Panes without the keyboard: ones whose shell exited close, the ones
    /// on show follow their output, clipboard writes land and a bell marks
    /// the tab.
    fn pump_panes(&mut self) {
        let mut gone = Vec::new();
        let shown: Vec<usize> = self.areas.iter().map(|a| a.0).collect();
        let bar = self.tabs.len() > 1;
        for (id, slot) in self.panes.iter_mut().enumerate() {
            let Some(pane) = slot else { continue };
            // only rows on view are copied, so hidden panes follow too and
            // a scrolled back one keeps its place
            if !follow(&mut pane.term, &mut pane.scroll_offset) {
                continue;
            }
            if pane.term.palette_version() != pane.palette_version {
                pane.palette_version = pane.term.palette_version();
                pane.colors = Colors::from_palette(&pane.term.palette());
            }
            if let Some(search) = &mut pane.search {
                search.stale = true;
            }
            // the bar shows every tab's title
            self.needs_redraw |= bar || shown.contains(&id);
            for event in pane.term.take_events() {
                match event {
                    Event::Bell => {
                        if !pane.bell {
                            pane.bell = true;
                            self.needs_redraw = true;
                        }
                    }
                    Event::Clipboard(text) if self.config.terminal.clipboard_write => {
                        let _ = set_clipboard_text(&text);
                    }
                    Event::Clipboard(_) => {}
                }
            }
        }
        // a shell whose output ended: asked every pass until it is reaped
        for (id, slot) in self.panes.iter().enumerate() {
            if let Some(pane) = slot
                && pane.term.closed()
                && !pane.term.is_running()
            {
                gone.push(id);
            }
        }
        for id in gone {
            self.close_pane(id);
        }
    }

    /// What the tab bar shows: each tab's title is its keyboard pane's.
    fn tab_labels(&self) -> Vec<TabLabel> {
        let bar = self.bar();
        let label = |term: &TerminalSession| {
            term.title()
                .map_or_else(|| "shell".to_owned(), |t| display_shell_name(&t).to_owned())
        };
        self.tabs
            .iter()
            .enumerate()
            .map(|(i, (tree, last))| {
                let bell = tree
                    .panes()
                    .iter()
                    .any(|id| matches!(self.panes.get(*id), Some(Some(p)) if p.bell));
                let text = if i == self.active {
                    label(&self.term)
                } else {
                    match self.panes.get(*last) {
                        Some(Some(pane)) => label(&pane.term),
                        _ => "shell".to_owned(),
                    }
                };
                TabLabel {
                    text,
                    active: i == self.active,
                    bell,
                    x: self.tab_x.get(i).copied().unwrap_or(bar.slot_x(i)),
                    hover: matches!(self.bar_hover, Some(BarHit::Tab(h) | BarHit::Close(h)) if h == i),
                    close_hover: self.bar_hover == Some(BarHit::Close(i)),
                    dragged: self.tab_drag.is_some_and(|(d, _)| d == i),
                }
            })
            .collect()
    }

    /// Alt+Shift+arrow: moves that way the line of the innermost split
    /// holding the pane with the keyboard. False when there is no such line,
    /// so the key goes to the app.
    fn nudge_line(&mut self, key: KeyCode) -> bool {
        let Some((dx, dy)) = arrow(key).filter(|_| self.areas.len() > 1) else {
            return false;
        };
        let (dir, delta) = if dx != 0 {
            (Dir::Right, f32::from(dx) * 0.05)
        } else {
            (Dir::Down, f32::from(dy) * 0.05)
        };
        let moved = self.tabs[self.active].0.nudge(self.focused, dir, delta);
        if moved {
            self.refit();
        }
        moved
    }

    /// The tab bar's layout now.
    fn bar(&self) -> Bar {
        Bar::new(
            self.window_size.0 as f32,
            self.tabs.len(),
            self.bar_height() as f32,
        )
    }

    /// Closes every pane of tab `i`.
    fn close_tab(&mut self, i: usize) {
        if let Some((tree, _)) = self.tabs.get(i) {
            for id in tree.panes() {
                self.close_pane(id);
            }
        }
    }

    /// Moves the dragged tab with the pointer; passing the middle of a
    /// neighbour swaps their slots.
    fn drag_tab(&mut self, x: f64) {
        let Some((index, grab)) = self.tab_drag.filter(|(i, _)| *i < self.tabs.len()) else {
            self.tab_drag = None;
            return;
        };
        let bar = self.bar();
        let max = bar.slot_x(bar.count.saturating_sub(1));
        let left = (x as f32 - grab).clamp(0.0, max);
        if let Some(shown) = self.tab_x.get_mut(index) {
            *shown = left;
        }
        let target = bar.drop_slot(left);
        if target != index && target < self.tabs.len() {
            let tab = self.tabs.remove(index);
            self.tabs.insert(target, tab);
            let shown = self.tab_x.remove(index);
            self.tab_x.insert(target, shown);
            // the dragged tab is the one on show
            self.active = target;
            self.tab_drag = Some((target, grab));
        }
        self.bar_hover = Some(BarHit::Tab(self.active));
        self.needs_redraw = true;
    }

    /// Slides tabs toward their slots a step a frame. True while any moves.
    fn slide_tabs(&mut self) -> bool {
        let bar = self.bar();
        self.tab_x.resize(self.tabs.len(), 0.0);
        let still = reduced_motion();
        let mut moving = false;
        for (i, x) in self.tab_x.iter_mut().enumerate() {
            if self.tab_drag.is_some_and(|(d, _)| d == i) {
                continue;
            }
            let target = bar.slot_x(i);
            let gap = target - *x;
            if still || gap.abs() < 0.5 {
                *x = target;
            } else {
                *x += gap * 0.35;
                moving = true;
            }
        }
        moving
    }

    /// The line between panes under a window point, a few pixels either
    /// side counting, unless that side is a pane whose app takes the mouse.
    fn divider_at(&self, x: f64, y: f64) -> Option<Divider> {
        if self.areas.len() < 2 {
            return None;
        }
        let (w, h) = self.window_size;
        let bar = self.bar_height() as f32;
        let area = (0.0, bar, w as f32, h as f32 - bar);
        let (x, y) = (x as f32, y as f32);
        if let Some(id) = self.pane_at(x as f64, y as f64) {
            let tracking = if id == self.focused {
                self.term.mouse().0
            } else {
                match self.panes.get(id) {
                    Some(Some(pane)) => pane.term.mouse().0,
                    _ => MouseTracking::Off,
                }
            };
            if tracking != MouseTracking::Off {
                return None;
            }
        }
        let slack = 3.0;
        self.tabs[self.active]
            .0
            .dividers(area, PANE_GAP)
            .into_iter()
            .find(|d| {
                let (lx, ly, lw, lh) = d.line;
                x >= lx - slack && x < lx + lw + slack && y >= ly - slack && y < ly + lh + slack
            })
    }

    /// The pane under a window point, in the tab on show.
    fn pane_at(&self, x: f64, y: f64) -> Option<usize> {
        let (x, y) = (x as f32, y as f32);
        self.areas
            .iter()
            .find(|(_, (ax, ay, w, h), _)| x >= *ax && y >= *ay && x < ax + w && y < ay + h)
            .map(|a| a.0)
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
                self.window_focused = matches!(event, WindowEvent::Focused);
                if !self.window_focused {
                    // an input method may not say its composition ended
                    self.preedit.clear();
                }
                if self.term.focus_tracking() {
                    let _ = self.term.write_str(if self.window_focused {
                        "\x1b[I"
                    } else {
                        "\x1b[O"
                    });
                }
                self.wake_cursor();
            }
            WindowEvent::MouseButton {
                button,
                pressed: true,
                x,
                y,
                click_count,
                ..
            } if (*y as f32) < self.bar_height() as f32 => {
                let bar = self.bar();
                match (button, bar.hit(*x, *y)) {
                    (MouseButton::Left, Some(BarHit::Tab(i))) => {
                        self.switch_tab(i);
                        self.tab_drag = Some((i, *x as f32 - bar.slot_x(i)));
                    }
                    // a second click of a double click lands on whatever slid
                    // under the pointer: only a fresh press closes or opens
                    (MouseButton::Left, Some(BarHit::Close(i))) if *click_count <= 1 => {
                        self.close_tab(i);
                    }
                    (MouseButton::Middle, Some(BarHit::Tab(i) | BarHit::Close(i))) => {
                        self.close_tab(i);
                    }
                    (MouseButton::Left, Some(BarHit::New)) if *click_count <= 1 => self.new_tab(),
                    // a double click on the empty bar opens a tab too
                    (MouseButton::Left, None) if *click_count == 2 => self.new_tab(),
                    _ => {}
                }
                self.needs_redraw = true;
            }
            WindowEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
                ..
            } if self.tab_drag.is_some() => {
                self.tab_drag = None;
                self.needs_redraw = true;
            }
            WindowEvent::MouseMove { x, .. } if self.tab_drag.is_some() => self.drag_tab(*x),
            WindowEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                x,
                y,
                ..
            } if self.divider_at(*x, *y).is_some() => {
                self.dragging = self.divider_at(*x, *y);
            }
            WindowEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
                ..
            } if self.dragging.is_some() => {
                self.dragging = None;
            }
            WindowEvent::MouseMove { x, y, .. } if self.dragging.is_some() => {
                if let Some(line) = &self.dragging {
                    let (px, py, pw, ph) = line.parent;
                    let ratio = match line.dir {
                        Dir::Right => (*x as f32 - px) / pw.max(1.0),
                        Dir::Down => (*y as f32 - py) / ph.max(1.0),
                    };
                    let path = line.path.clone();
                    self.tabs[self.active].0.set_ratio(&path, ratio);
                    self.refit();
                }
            }
            WindowEvent::MouseButton {
                pressed: true,
                x,
                y,
                ..
            } if self.pane_at(*x, *y).is_some_and(|id| id != self.focused) => {
                // a click on another pane only gives it the keyboard
                if let Some(id) = self.pane_at(*x, *y) {
                    self.focus(id);
                }
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
                let hover = self.bar().hit(*x, *y);
                if hover != self.bar_hover {
                    self.bar_hover = hover;
                    self.needs_redraw = true;
                }
                if self.mouse_dragging {
                    self.on_drag(*x, *y);
                } else if self.reported != 0
                    || (self.app_takes_mouse(*modifiers)
                        && self.layout.contains(*x, *y, &self.metrics))
                {
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
        // past the window's top or bottom the view scrolls along; into a
        // pane above or below, the selection just stops at the edge
        let top = self.bar_height() as f64;
        let bottom = self.window_size.1 as f64;
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
        } else if copy_paste && matches!(key, KeyCode::PageUp | KeyCode::PageDown) {
            // ctrl+shift+pageup / pagedown move the tab left or right
            let to = if key == KeyCode::PageUp {
                self.active.checked_sub(1)
            } else {
                Some(self.active + 1).filter(|&i| i < self.tabs.len())
            };
            if let Some(to) = to {
                self.tab_drag = None;
                self.tabs.swap(self.active, to);
                self.tab_x.swap(self.active, to);
                self.active = to;
                self.needs_redraw = true;
            }
        } else if copy_paste && key == KeyCode::T {
            self.new_tab();
        } else if copy_paste && key == KeyCode::W {
            // the last pane stays: closing the window is the close button's job
            self.close_pane(self.focused);
        } else if modifiers.alt && modifiers.shift && key == KeyCode::Equals {
            self.split(Dir::Right);
        } else if modifiers.alt && modifiers.shift && key == KeyCode::Minus {
            self.split(Dir::Down);
        } else if modifiers.alt && modifiers.shift && !modifiers.ctrl && self.nudge_line(key) {
            // a line between panes moved
        } else if let Some(arrow) = arrow(key).filter(|_| {
            modifiers.alt && !modifiers.ctrl && !modifiers.shift && self.areas.len() > 1
        }) {
            let areas: Vec<(usize, Area)> = self.areas.iter().map(|a| (a.0, a.1)).collect();
            if let Some(id) = tabs::neighbour(&areas, self.focused, arrow) {
                self.focus(id);
            }
        } else if modifiers.ctrl && key == KeyCode::Tab && self.tabs.len() > 1 {
            let n = self.tabs.len();
            let next = if modifiers.shift {
                (self.active + n - 1) % n
            } else {
                (self.active + 1) % n
            };
            self.switch_tab(next);
        } else if let Some(i) = tab_digit(key).filter(|_| modifiers.ctrl && modifiers.alt) {
            // ctrl+alt+9 is the last tab, like a browser
            let i = if i == 8 { self.tabs.len() - 1 } else { i };
            self.switch_tab(i);
        } else if copy_paste && key == KeyCode::F {
            if self.search.take().is_none() {
                self.search = Some(Search::default());
            }
            self.needs_redraw = true;
        } else if self.search.is_some() && self.on_search_key(key, modifiers) {
            // the find bar took it
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

    /// Keys while the find bar is open: Enter steps to older matches,
    /// Shift+Enter to newer ones, Escape closes. False for keys it leaves
    /// alone (copy, zoom, scrollback).
    fn on_search_key(&mut self, key: KeyCode, mods: Modifiers) -> bool {
        let Some(search) = &mut self.search else {
            return false;
        };
        match key {
            KeyCode::Escape => self.search = None,
            KeyCode::Enter | KeyCode::Up | KeyCode::Down => {
                // enter and up go to older matches, shift+enter and down to newer
                let older = match key {
                    KeyCode::Enter => !mods.shift,
                    k => k == KeyCode::Up,
                };
                search.step(older);
                self.show_match();
            }
            KeyCode::Backspace => {
                search.query.pop();
                self.find();
            }
            KeyCode::V if mods.ctrl && mods.shift => {
                if let Some(text) = clipboard_text() {
                    let line = text.lines().next().unwrap_or_default().to_owned();
                    search.query.push_str(&line);
                    self.find();
                }
            }
            _ if mods.ctrl || mods.alt => return false,
            KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => return false,
            // the rest are typed, as text input
            _ => {}
        }
        self.needs_redraw = true;
        true
    }

    /// The query changed: search again and show the newest match.
    fn find(&mut self) {
        if let Some(search) = &mut self.search {
            search.stale = true;
            search.jump = true;
            search.query_gen += 1;
        }
        self.poll_search();
    }

    /// Takes a finished search's matches, and starts the next one when the
    /// matches are behind. Searches run on vanta's side, off this thread.
    fn poll_search(&mut self) {
        let Some(search) = &mut self.search else {
            return;
        };
        if let Some(rx) = &search.pending {
            match rx.try_recv() {
                Ok(_) if search.pending_gen != search.query_gen => {
                    // for a query since typed over; its own search is next
                    search.pending = None;
                }
                Ok((dropped, found)) => {
                    search.pending = None;
                    search.set_found(dropped, &found);
                    let jump = take(&mut search.jump);
                    self.needs_redraw = true;
                    if jump {
                        self.show_match();
                    }
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    search.pending = None;
                    search.stale = true;
                }
            }
        }
        let Some(search) = &mut self.search else {
            return;
        };
        if search.due() {
            search.stale = false;
            let (tx, rx) = channel();
            search.pending = Some(rx);
            search.pending_gen = search.query_gen;
            let wake = Arc::clone(&self.wake);
            self.term
                .find_in_background(search.query.clone(), move |found| {
                    let _ = tx.send(found);
                    wake();
                });
        }
    }

    /// Scrolls so the current match is on view, near the middle.
    fn show_match(&mut self) {
        let Some(((line, _), _)) = self.search.as_ref().and_then(Search::current) else {
            return;
        };
        let Some(index) = line
            .checked_sub(self.term.dropped())
            .and_then(|l| usize::try_from(l).ok())
        else {
            return;
        };
        let rows = self.layout.rows;
        let first = self.term.first();
        if index < first || index >= first + rows {
            let top = index.saturating_sub(rows / 2);
            self.scroll_offset = self.term.scrollback_len().saturating_sub(top);
        }
        self.needs_redraw = true;
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
        // over another pane: not this one's to scroll
        let (px, py) = self.pointer;
        if self.pane_at(px, py).is_some_and(|id| id != self.focused) {
            return;
        }
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
        if self.search.is_some() {
            let text = self.text_input.normalize(text);
            if let Some(search) = &mut self.search {
                search
                    .query
                    .extend(text.chars().filter(|c| !c.is_control()));
            }
            self.find();
            return;
        }
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
    fn pump_shell(&mut self) {
        let blink = Duration::from_millis(self.config.cursor.blink_interval_ms);
        if follow(&mut self.term, &mut self.scroll_offset) {
            self.retitle = true;
            if let Some(search) = &mut self.search {
                search.stale = true;
            }
            self.wake_cursor();
            self.last_output = Instant::now();
            self.take_events();
            if self.term.palette_version() != self.palette_version {
                self.palette_version = self.term.palette_version();
                self.colors = Colors::from_palette(&self.term.palette());
            }
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

    /// A config the watcher read: applied when it parsed, else its error
    /// shows and the config in force stays.
    fn take_config(&mut self) {
        let Some((config, problem)) = self.config_changes.try_iter().last() else {
            return;
        };
        if let Some(problem) = problem {
            self.diagnostics = vec![problem];
            self.diagnostics_until = Some(Instant::now() + DIAGNOSTICS_FOR);
            self.needs_redraw = true;
            return;
        }
        let mut config = config;
        // a zoom holds unless the file's own size changed
        if config.font.size == self.base_font_size {
            self.base_font_size = config.font.size;
            config.font.size = self.config.font.size;
        } else {
            self.base_font_size = config.font.size;
        }
        let old = &self.config.font;
        let font = &config.font;
        self.refont |= old.family != font.family
            || old.fallbacks != font.fallbacks
            || old.size != font.size
            || old.line_height != font.line_height;
        let padding = self.config.window.padding != config.window.padding
            || self.config.window.always_show_tabs != config.window.always_show_tabs;
        self.config = config;
        if padding {
            self.refit();
        }
        self.diagnostics.clear();
        self.wake_cursor();
    }

    /// Remeasures the cells for a new font size and refits the grid.
    fn set_font_size(&mut self, size: f32, canvas: &Canvas) {
        if size == self.config.font.size {
            return;
        }
        self.config.font.size = size;
        self.remeasure(canvas);
    }

    /// Builds the fonts again from the config and refits the grid to the
    /// new cell size.
    fn remeasure(&mut self, canvas: &Canvas) {
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
            match event {
                Event::Clipboard(text) if self.config.terminal.clipboard_write => {
                    let _ = set_clipboard_text(&text);
                }
                Event::Bell if self.config.bell.visual => {
                    self.flash = Some(Instant::now());
                    self.needs_redraw = true;
                }
                _ => {}
            }
        }
    }

    /// The app's DECSCUSR shape, else the configured one; an outline
    /// while another window has the keyboard.
    fn cursor_shape(&self) -> CursorShape {
        if !self.window_focused {
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
        wanted && self.window_focused && self.term.app_cursor_visible()
    }

    /// The window title: the tab on show's.
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

    /// Panes on show without the keyboard, each on its own background,
    /// with an outline cursor, and the lines between panes.
    fn draw_other_panes(&self, ctx: &mut dyn DrawingContext) -> AureaResult<()> {
        for &(id, (x, y, w, h), layout) in &self.areas {
            if x > 0.0 {
                ctx.draw_rect(
                    Rect::new(x - PANE_GAP, y, PANE_GAP, h),
                    &solid(theme::DIVIDER),
                )?;
            }
            if y > self.bar_height() as f32 {
                ctx.draw_rect(
                    Rect::new(x, y - PANE_GAP, w, PANE_GAP),
                    &solid(theme::DIVIDER),
                )?;
            }
            let Some(Some(pane)) = self.panes.get(id) else {
                continue;
            };
            ctx.draw_rect(Rect::new(x, y, w, h), &solid(pane.colors.background))?;
            let cursor = pane
                .term
                .cursor()
                .filter(|_| pane.term.app_cursor_visible());
            render::draw_grid(
                ctx,
                pane.term.rows(),
                cursor,
                &self.metrics,
                &self.fonts,
                &CursorShape::Hollow,
                &layout,
                None,
                &[],
                &pane.colors,
            )?;
        }
        Ok(())
    }

    fn draw(&mut self, canvas: &mut Canvas) -> AureaResult<()> {
        let selection = self.selection_on_view();
        let top = self.term.dropped() + self.term.first() as u64;
        let found = self
            .search
            .as_ref()
            .map(|s| s.pieces(top, self.term.rows().len()))
            .unwrap_or_default();
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
            if self.areas.len() > 1 {
                self.draw_other_panes(ctx)?;
                let (x, y, w, h) = self
                    .areas
                    .iter()
                    .find(|a| a.0 == self.focused)
                    .map_or((0.0, 0.0, 0.0, 0.0), |a| a.1);
                ctx.draw_rect(Rect::new(x, y, w, h), &solid(self.colors.background))?;
            }
            render::draw_grid(
                ctx,
                rows,
                cursor,
                &self.metrics,
                &self.fonts,
                &shape,
                &self.layout,
                selection,
                &found,
                &self.colors,
            )?;
            let bar = self.bar_height() as f32;
            if bar > 0.0 {
                render::draw_tab_bar(
                    ctx,
                    &self.bar(),
                    &self.tab_labels(),
                    self.bar_hover == Some(BarHit::New),
                    self.window_size.0 as f32,
                    &self.metrics,
                    &self.fonts,
                    &self.colors,
                )?;
            }
            if let Some(search) = &self.search {
                render::draw_find_bar(
                    ctx,
                    &search.query,
                    search.count(),
                    // the focused pane's top right
                    self.areas
                        .iter()
                        .find(|a| a.0 == self.focused)
                        .map_or((self.window_size.0 as f32, bar), |a| (a.1.0 + a.1.2, a.1.1)),
                    &self.metrics,
                    &self.fonts,
                    &self.colors,
                )?;
            }
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
            if let Some(start) = self.flash {
                // a light wash over the pane, fading out
                let left = 1.0 - start.elapsed().as_secs_f32() / FLASH_FOR.as_secs_f32();
                if left > 0.0 {
                    let (x, y, w, h) = self
                        .areas
                        .iter()
                        .find(|a| a.0 == self.focused)
                        .map_or((0.0, 0.0, 0.0, 0.0), |a| a.1);
                    let alpha = (left * 40.0) as u8;
                    ctx.draw_rect(
                        Rect::new(x, y, w, h),
                        &solid(Color::rgba(255, 255, 255, alpha)),
                    )?;
                }
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
