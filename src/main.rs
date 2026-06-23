mod canvas;
mod config;
mod input;
mod render_cpu;
mod terminal;
mod theme;

use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use aurea::ffi::ng_platform_poll_events;
use aurea::render::{Canvas, Font, RendererBackend};
use aurea::{AureaResult, KeyCode, Window, WindowEvent};

use canvas::{SendableCanvas, SharedCanvas, lock};
use config::Config;
use render_cpu::CellMetrics;
use terminal::{SpawnOverrides, TerminalSession};
use vanta::Cell;

const POLL_INTERVAL: Duration = Duration::from_millis(8);
const DEFAULT_TITLE: &str = "Glacia";

/// Measure the font's actual horizontal advance for a representative glyph
/// rather than guessing `font_size * 0.6` — the guess drifts visibly from
/// the real rendered text over enough columns (the cursor block creeps away
/// from the character it should sit on). Falls back to the guess if
/// measurement fails or returns something degenerate.
fn cell_metrics(config: &Config, canvas: &Arc<Mutex<SendableCanvas>>, font: &Font) -> CellMetrics {
    let mut width = config.font.size * 0.6;
    let _ = lock(canvas.as_ref()).draw(|ctx| {
        let measured = ctx.measure_text("M", font)?;
        if measured.advance > 0.0 {
            width = measured.advance;
        }
        Ok(())
    });
    CellMetrics {
        width,
        height: config.font.size * config.font.line_height,
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

    let font = Font::new(&config.font.family, config.font.size);
    let metrics = cell_metrics(&config, &canvas_arc, &font);
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
            return wait_for_close_with_message(&window, &canvas_arc, &font, &message);
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
                    needs_redraw = true;
                }
                WindowEvent::KeyInput {
                    key,
                    pressed: true,
                    modifiers,
                } => {
                    let visible_rows = (window_size.1 as f32 / metrics.height).max(1.0) as usize;
                    let scroll_page = visible_rows / 2;
                    match key {
                        KeyCode::PageUp => {
                            let max_scroll = term.scrollback_rows().len();
                            scroll_offset = (scroll_offset + scroll_page).min(max_scroll);
                            needs_redraw = true;
                        }
                        KeyCode::PageDown => {
                            scroll_offset = scroll_offset.saturating_sub(scroll_page);
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

        if !term.is_running() {
            break;
        }

        if needs_redraw {
            let mut canvas = lock(canvas_arc.as_ref());
            // `canvas.draw()` invalidates the native surface itself; an extra
            // `invalidate_all()` here would trigger a second native repaint
            // that re-runs the registered `set_draw_callback` (background
            // only) and clobbers this frame's real content.
            canvas.draw(|ctx| {
                let visible_rows = (window_size.1 as f32 / metrics.height).max(1.0) as usize;
                if scroll_offset == 0 {
                    render_cpu::draw_grid(
                        ctx,
                        term.cells(),
                        term.cursor(),
                        cursor_visible,
                        &metrics,
                        &font,
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
                        &font,
                    )?;
                }
                if let Some(message) = diagnostics.first() {
                    render_cpu::draw_diagnostics_banner(
                        ctx,
                        message,
                        window_size.0 as f32,
                        window_size.1 as f32,
                        &font,
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
