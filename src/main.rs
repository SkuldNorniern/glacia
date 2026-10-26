// Release builds run as a GUI app — without this, Windows allocates a console
// window alongside the terminal's own window. Kept active in debug builds so
// `cargo run` still shows panics/stderr in the launching console.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod input;
mod links;
mod mouse;
mod platform;
mod plugin;
mod render;
mod search;
mod selection;
mod tabs;
mod terminal;
mod theme;
mod unicode;

use std::sync::{Arc, OnceLock};

use aurea::render::{Canvas, Font, FontMetrics, RendererBackend, font_metrics, measure_text};
use aurea::window::WindowProxy;
use aurea::{AureaResult, Window, WindowEvent};

use app::{App, DEFAULT_TITLE, grid_size};
use config::Config;
use render::{CellMetrics, RowFonts};
use terminal::{SpawnOverrides, TerminalSession};

/// Cell sizes from the font itself (aurea `font_metrics`, no canvas
/// needed). A row is the face's ascent, descent and line gap, the way
/// alacritty and kitty size it, times `line_height`; the room it adds goes
/// half above, half below. Cells are as wide as `M`.
fn cell_metrics(config: &Config, fonts: &RowFonts) -> CellMetrics {
    let size = config.font.size;
    let face = font_metrics(&fonts.primary).unwrap_or(FontMetrics {
        ascent: size * 0.8,
        descent: size * 0.2,
        underline_offset: size / 10.0,
        underline_thickness: (size / 14.0).max(1.0),
        ..FontMetrics::default()
    });
    let natural = face.ascent + face.descent + face.line_gap;
    let height = (natural * config.font.line_height).round().max(1.0);
    let baseline_offset = ((height - face.ascent - face.descent) / 2.0 + face.ascent)
        .round()
        .min(height - 1.0)
        .max(1.0);
    let width = measure_text("M", &fonts.primary)
        .map(|m| m.advance)
        .ok()
        .filter(|w| *w > 0.0)
        .unwrap_or(size * 0.6);
    CellMetrics {
        width,
        height,
        baseline_offset,
        underline_offset: face.underline_offset.max(1.0),
        underline_thickness: face.underline_thickness,
        strike_offset: face.x_height.unwrap_or(face.ascent * 0.7) / 2.0,
    }
}

/// A canvas on `backend`, set as the window's content.
fn attach_canvas(
    window: &mut Window,
    config: &Config,
    backend: RendererBackend,
) -> AureaResult<Canvas> {
    let canvas = Canvas::new(config.window.width, config.window.height, backend)?;
    // The loop redraws with `canvas.draw()`, but OS repaints such as the
    // first paint need a callback too or the surface stays blank. Both clear
    // to the background first, so no white frame shows.
    canvas.set_background_color(theme::BACKGROUND);
    canvas.set_draw_callback(|ctx| ctx.clear(theme::BACKGROUND))?;
    window.set_content(canvas.clone())?;
    Ok(canvas)
}

/// The GPU canvas when the config asks for it and a GPU opens, else the CPU
/// one. A GPU is only opened on the first draw, so that draw is the test.
fn open_canvas(window: &mut Window, config: &Config) -> AureaResult<Canvas> {
    if config.render.gpu
        && let Ok(mut canvas) = attach_canvas(window, config, RendererBackend::ZenGpu)
        && canvas.draw(|ctx| ctx.clear(theme::BACKGROUND)).is_ok()
    {
        return Ok(canvas);
    }
    attach_canvas(window, config, RendererBackend::Cpu)
}

/// The configured font, then the configured fallbacks, then the platform's.
fn row_fonts(config: &Config) -> RowFonts {
    let mut families = config.font.fallbacks.clone();
    families.extend(
        platform::default_fallback_fonts()
            .iter()
            .map(|family| (*family).to_owned()),
    );
    families.dedup();
    families.retain(|family| family != &config.font.family);
    let fallbacks = families
        .iter()
        .map(|family| Font::new(family, config.font.size))
        .collect();
    RowFonts::new(Font::new(&config.font.family, config.font.size), fallbacks)
}

/// Shows `message` until the window closes, for when there is no shell to
/// show (it failed to start).
fn wait_for_close_with_message(
    window: &Window,
    canvas: &Canvas,
    font: &Font,
    message: &str,
) -> AureaResult<()> {
    canvas
        .clone()
        .draw(|ctx| render::draw_fatal_message(ctx, message, font))?;
    while !window
        .wait_events(None)
        .iter()
        .any(|event| matches!(event, WindowEvent::CloseRequested))
    {}
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
    let shell = if config.terminal.shell.is_empty() {
        platform::preferred_shell().unwrap_or_default()
    } else {
        config.terminal.shell.clone()
    };
    let padding = config.window.padding;
    // measured before the shell starts, so it starts at its real size
    let fonts = row_fonts(&config);
    let metrics = cell_metrics(&config, &fonts);
    let (cols, rows) = grid_size(config.window.width, config.window.height, &metrics, padding);
    // On macOS, fork() after GUI/canvas/font runtime initialization can leave
    // the child in a bad state. Spawn the PTY first, then resize it after exact
    // font metrics are known.
    // The window doesn't exist yet when the shell starts; its proxy lands
    // here once it does, and output wakes the event loop through it.
    let proxy: Arc<OnceLock<WindowProxy>> = Arc::default();
    // every tab's shell starts here: the first now, more from the app
    let spawn: tabs::Spawner = {
        let proxy = Arc::clone(&proxy);
        let start_dir = config.terminal.working_directory.clone();
        Box::new(move |cols, rows, dir: Option<&str>| {
            let proxy = Arc::clone(&proxy);
            let wake = move || {
                if let Some(proxy) = proxy.get() {
                    proxy.wake();
                }
            };
            TerminalSession::spawn(
                SpawnOverrides {
                    cols,
                    rows,
                    shell: &shell,
                    args: platform::shell_args(&shell),
                    env: platform::terminal_env(),
                    working_directory: dir.unwrap_or(&start_dir),
                },
                wake,
            )
        })
    };
    let term = spawn(cols, rows, None);

    let mut window = Window::new(
        DEFAULT_TITLE,
        config.window.width as i32,
        config.window.height as i32,
    )?;
    let _ = proxy.set(window.proxy());
    let mut canvas = open_canvas(&mut window, &config)?;

    let term = match term {
        Ok(term) => term,
        Err(err) => {
            let message = format!("failed to spawn default shell: {err}");
            return wait_for_close_with_message(&window, &canvas, &fonts.primary, &message);
        }
    };
    term.set_palette(theme::Colors::theme().palette());

    let wake: app::Wake = Arc::new(move || {
        if let Some(proxy) = proxy.get() {
            proxy.wake();
        }
    });
    App::new(config, term, spawn, wake, metrics, fonts, diagnostics).run(&window, &mut canvas)
}
