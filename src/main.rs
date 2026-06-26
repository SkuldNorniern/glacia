// Release builds run as a GUI app — without this, Windows allocates a console
// window alongside the terminal's own window. Kept active in debug builds so
// `cargo run` still shows panics/stderr in the launching console.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod canvas;
mod config;
mod input;
mod platform;
mod plugin;
mod render_cpu;
mod terminal;
mod theme;
mod unicode;

use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use aurea::ffi::ng_platform_poll_events;
use aurea::render::{Canvas, Font, RendererBackend};
use aurea::{AureaResult, KeyCode, MouseButton, Window, WindowEvent};
use aurea::{clipboard_text, set_clipboard_text};

use canvas::{SendableCanvas, SharedCanvas, lock};
use config::Config;
use render_cpu::{CellMetrics, CellRowsView, RowFonts, SelectionRange};
use terminal::{SpawnOverrides, TerminalSession};
use vanta::{Cell, CellKind};

const POLL_INTERVAL: Duration = Duration::from_millis(8);
const FRAME_INTERVAL: Duration = Duration::from_micros(16_667); // 60 FPS render cap
const DEFAULT_TITLE: &str = "Glacia";

/// Measure font metrics and build the cell sizing struct.
///
/// Width comes from the actual advance of 'M' so the cursor stays aligned with
/// the rendered glyphs. Height and baseline_offset come from the font's true
/// ascent+descent rather than a fixed fraction, preventing the last row's
/// descenders from being clipped at the canvas edge. Also probes which
/// codepoints the primary font lacks so fallback routing is automatic.
fn cell_metrics(
    config: &Config,
    canvas: &Arc<Mutex<SendableCanvas>>,
    row_fonts: &mut RowFonts,
) -> CellMetrics {
    let configured_h = config.font.size * config.font.line_height;
    let mut width = config.font.size * 0.6;
    let mut height = configured_h;
    let mut baseline_offset = configured_h * 0.8;
    let mut ascent = configured_h * 0.8;

    let _ = lock(canvas.as_ref()).draw(|ctx| {
        if let Ok(m) = ctx.measure_text("M", &row_fonts.primary) {
            if m.ascent > 0.0 {
                let font_h = m.ascent + m.descent;
                let leading = font_h * (config.font.line_height - 1.0);
                // Centre the line_height gap evenly above and below the glyph.
                baseline_offset = leading / 2.0 + m.ascent;
                height = font_h + leading;
                ascent = m.ascent;
            }
            if m.advance > 0.0 {
                width = m.advance;
            }
        }
        row_fonts.probe(ctx, width);
        Ok(())
    });

    CellMetrics {
        width,
        height,
        baseline_offset,
        ascent,
    }
}

