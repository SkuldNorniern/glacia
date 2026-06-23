//! User configuration: built-in defaults, optionally overridden by
//! `config.toml` at the platform config path. Invalid or unreadable config
//! never blocks startup — it falls back to defaults and reports a
//! diagnostic instead.

use std::env::var_os;
use std::fs::read_to_string;
use std::path::PathBuf;

use toml::Table;
use toml::Value;
use toml::de::Error as TomlError;

#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub width: u32,
    pub height: u32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "Consolas".to_owned(),
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

/// Cursor shape is always a block today — `render_cpu` doesn't draw any
/// other shape yet, so there's nothing to configure until it does.
#[derive(Debug, Clone)]
pub struct CursorConfig {
    pub blink: bool,
    pub blink_interval_ms: u64,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            blink: true,
            blink_interval_ms: 530,
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
        .filter(|i| *i >= 0)
        .map(|i| i as u32)
}

fn as_u64(v: Option<&Value>) -> Option<u64> {
    v.and_then(Value::as_integer)
        .filter(|i| *i >= 0)
        .map(|i| i as u64)
}

fn as_f32(v: Option<&Value>) -> Option<f32> {
    v.and_then(Value::as_float).map(|f| f as f32)
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
        }

        if let Some(font) = table.get("font").and_then(Value::as_table) {
            if let Some(v) = font.get("family").and_then(Value::as_str)
                && v != "auto"
                && !v.trim().is_empty()
            {
                config.font.family = v.to_owned();
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
                config.terminal.shell = v.to_owned();
            }
            if let Some(v) = terminal.get("working_directory").and_then(Value::as_str) {
                config.terminal.working_directory = v.to_owned();
            }
        }

        if let Some(cursor) = table.get("cursor").and_then(Value::as_table) {
            if let Some(v) = cursor.get("blink").and_then(Value::as_bool) {
                config.cursor.blink = v;
            }
            if let Some(v) = as_u64(cursor.get("blink_interval_ms")) {
                config.cursor.blink_interval_ms = v.max(1);
            }
        }

        (config, None)
    }

    /// Load from the platform user config path if it exists, otherwise
    /// return defaults. Returns the resolved config plus a human-readable
    /// diagnostic if a config file existed but couldn't be read or parsed.
    pub fn load() -> (Self, Option<String>) {
        let Some(path) = user_config_path() else {
            return (Self::default(), None);
        };
        if !path.exists() {
            return (Self::default(), None);
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

/// `%APPDATA%\glacia\config.toml` on Windows, `$XDG_CONFIG_HOME/glacia/config.toml`
/// (falling back to `~/.config/...`) elsewhere.
fn user_config_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        var_os("APPDATA").map(PathBuf::from)
    } else {
        var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("glacia").join("config.toml"))
}
