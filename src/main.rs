mod canvas;
mod input;
mod render_cpu;
mod terminal;
mod theme;

use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use aurea::ffi::ng_platform_poll_events;
use aurea::render::{Canvas, Font, RendererBackend};
use aurea::{AureaResult, Window, WindowEvent};

use canvas::{SendableCanvas, SharedCanvas, lock};
use render_cpu::CellMetrics;
use terminal::TerminalSession;

const WINDOW_WIDTH: u32 = 1280;
const WINDOW_HEIGHT: u32 = 800;
const POLL_INTERVAL: Duration = Duration::from_millis(8);
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 1.25;
const FONT_FAMILY: &str = "Consolas";
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(530);

fn cell_metrics() -> CellMetrics {
    CellMetrics {
        width: FONT_SIZE * 0.6,
        height: FONT_SIZE * LINE_HEIGHT,
    }
}

fn grid_size(width: u32, height: u32, metrics: &CellMetrics) -> (u16, u16) {
    let cols = (width as f32 / metrics.width).max(1.0) as u16;
    let rows = (height as f32 / metrics.height).max(1.0) as u16;
    (cols, rows)
}

fn main() -> AureaResult<()> {
    let mut window = Window::new("Glacia", WINDOW_WIDTH as i32, WINDOW_HEIGHT as i32)?;

    let metrics = cell_metrics();
    let (cols, rows) = grid_size(WINDOW_WIDTH, WINDOW_HEIGHT, &metrics);
    let mut term = TerminalSession::spawn(cols, rows)
        .unwrap_or_else(|err| panic!("failed to spawn default shell: {err}"));

    let raw_canvas = Canvas::new(WINDOW_WIDTH, WINDOW_HEIGHT, RendererBackend::Cpu)?;
    // A registered draw callback is required even though the loop below
    // redraws manually via `canvas.draw()`: without one, OS-initiated
    // repaints (e.g. the window's first paint) have nothing to call back
    // into and the surface stays blank.
    raw_canvas.set_draw_callback(|ctx| ctx.clear(theme::BACKGROUND))?;
    let canvas_arc = Arc::new(Mutex::new(SendableCanvas(raw_canvas)));
    window.set_content(SharedCanvas(canvas_arc.clone()))?;

    let font = Font::new(FONT_FAMILY, FONT_SIZE);
    let mut cursor_visible = true;
    let mut last_blink = Instant::now();
    let mut needs_redraw = true;

    loop {
        unsafe { ng_platform_poll_events() };

        let events = window.poll_events();
        let mut should_close = false;
        for event in &events {
            match event {
                WindowEvent::CloseRequested => should_close = true,
                WindowEvent::Resized { width, height } => {
                    let (cols, rows) = grid_size(*width, *height, &metrics);
                    let _ = term.resize(cols, rows);
                    needs_redraw = true;
                }
                WindowEvent::KeyInput {
                    key,
                    pressed: true,
                    modifiers,
                } => {
                    if let Some(bytes) = input::terminal_key_bytes(*key, *modifiers) {
                        let _ = term.write_str(bytes);
                        cursor_visible = true;
                        last_blink = Instant::now();
                        needs_redraw = true;
                    }
                }
                WindowEvent::TextInput { text } => {
                    let printable: String = text.chars().filter(|c| !c.is_control()).collect();
                    if !printable.is_empty() {
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
        } else if last_blink.elapsed() >= CURSOR_BLINK_INTERVAL {
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
                render_cpu::draw_grid(
                    ctx,
                    term.cells(),
                    term.cursor(),
                    cursor_visible,
                    &metrics,
                    &font,
                )
            })?;
            needs_redraw = false;
        }

        window.process_frames()?;
        sleep(POLL_INTERVAL);
    }

    Ok(())
}
