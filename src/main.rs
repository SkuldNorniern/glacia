use std::thread::sleep;
use std::time::Duration;

use aurea::ffi::ng_platform_poll_events;
use aurea::render::{Canvas, Color, Paint, PaintStyle, Rect, RendererBackend};
use aurea::{AureaResult, Window, WindowEvent};

const WINDOW_WIDTH: i32 = 1280;
const WINDOW_HEIGHT: i32 = 800;
const POLL_INTERVAL: Duration = Duration::from_millis(8);

fn main() -> AureaResult<()> {
    let mut window = Window::new("Glacia", WINDOW_WIDTH, WINDOW_HEIGHT)?;

    let canvas = Canvas::new(
        WINDOW_WIDTH as u32,
        WINDOW_HEIGHT as u32,
        RendererBackend::Cpu,
    )?;
    canvas.set_draw_callback(|ctx| {
        ctx.clear(Color::rgb(16, 18, 24))?;
        let paint = Paint {
            color: Color::rgb(216, 222, 233),
            style: PaintStyle::Fill,
            ..Default::default()
        };
        ctx.draw_rect(Rect::new(32.0, 32.0, 200.0, 60.0), &paint)?;
        Ok(())
    })?;
    window.set_content(canvas)?;

    loop {
        unsafe { ng_platform_poll_events() };

        let events = window.poll_events();
        let should_close = events
            .iter()
            .any(|event| matches!(event, WindowEvent::CloseRequested));
        if should_close {
            break;
        }

        window.process_frames()?;
        sleep(POLL_INTERVAL);
    }

    Ok(())
}
