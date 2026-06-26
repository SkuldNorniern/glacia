//! User configuration: built-in defaults, optionally overridden by
//! `config.toml` at the platform config path. Invalid or unreadable config
//! never blocks startup — it falls back to defaults and reports a
//! diagnostic instead.

use std::fs::read_to_string;
use std::fs::{create_dir_all, write as write_file};
use std::path::Path;

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
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            padding: 4,
        }
    }
}

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

#[derive(Debug, Clone, Default)]
pub struct TerminalConfig {
    /// Empty means the platform default shell.
    pub shell: String,
    /// Empty means inherit the current working directory.
    pub working_directory: String,
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
pub struct Config {
    pub window: WindowConfig,
    pub font: FontConfig,
    pub terminal: TerminalConfig,
    pub cursor: CursorConfig,
}

fn as_u32(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_integer)
        .and_then(|i| u32::try_from(i).ok())
}

fn as_u64(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_integer)
        .and_then(|i| u64::try_from(i).ok())
}

fn as_f32(v: Option<&Value>) -> Option<f32> {
    v.and_then(Value::as_float).map(|f| f as f32)
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
                && v > 0.0
            {
                config.font.size = v;
            }
            if let Some(v) = as_f32(font.get("line_height"))
                && v > 0.0
            {
                config.font.line_height = v;
            }
        }

        if let Some(terminal) = table.get("terminal").and_then(Value::as_table) {
            if let Some(v) = terminal.get("shell").and_then(Value::as_str) {
                v.clone_into(&mut config.terminal.shell);
            }
            if let Some(v) = terminal.get("working_directory").and_then(Value::as_str) {
                v.clone_into(&mut config.terminal.working_directory);
            }
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

[font]
# family      = \"{default_font}\"  # \"auto\" uses the platform default monospace
# fallback    = \"\"                # comma-separated fallback cascade; empty = platform defaults
# size        = 14.0
# line_height = 1.25

[terminal]
# shell             = \"\"  # empty = OS default (cmd.exe on Windows, $SHELL on Unix)
# working_directory = \"\"  # empty = inherit current directory

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
