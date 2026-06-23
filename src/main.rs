mod canvas;
mod config;
mod input;
mod platform;
mod plugin;
mod render_cpu;
mod terminal;
mod theme;

use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use aurea::clipboard_text;
use aurea::ffi::ng_platform_poll_events;
use aurea::render::{Canvas, Font, RendererBackend};
use aurea::{AureaResult, KeyCode, Window, WindowEvent};

use canvas::{SendableCanvas, SharedCanvas, lock};
use config::Config;
use render_cpu::{CellMetrics, RowFonts};
use terminal::{SpawnOverrides, TerminalSession};
use vanta::Cell;

const POLL_INTERVAL: Duration = Duration::from_millis(8);
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

    let _ = lock(canvas.as_ref()).draw(|ctx| {
        if let Ok(m) = ctx.measure_text("M", &row_fonts.primary) {
            if m.advance > 0.0 {
                width = m.advance;
            }
            if m.ascent > 0.0 {
                let font_h = m.ascent + m.descent;
                let leading = font_h * (config.font.line_height - 1.0);
                // Centre the line_height gap evenly above and below the glyph.
                baseline_offset = leading / 2.0 + m.ascent;
                height = font_h + leading;
            }
        }
        row_fonts.probe(ctx, width);
        Ok(())
    });

    CellMetrics {
        width,
        height,
        baseline_offset,
    }
}

fn grid_size(width: u32, height: u32, metrics: &CellMetrics) -> (u16, u16) {
    let cols = (width as f32 / metrics.width).max(1.0) as u16;
    let rows = (height as f32 / metrics.height).max(1.0) as u16;
    (cols, rows)
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
    raw_canvas.set_draw_callback(|ctx| ctx.clear(theme::BACKGROUND))?;
    let canvas_arc = Arc::new(Mutex::new(SendableCanvas(raw_canvas)));
    window.set_content(SharedCanvas(canvas_arc.clone()))?;

    // Build fonts once. RowFonts owns the bold variants so they are not
    // re-cloned on every frame.
    let primary_font = Font::new(&config.font.family, config.font.size);
    let fallback_font = if config.font.fallback.is_empty() {
        None
    } else {
        Some(Font::new(&config.font.fallback, config.font.size))
    };
    // Build RowFonts first so cell_metrics can probe font support in the same
    // canvas draw call that measures the cell width.
    let mut row_fonts = RowFonts::new(primary_font, fallback_font);
    let metrics = cell_metrics(&config, &canvas_arc, &mut row_fonts);

    let (cols, rows) = grid_size(config.window.width, config.window.height, &metrics);
    let mut term = match TerminalSession::spawn(SpawnOverrides {
        cols,
        rows,
        shell: &config.terminal.shell,
        working_directory: &config.terminal.working_directory,
    }) {
        Ok(term) => term,
        Err(err) => {
            let message = format!("failed to spawn default shell: {err}");
            return wait_for_close_with_message(&window, &canvas_arc, &row_fonts.primary, &message);
        }
    };

    let mut cursor_visible = true;
    let mut last_blink = Instant::now();
    let mut needs_redraw = true;
    let mut window_size = (config.window.width, config.window.height);
    let mut last_title: Option<String> = None;
    let mut scroll_offset: usize = 0;

    loop {
        unsafe { ng_platform_poll_events() };

        let events = window.poll_events();
        let mut should_close = false;
        for event in &events {
            match event {
                WindowEvent::CloseRequested => should_close = true,
                WindowEvent::Resized { width, height } => {
                    window_size = (*width, *height);
                    let (cols, rows) = grid_size(*width, *height, &metrics);
                    let _ = term.resize(cols, rows);
                    scroll_offset = 0;
                    needs_redraw = true;
                }
                WindowEvent::KeyInput {
                    key,
                    pressed: true,
                    modifiers,
                } => {
                    // Ctrl+Shift+V: paste clipboard with optional bracketed-paste wrapping.
                    if modifiers.ctrl && modifiers.shift && *key == KeyCode::V {
                        if let Some(text) = clipboard_text() {
                            scroll_offset = 0;
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
                            let _ = term.write_str(seq);
                            cursor_visible = true;
                            last_blink = Instant::now();
                            needs_redraw = true;
                        } else if let Some(bytes) = input::terminal_key_bytes(*key, *modifiers) {
                            let _ = term.write_str(bytes);
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
                                needs_redraw = true;
                            }
                            KeyCode::PageDown => {
                                scroll_offset = scroll_offset.saturating_sub(scroll_page);
                                needs_redraw = true;
                            }
                            // Home jumps to the oldest scrollback line.
                            KeyCode::Home if scroll_offset < max_scroll => {
                                scroll_offset = max_scroll;
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
                                    let _ = term.write_str(bytes);
                                    cursor_visible = true;
                                    last_blink = Instant::now();
                                    needs_redraw = true;
                                }
                            }
                        }
                    }
                }
                WindowEvent::MouseWheel { delta_y, .. } => {
                    if term.is_alt_screen() {
                        // Forward wheel as arrow-key repeats so vim/less/htop
                        // scroll naturally. The app sees the same sequences it
                        // would receive from keyboard arrow presses.
                        let lines = (delta_y.abs() * 3.0).ceil() as usize;
                        let seq = if *delta_y < 0.0 { "\x1b[A" } else { "\x1b[B" };
                        for _ in 0..lines {
                            let _ = term.write_str(seq);
                        }
                        needs_redraw = true;
                    } else {
                        let lines = (delta_y.abs() * 3.0).ceil() as usize;
                        if *delta_y < 0.0 {
                            let max_scroll = term.scrollback_rows().len();
                            scroll_offset = (scroll_offset + lines).min(max_scroll);
                        } else {
                            scroll_offset = scroll_offset.saturating_sub(lines);
                        }
                        needs_redraw = true;
                    }
                }
                WindowEvent::TextInput { text } => {
                    let printable: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() {
                        scroll_offset = 0;
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

        if term.sync() {
            cursor_visible = true;
            last_blink = Instant::now();
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

        if needs_redraw {
            let visible_rows = (window_size.1 as f32 / metrics.height).max(1.0) as usize;
            let mut canvas = lock(canvas_arc.as_ref());
            // `canvas.draw()` invalidates the native surface itself; an extra
            // `invalidate_all()` here would trigger a second native repaint
            // that re-runs the registered `set_draw_callback` (background
            // only) and clobbers this frame's real content.
            canvas.draw(|ctx| {
                if scroll_offset == 0 {
                    render_cpu::draw_grid(
                        ctx,
                        term.cells(),
                        term.cursor(),
                        cursor_visible,
                        &metrics,
                        &row_fonts,
                    )?;
                } else {
                    let sb = term.scrollback_rows();
                    let screen = term.cells();
                    let total = sb.len() + screen.len();
                    let end = total.saturating_sub(scroll_offset);
                    let start = end.saturating_sub(visible_rows);
                    let view: Vec<Vec<Cell>> = (start..end)
                        .map(|i| {
                            if i < sb.len() {
                                sb[i].clone()
                            } else {
                                screen[i - sb.len()].clone()
                            }
                        })
                        .collect();
                    render_cpu::draw_grid(
                        ctx,
                        &view,
                        (usize::MAX, usize::MAX),
                        false,
                        &metrics,
                        &row_fonts,
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
        }

        window.process_frames()?;
        sleep(POLL_INTERVAL);
    }

    Ok(())
}
