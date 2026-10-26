// Release builds run as a GUI app — without this, Windows allocates a console
// window alongside the terminal's own window. Kept active in debug builds so
// `cargo run` still shows panics/stderr in the launching console.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod input;
mod mouse;
mod platform;
mod plugin;
mod render;
mod search;
mod selection;
mod terminal;
mod theme;
mod unicode;

use std::sync::{Arc, OnceLock};

use aurea::render::{Canvas, Font, RendererBackend};
use aurea::window::WindowProxy;
use aurea::{AureaResult, Window, WindowEvent};

use app::{App, DEFAULT_TITLE, grid_size};
use config::Config;
use render::{CellMetrics, RowFonts};
use terminal::{SpawnOverrides, TerminalSession};

/// Cell sizes guessed from the config alone, before a font is measured.
fn estimated_metrics(config: &Config) -> CellMetrics {
    let height = config.font.size * config.font.line_height;
    CellMetrics {
        width: config.font.size * 0.6,
        height,
        baseline_offset: height * 0.8,
        ascent: height * 0.8,
    }
}

/// Measure font metrics and build the cell sizing struct.
///
/// Width comes from the actual advance of 'M' so the cursor stays aligned with
/// the rendered glyphs. Height and baseline_offset come from the font's true
/// ascent+descent rather than a fixed fraction, preventing the last row's
/// descenders from being clipped at the canvas edge. Also probes which
/// codepoints the primary font lacks so fallback routing is automatic.
fn cell_metrics(config: &Config, canvas: &Canvas, row_fonts: &mut RowFonts) -> CellMetrics {
    let mut metrics = estimated_metrics(config);
    let _ = canvas.clone().draw(|ctx| {
        if let Ok(m) = ctx.measure_text("M", &row_fonts.primary) {
            if m.ascent > 0.0 {
                let font_h = m.ascent + m.descent;
                // whole-pixel rows: every row edge and baseline lands on a
                // pixel, so lines stay sharp and evenly spaced
                let height = (font_h * config.font.line_height).round().max(1.0);
                // the line_height gap split evenly above and below the glyph
                metrics.baseline_offset = ((height - font_h) / 2.0 + m.ascent).round();
                metrics.height = height;
                metrics.ascent = m.ascent;
            }
            if m.advance > 0.0 {
                metrics.width = m.advance;
            }
        }
        row_fonts.probe(ctx, metrics.width);
        Ok(())
    });
    metrics
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
    let (cols, rows) = grid_size(
        config.window.width,
        config.window.height,
        &estimated_metrics(&config),
        padding,
    );
    // On macOS, fork() after GUI/canvas/font runtime initialization can leave
    // the child in a bad state. Spawn the PTY first, then resize it after exact
    // font metrics are known.
    // The window doesn't exist yet when the shell starts; its proxy lands
    // here once it does, and output wakes the event loop through it.
    let proxy: Arc<OnceLock<WindowProxy>> = Arc::default();
    let wake = {
        let proxy = Arc::clone(&proxy);
        move || {
            if let Some(proxy) = proxy.get() {
                proxy.wake();
            }
        }
    };
    let term = TerminalSession::spawn(
        SpawnOverrides {
            cols,
            rows,
            shell: &shell,
            args: platform::shell_args(&shell),
            env: platform::terminal_env(),
            working_directory: &config.terminal.working_directory,
        },
        wake,
    );

    let mut window = Window::new(
        DEFAULT_TITLE,
        config.window.width as i32,
        config.window.height as i32,
    )?;
    let _ = proxy.set(window.proxy());
    let mut canvas = open_canvas(&mut window, &config)?;
    let mut fonts = row_fonts(&config);
    let metrics = cell_metrics(&config, &canvas, &mut fonts);

    let mut term = match term {
        Ok(term) => term,
        Err(err) => {
            let message = format!("failed to spawn default shell: {err}");
            return wait_for_close_with_message(&window, &canvas, &fonts.primary, &message);
        }
    };
    let (cols, rows) = grid_size(config.window.width, config.window.height, &metrics, padding);
    let _ = term.resize(cols, rows);
    term.set_palette(theme::Colors::theme().palette());

    App::new(config, term, metrics, fonts, diagnostics).run(&window, &mut canvas)
}
