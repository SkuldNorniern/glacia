//! User configuration: built-in defaults, optionally overridden by
//! `config.toml` at the platform config path. Invalid or unreadable config
//! never blocks startup — it falls back to defaults and reports a
//! diagnostic instead.

use std::fs::{create_dir_all, write as write_file};
use std::fs::{metadata, read_to_string};
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

use toml::Table;
use toml::Value;
use toml::de::Error as TomlError;

use crate::platform;

#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub width: u32,
    pub height: u32,
    /// Uniform padding in pixels between the window edge and the terminal grid.
    pub padding: u32,
    /// Show the tab bar with one tab too, so its new tab button is there.
    pub always_show_tabs: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            padding: 4,
            always_show_tabs: false,
        }
    }
}

/// Font sizes the config and zoom keep to.
pub const MIN_FONT_SIZE: f32 = 6.0;
pub const MAX_FONT_SIZE: f32 = 96.0;

#[derive(Debug, Clone)]
pub struct FontConfig {
    pub family: String,
    /// Font families tried, in order, when the primary face cannot render a
    /// cell's text. Empty means use the platform fallback cascade.
    pub fallbacks: Vec<String>,
    pub size: f32,
    pub line_height: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: platform::default_primary_font().to_owned(),
            fallbacks: Vec::new(),
            size: 14.0,
            line_height: 1.25,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    /// Empty means the platform default shell.
    pub shell: String,
    /// Empty means inherit the current working directory.
    pub working_directory: String,
    /// Whether apps may set the clipboard (OSC 52), as a remote vim or
    /// tmux does on yank. They can never read it.
    pub clipboard_write: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            shell: String::new(),
            working_directory: String::new(),
            clipboard_write: true,
        }
    }
}

/// Cursor rendering shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// Solid block covering the whole cell.
    #[default]
    Block,
    /// Thin vertical bar at the left edge of the cell.
    Beam,
    /// Thin horizontal bar at the bottom of the cell.
    Underline,
    /// An outline of the cell: the window doesn't have the keyboard.
    Hollow,
}

#[derive(Debug, Clone)]
pub struct CursorConfig {
    pub blink: bool,
    pub blink_interval_ms: u64,
    pub shape: CursorShape,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            blink: true,
            blink_interval_ms: 530,
            shape: CursorShape::Block,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RenderConfig {
    /// Draw on the GPU. Off by default: the GPU driver alone takes about
    /// 200 MB, against a few MB for the CPU path. Falls back to the CPU
    /// when no GPU opens.
    pub gpu: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub window: WindowConfig,
    pub render: RenderConfig,
    pub font: FontConfig,
    pub terminal: TerminalConfig,
    pub cursor: CursorConfig,
    pub bell: BellConfig,
}

#[derive(Debug, Clone)]
pub struct BellConfig {
    /// A short flash over the pane when an app rings.
    pub visual: bool,
}

impl Default for BellConfig {
    fn default() -> Self {
        Self { visual: true }
    }
}

/// How often the config file is checked for changes.
const WATCH_EVERY: Duration = Duration::from_secs(1);

/// Watches the config file on a thread of its own; each time it changes,
/// the new config comes through the channel and `wake` runs. A config that
/// doesn't parse comes with its error and should be ignored. A missing
/// file changes nothing. The thread ends at the first change after the
/// receiver is gone, or with the process.
pub fn watch(wake: impl Fn() + Send + 'static) -> Receiver<(Config, Option<String>)> {
    let (tx, rx) = channel();
    let Some(path) = platform::user_config_path() else {
        return rx;
    };
    thread::spawn(move || {
        let stamp = |p: &Path| metadata(p).and_then(|m| m.modified()).ok();
        let mut seen = stamp(&path);
        loop {
            thread::sleep(WATCH_EVERY);
            let now = stamp(&path);
            if now == seen {
                continue;
            }
            seen = now;
            // deleted or renamed: keep what is in force, don't write a new one
            if now.is_none() {
                continue;
            }
            if tx.send(Config::load()).is_err() {
                return;
            }
            wake();
        }
    });
    rx
}

fn as_u32(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_integer)
        .and_then(|i| u32::try_from(i).ok())
}

fn as_u64(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_integer)
        .and_then(|i| u64::try_from(i).ok())
}

/// A number, written with a point or without (`size = 16` counts too).
fn as_f32(v: Option<&Value>) -> Option<f32> {
    let v = v?;
    v.as_float()
        .or_else(|| v.as_integer().map(|i| i as f64))
        .map(|f| f as f32)
}

fn parse_font_fallbacks(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|family| !family.is_empty())
        .map(str::to_owned)
        .collect()
}