fn grid_size(width: u32, height: u32, metrics: &CellMetrics, padding: u32) -> (u16, u16) {
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

/// Extract plain text for the cells covered by `sel` from the given rows.
fn extract_selection_text(rows: &[Vec<Cell>], sel: SelectionRange) -> String {
    let mut result = String::new();
    let (r1, _) = sel.start;
    let (r2, _) = sel.end;
    for row_idx in r1..=r2 {
        let Some(row) = rows.get(row_idx) else { break };
        let (sc, ec) = match sel.cols_for_row(row_idx, row.len()) {
            Some(r) => r,
            None => continue,
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

/// Wait for the window to be closed, redrawing `message` as a fatal
/// startup notice — used when there's no terminal session to render
/// alongside (e.g. the shell failed to spawn).
fn wait_for_close_with_message(
    window: &Window,
    canvas_arc: &Arc<Mutex<SendableCanvas>>,
    font: &Font,
    message: &str,
) -> AureaResult<()> {
    {
        let mut canvas = lock(canvas_arc.as_ref());
        canvas.draw(|ctx| render_cpu::draw_fatal_message(ctx, message, font))?;
    }
    loop {
        unsafe { ng_platform_poll_events() };
        let events = window.poll_events();
        if events
            .iter()
            .any(|event| matches!(event, WindowEvent::CloseRequested))
        {
            break;
        }
        window.process_frames()?;
        sleep(POLL_INTERVAL);
    }
    Ok(())
}

fn main() -> AureaResult<()> {
    let (config, config_diagnostic) = Config::load();
    let diagnostics: Vec<String> = config_diagnostic.into_iter().collect();

    let plugins = platform::plugins_dir()
        .map(|d| plugin::load_plugins(&d))
        .unwrap_or_default();
    let _ = plugins; // available for the oxygen scripting engine when wired in

    // Prefer PowerShell on Windows when the user hasn't set an explicit shell.
    let resolved_shell = if config.terminal.shell.is_empty() {
        platform::preferred_shell().unwrap_or_default()
    } else {
        config.terminal.shell.clone()
    };
    let padding = config.window.padding;
    let initial_metrics = CellMetrics {
        width: config.font.size * 0.6,
        height: config.font.size * config.font.line_height,
        baseline_offset: config.font.size * config.font.line_height * 0.8,
        ascent: config.font.size * config.font.line_height * 0.8,
    };
    let (initial_cols, initial_rows) = grid_size(
        config.window.width,
        config.window.height,
        &initial_metrics,
        padding,
    );
    // On macOS, fork() after GUI/canvas/font runtime initialization can leave
    // the child in a bad state. Spawn the PTY first, then resize it after exact
    // font metrics are known.
    let term_result = TerminalSession::spawn(SpawnOverrides {
        cols: initial_cols,
        rows: initial_rows,
        shell: &resolved_shell,
        args: platform::shell_args(&resolved_shell),
        env: platform::terminal_env(),
        working_directory: &config.terminal.working_directory,
    });

    let mut window = Window::new(
        DEFAULT_TITLE,
        config.window.width as i32,
        config.window.height as i32,
    )?;

    let raw_canvas = Canvas::new(
        config.window.width,
        config.window.height,
        RendererBackend::Cpu,
    )?;
    // A registered draw callback is required even though the loop below
    // redraws manually via `canvas.draw()`: without one, OS-initiated
    // repaints (e.g. the window's first paint) have nothing to call back
    // into and the surface stays blank.
    // canvas.draw() always clears to background_color before our closure runs;
    // setting it here means both the retained-mode callback and our imperative
    // draw path start from the correct dark color — no redundant white clear.
    raw_canvas.set_background_color(theme::BACKGROUND);
    raw_canvas.set_draw_callback(|ctx| ctx.clear(theme::BACKGROUND))?;
    let canvas_arc = Arc::new(Mutex::new(SendableCanvas(raw_canvas)));
    window.set_content(SharedCanvas(canvas_arc.clone()))?;

    // Build fonts once. RowFonts owns the bold variants so they are not
    // re-cloned on every frame.
    let primary_font = Font::new(&config.font.family, config.font.size);
    let mut fallback_families = config.font.fallbacks.clone();
    fallback_families.extend(
        platform::default_fallback_fonts()
            .iter()
            .map(|family| (*family).to_owned()),
    );
    fallback_families.dedup();
    fallback_families.retain(|family| family != &config.font.family);
    let fallback_fonts = fallback_families
        .iter()
        .map(|family| Font::new(family, config.font.size))
        .collect();
    // Build RowFonts first so cell_metrics can probe font support in the same
    // canvas draw call that measures the cell width.
    let mut row_fonts = RowFonts::new(primary_font, fallback_fonts);
    let metrics = cell_metrics(&config, &canvas_arc, &mut row_fonts);

    let (cols, rows) = grid_size(config.window.width, config.window.height, &metrics, padding);
    let mut term = match term_result {
        Ok(term) => term,
        Err(err) => {
            let message = format!("failed to spawn default shell: {err}");
            return wait_for_close_with_message(&window, &canvas_arc, &row_fonts.primary, &message);
        }
    };
    let _ = term.resize(cols, rows);

    let mut cursor_visible = true;
    let mut last_blink = Instant::now();
    let mut last_render = Instant::now();
    // Track when output last arrived; blink timer is suppressed until the PTY
    // has been idle for at least one full blink interval (avoids cursor
    // flickering during heavy output like `cat large-file`).
    let mut last_output = Instant::now();
    let mut needs_redraw = true;
    let mut window_size = (config.window.width, config.window.height);
    let mut last_title: Option<String> = None;
    let mut scroll_offset: usize = 0;
    let mut text_input = input::TextInputNormalizer::new();
    // Mouse text selection state.
    let mut sel_anchor: Option<(usize, usize)> = None; // (row, col) where drag started
    let mut sel_end: Option<(usize, usize)> = None; // (row, col) where drag is/ended
    let mut mouse_dragging = false;

    loop {
        unsafe { ng_platform_poll_events() };

        let events = window.poll_events();
        let mut should_close = false;
        for event in &events {
            match event {
                WindowEvent::CloseRequested => should_close = true,
                WindowEvent::Resized { width, height } => {
                    window_size = (*width, *height);
                    let (cols, rows) = grid_size(*width, *height, &metrics, padding);
                    let _ = term.resize(cols, rows);
                    scroll_offset = 0;
                    sel_anchor = None;
                    sel_end = None;
                    last_render = Instant::now()
                        .checked_sub(FRAME_INTERVAL)
                        .unwrap_or(Instant::now());
                    needs_redraw = true;
                }
                WindowEvent::ScaleFactorChanged { .. } => {
                    // Let the canvas sync loop pick up the new logical dimensions
                    // on the next iteration — nothing to do explicitly here.
                    needs_redraw = true;
                }
                WindowEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed,
                    x,
                    y,
                    ..
                } => {
                    if *pressed {
                        let cell = px_to_cell(*x, *y, &metrics, padding as f32);
                        sel_anchor = Some(cell);
                        sel_end = Some(cell);
                        mouse_dragging = true;
                    } else {
                        mouse_dragging = false;
                    }
                    needs_redraw = true;
                }
                WindowEvent::MouseMove { x, y } => {
                    if mouse_dragging {
                        sel_end = Some(px_to_cell(*x, *y, &metrics, padding as f32));
                        needs_redraw = true;
                    }
                }
                WindowEvent::KeyInput {
                    key,
                    pressed: true,
                    modifiers,
                } => {
                    // Ctrl+Shift+C: copy selected text to clipboard.
                    if modifiers.ctrl && modifiers.shift && *key == KeyCode::C {
                        if let (Some(anchor), Some(end)) = (sel_anchor, sel_end) {
                            let sel = SelectionRange::new(anchor, end);
                            let text = extract_selection_text(term.cells(), sel);
                            if !text.is_empty() {
                                let _ = set_clipboard_text(&text);
                            }
                        }
                    // Ctrl+Shift+V: paste clipboard with optional bracketed-paste wrapping.
                    } else if modifiers.ctrl && modifiers.shift && *key == KeyCode::V {
                        if let Some(text) = clipboard_text() {
                            scroll_offset = 0;
                            let pending = text_input.flush();
                            if !pending.is_empty() {
                                let _ = term.write_str(&pending);
                            }
                            if term.bracketed_paste_enabled() {
                                let _ = term.write_str("\x1b[200~");
                                let _ = term.write_str(&text);
                                let _ = term.write_str("\x1b[201~");
                            } else {
                                let _ = term.write_str(&text);
                            }
                            cursor_visible = true;
                            last_blink = Instant::now();
                            needs_redraw = true;
                        }
                    } else if term.is_alt_screen() {
                        // On the alternate screen the app owns scrolling; route
                        // PageUp/Down as standard VT sequences so vim/less/htop
                        // can respond. Other keys fall through to normal routing.
                        let forwarded = match key {
                            KeyCode::PageUp => Some("\x1b[5~"),
                            KeyCode::PageDown => Some("\x1b[6~"),
                            _ => None,
                        };
                        if let Some(seq) = forwarded {
                            let pending = text_input.flush();
                            if !pending.is_empty() {
                                let _ = term.write_str(&pending);
                            }
                            let _ = term.write_str(seq);
                            cursor_visible = true;
                            last_blink = Instant::now();
                            needs_redraw = true;
                        } else if let Some(bytes) = input::terminal_key_bytes(*key, *modifiers) {
                            sel_anchor = None;
                            sel_end = None;
                            let pending = text_input.flush();
                            if !pending.is_empty() {
                                let _ = term.write_str(&pending);
                            }
                            let _ = term.write_str(&bytes);
                            cursor_visible = true;
                            last_blink = Instant::now();
                            needs_redraw = true;
                        }
                    } else {
                        let visible_rows =
                            (window_size.1 as f32 / metrics.height).max(1.0) as usize;
                        let scroll_page = visible_rows / 2;
                        let max_scroll = term.scrollback_rows().len();
                        match key {
                            KeyCode::PageUp => {
                                scroll_offset = (scroll_offset + scroll_page).min(max_scroll);
                                sel_anchor = None;
                                sel_end = None;
                                needs_redraw = true;
                            }
                            KeyCode::PageDown => {
                                scroll_offset = scroll_offset.saturating_sub(scroll_page);
                                sel_anchor = None;
                                sel_end = None;
                                needs_redraw = true;
                            }
                            // Home jumps to the oldest scrollback line.
                            KeyCode::Home if scroll_offset < max_scroll => {
                                scroll_offset = max_scroll;
                                sel_anchor = None;
                                sel_end = None;
                                needs_redraw = true;
                            }
                            // End snaps back to the live view; if already live
                            // the key falls through to the PTY as normal.
                            KeyCode::End if scroll_offset > 0 => {
                                scroll_offset = 0;
                                needs_redraw = true;
                            }
                            _ => {
                                if let Some(bytes) = input::terminal_key_bytes(*key, *modifiers) {
                                    scroll_offset = 0;
                                    sel_anchor = None;
                                    sel_end = None;
                                    let pending = text_input.flush();
                                    if !pending.is_empty() {
                                        let _ = term.write_str(&pending);
                                    }
                                    let _ = term.write_str(&bytes);
                                    cursor_visible = true;
                                    last_blink = Instant::now();
                                    needs_redraw = true;
                                }
                            }
                        }
                    }
                }
                WindowEvent::MouseWheel { delta_y, .. } => {
                    // Sign convention differs per native backend, not just per OS:
                    // - macOS (NSEvent, natural scroll): positive = fingers moving
                    //   up = content moves up = older lines come into view.
                    // - Windows (WM_MOUSEWHEEL, both mouse wheel and Precision
                    //   Touchpad): GET_WHEEL_DELTA_WPARAM is positive for the
                    //   "away from user" rotation, which also means scroll up —
                    //   same convention as macOS.
                    // - Linux (GTK GdkEventScroll): dy is negative for
                    //   GDK_SCROLL_UP — the opposite convention.
                    #[cfg(any(target_os = "macos", target_os = "windows"))]
                    let scroll_up = *delta_y > 0.0;
                    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                    let scroll_up = *delta_y < 0.0;

                    if term.is_alt_screen() {
                        // Forward wheel as arrow-key repeats so vim/less/htop
                        // scroll naturally. The app sees the same sequences it
                        // would receive from keyboard arrow presses.
                        let lines = (delta_y.abs() * 3.0).ceil() as usize;
                        let seq = if scroll_up { "\x1b[A" } else { "\x1b[B" };
                        for _ in 0..lines {
                            let _ = term.write_str(seq);
                        }
                        needs_redraw = true;
                    } else {
                        let lines = (delta_y.abs() * 3.0).ceil() as usize;
                        if scroll_up {
                            let max_scroll = term.scrollback_rows().len();
                            scroll_offset = (scroll_offset + lines).min(max_scroll);
                        } else {
                            scroll_offset = scroll_offset.saturating_sub(lines);
                        }
                        needs_redraw = true;
                    }
                }
                WindowEvent::TextInput { text } => {
                    let printable: String = text_input
                        .normalize(text)
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect();
                    if !printable.is_empty() {
                        scroll_offset = 0;
                        sel_anchor = None;
                        sel_end = None;
                        let _ = term.write_str(&printable);
                        cursor_visible = true;
                        last_blink = Instant::now();
                        needs_redraw = true;
                    }
                }
                _ => {}
            }
        }
        if should_close {
            break;
        }

        // Sync window_size with the canvas's actual rendered dimensions each frame.
        // canvas.draw() calls check_and_resize() internally, so canvas.size() reflects
        // the real platform allocation — which may differ from config or Resized events
        // by a few pixels (title-bar inclusion, DPI rounding). Using stale dimensions
        // makes the grid slightly too large, causing the last row/column to be clipped.
        {
            let actual = lock(canvas_arc.as_ref()).size();
            if actual.0 > 0 && actual.1 > 0 && actual != window_size {
                window_size = actual;
                let (nc, nr) = grid_size(actual.0, actual.1, &metrics, padding);
                let _ = term.resize(nc, nr);
                needs_redraw = true;
            }
        }

        if term.sync() {
            cursor_visible = true;
            last_blink = Instant::now();
            last_output = Instant::now();
            needs_redraw = true;
            // Entering alt screen (TUI app launched): clear any existing
            // scrollback offset so the full alt-screen is immediately visible.
            if term.is_alt_screen() {
                scroll_offset = 0;
            }

            let title = term.title();
            if title != last_title {
                let shown = match &title {
                    Some(title) => format!("{DEFAULT_TITLE} - {}", display_shell_name(title)),
                    None => DEFAULT_TITLE.to_owned(),
                };
                let _ = window.set_title(&shown);
                last_title = title;
            }
        } else if config.cursor.blink
            && last_output.elapsed() >= Duration::from_millis(config.cursor.blink_interval_ms)
            && last_blink.elapsed() >= Duration::from_millis(config.cursor.blink_interval_ms)
        {
            cursor_visible = !cursor_visible;
            last_blink = Instant::now();
            needs_redraw = true;
        }

        // TUI apps (vim, htop, …) hide the cursor via DECTCEM and draw their
        // own. Override the blink state so we never draw a ghost block on top.
        if !term.app_cursor_visible() {
            cursor_visible = false;
        }

        if !term.is_running() {
            break;
        }

        if needs_redraw && last_render.elapsed() >= FRAME_INTERVAL {
            let pad = padding as f32;
            let visible_rows = ((window_size.1.saturating_sub(2 * padding)) as f32 / metrics.height)
                .max(1.0) as usize;
            let active_sel = sel_anchor
                .zip(sel_end)
                .map(|(a, e)| SelectionRange::new(a, e));
            let mut canvas = lock(canvas_arc.as_ref());
            // `canvas.draw()` invalidates the native surface itself; an extra
            // `invalidate_all()` here would trigger a second native repaint
            // that re-runs the registered `set_draw_callback` (background
            // only) and clobbers this frame's real content.
            canvas.draw(|ctx| {
                if scroll_offset == 0 {
                    render_cpu::draw_grid(
                        ctx,
                        &CellRowsView::from_screen(term.cells()),
                        term.cursor(),
                        cursor_visible,
                        &metrics,
                        &row_fonts,
                        &config.cursor.shape,
                        pad,
                        active_sel,
                    )?;
                } else {
                    let sb = term.scrollback_rows();
                    let screen = term.cells();
                    let total = sb.len() + screen.len();
                    let end = total.saturating_sub(scroll_offset);
                    let start = end.saturating_sub(visible_rows);
                    render_cpu::draw_grid(
                        ctx,
                        &CellRowsView::from_split(sb, screen, start, end),
                        (usize::MAX, usize::MAX),
                        false,
                        &metrics,
                        &row_fonts,
                        &config.cursor.shape,
                        pad,
                        None,
                    )?;
                }
                if let Some(message) = diagnostics.first() {
                    render_cpu::draw_diagnostics_banner(
                        ctx,
                        message,
                        window_size.0 as f32,
                        window_size.1 as f32,
                        &row_fonts.primary,
                    )?;
                }
                Ok(())
            })?;
            needs_redraw = false;
            last_render = Instant::now();
        }

        window.process_frames()?;
        sleep(POLL_INTERVAL);
    }

    Ok(())
}