impl Config {
    /// Parse a TOML config string, falling back to defaults per missing or
    /// malformed field. Returns the resolved config plus the TOML syntax
    /// error if the text didn't parse as TOML at all.
    pub fn parse_str(text: &str) -> (Self, Option<TomlError>) {
        let table = match text.parse::<Table>() {
            Ok(table) => table,
            Err(error) => return (Self::default(), Some(error)),
        };

        let mut config = Self::default();

        if let Some(window) = table.get("window").and_then(Value::as_table) {
            if let Some(v) = as_u32(window.get("width")) {
                config.window.width = v.max(1);
            }
            if let Some(v) = as_u32(window.get("height")) {
                config.window.height = v.max(1);
            }
            if let Some(v) = as_u32(window.get("padding")) {
                config.window.padding = v.min(64);
            }
            if let Some(v) = window.get("always_show_tabs").and_then(Value::as_bool) {
                config.window.always_show_tabs = v;
            }
        }

        if let Some(gpu) = table
            .get("render")
            .and_then(Value::as_table)
            .and_then(|render| render.get("gpu"))
            .and_then(Value::as_bool)
        {
            config.render.gpu = gpu;
        }

        if let Some(font) = table.get("font").and_then(Value::as_table) {
            if let Some(v) = font.get("family").and_then(Value::as_str)
                && v != "auto"
                && !v.trim().is_empty()
            {
                v.clone_into(&mut config.font.family);
            }
            if let Some(v) = font.get("fallback").and_then(Value::as_str)
                && !v.trim().is_empty()
            {
                config.font.fallbacks = parse_font_fallbacks(v);
            }
            if let Some(v) = as_f32(font.get("size"))
                && v.is_finite()
            {
                config.font.size = v.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
            }
            if let Some(v) = as_f32(font.get("line_height"))
                && v.is_finite()
            {
                config.font.line_height = v.clamp(0.8, 3.0);
            }
        }

        if let Some(terminal) = table.get("terminal").and_then(Value::as_table) {
            if let Some(v) = terminal.get("shell").and_then(Value::as_str) {
                v.clone_into(&mut config.terminal.shell);
            }
            if let Some(v) = terminal.get("working_directory").and_then(Value::as_str) {
                v.clone_into(&mut config.terminal.working_directory);
            }
            if let Some(v) = terminal.get("clipboard_write").and_then(Value::as_bool) {
                config.terminal.clipboard_write = v;
            }
        }

        if let Some(v) = table
            .get("bell")
            .and_then(Value::as_table)
            .and_then(|bell| bell.get("visual"))
            .and_then(Value::as_bool)
        {
            config.bell.visual = v;
        }

        if let Some(cursor) = table.get("cursor").and_then(Value::as_table) {
            if let Some(v) = cursor.get("blink").and_then(Value::as_bool) {
                config.cursor.blink = v;
            }
            if let Some(v) = as_u64(cursor.get("blink_interval_ms")) {
                config.cursor.blink_interval_ms = v.max(1);
            }
            if let Some(v) = cursor.get("shape").and_then(Value::as_str) {
                config.cursor.shape = match v {
                    "beam" => CursorShape::Beam,
                    "underline" => CursorShape::Underline,
                    _ => CursorShape::Block,
                };
            }
        }

        (config, None)
    }

    /// Load from the platform user config path if it exists. On first launch
    /// (no file present), writes a commented template for the user to edit.
    /// Returns the resolved config plus a human-readable diagnostic if a
    /// config file existed but couldn't be read or parsed, or if the
    /// first-launch template write failed.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = platform::user_config_path() else {
            return (Self::default(), None);
        };
        if !path.exists() {
            let diagnostic = write_default_template(&path)
                .map(|e| format!("could not write default config: {e}"));
            return (Self::default(), diagnostic);
        }
        match read_to_string(&path) {
            Ok(text) => {
                let (config, error) = Self::parse_str(&text);
                let diagnostic =
                    error.map(|e| format!("could not parse config {}: {e}", path.display()));
                (config, diagnostic)
            }
            Err(error) => (
                Self::default(),
                Some(format!("could not read config {}: {error}", path.display())),
            ),
        }
    }
}

/// Write a fully-commented template `config.toml` so the user has a file to
/// edit on first launch. All entries are commented out — the parser reads
/// defaults if nothing is uncommented. Returns an error string on failure.
fn write_default_template(path: &Path) -> Option<String> {
    if let Some(parent) = path.parent() {
        if let Err(e) = create_dir_all(parent) {
            return Some(format!("could not create {}: {e}", parent.display()));
        }
    }

    let default_font = platform::default_primary_font();
    let template = format!(
        "\
# Glacia terminal emulator — configuration
# Generated on first launch. Uncomment and edit any setting you want to change.
# All commented-out values are the built-in defaults.

[window]
# width   = 1280
# height  = 800
# padding = 4
# always_show_tabs = false  # the tab bar with a single tab too

[render]
# gpu = false  # true draws on the GPU; it uses far more memory

[font]
# family      = \"{default_font}\"  # \"auto\" uses the platform default monospace
# fallback    = \"\"                # comma-separated fallback cascade; empty = platform defaults
# size        = 14.0
# line_height = 1.25

[terminal]
# shell             = \"\"  # empty = OS default (cmd.exe on Windows, $SHELL on Unix)
# working_directory = \"\"  # empty = inherit current directory
# clipboard_write   = true  # apps may set the clipboard (osc 52), never read it

[bell]
# visual = true  # a short flash when an app rings

[cursor]
# blink             = true
# blink_interval_ms = 530
# shape             = \"block\"   # block | beam | underline
"
    );

    if let Err(e) = write_file(path, template) {
        return Some(format!("could not write {}: {e}", path.display()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gpu_is_off_unless_asked_for() {
        assert!(!Config::parse_str("").0.render.gpu);
        assert!(
            Config::parse_str(
                "[render]
gpu = true"
            )
            .0
            .render
            .gpu
        );
    }

    #[test]
    fn bell_and_bounds_parse() {
        let (config, error) = Config::parse_str(
            "[bell]
visual = false
[font]
size = 1000
line_height = 0.1",
        );
        assert!(error.is_none());
        assert!(!config.bell.visual);
        assert_eq!(config.font.size, MAX_FONT_SIZE);
        assert_eq!(config.font.line_height, 0.8);
        assert!(Config::default().bell.visual);
    }
}
