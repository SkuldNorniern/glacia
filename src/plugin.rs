//! Oxygen script plugin discovery and manifest parsing.
// Types and functions are public API for the future oxygen scripting engine;
// until that engine is wired in they will be flagged as unused.
#![allow(dead_code)]
//!
//!
//! Each plugin lives in its own subdirectory under the platform plugins dir
//! (see [`crate::platform::plugins_dir`]) and must contain a `plugin.toml`
//! manifest file. Other contents are up to the plugin author.
//!
//! **Current scope**: discovery and manifest loading only. Execution is out
//! of scope until the oxygen scripting engine is wired in.

use std::fs;
use std::path::{Path, PathBuf};

use toml::Table;

/// Parsed `plugin.toml` manifest.
#[derive(Debug, Clone)]
pub struct PluginManifest {
    /// Human-readable plugin name (e.g. `"My AI Bridge"`).
    pub name: String,
    /// Semver string declared by the plugin author (e.g. `"0.1.0"`).
    pub version: String,
    /// Optional one-line description shown in future plugin manager UI.
    pub description: String,
    /// Optional theme overrides the plugin can declare.
    pub theme: Option<PluginTheme>,
}

/// Partial theme overrides a plugin may declare. Only the fields that are
/// present override the active theme; missing fields inherit from it.
#[derive(Debug, Clone, Default)]
pub struct PluginTheme {
    /// Override for the terminal background colour (hex `"#rrggbb"`).
    pub background: Option<String>,
    /// Override for the default foreground colour.
    pub foreground: Option<String>,
    /// Override for the cursor colour.
    pub cursor: Option<String>,
}

/// A loaded plugin: its file-system root and its parsed manifest.
#[derive(Debug, Clone)]
pub struct Plugin {
    /// Absolute path to the plugin's directory.
    pub root: PathBuf,
    pub manifest: PluginManifest,
}

fn parse_manifest(raw: &str) -> Option<PluginManifest> {
    let table: Table = raw.parse().ok()?;
    let name = table.get("name")?.as_str()?.to_owned();
    let version = table.get("version")?.as_str()?.to_owned();
    let description = table
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let theme = table.get("theme").and_then(|v| v.as_table()).map(|t| {
        let str_field = |key: &str| -> Option<String> { t.get(key)?.as_str().map(str::to_owned) };
        PluginTheme {
            background: str_field("background"),
            foreground: str_field("foreground"),
            cursor: str_field("cursor"),
        }
    });

    Some(PluginManifest {
        name,
        version,
        description,
        theme,
    })
}

/// Scan `plugins_dir` for valid plugins and return those whose `plugin.toml`
/// parses successfully. Directories without a manifest or with a malformed one
/// are silently skipped — the terminal must never fail to start because a
/// plugin is broken.
pub fn load_plugins(plugins_dir: &Path) -> Vec<Plugin> {
    let Ok(entries) = fs::read_dir(plugins_dir) else {
        return Vec::new();
    };

    entries
        .filter_map(|entry| {
            let dir = entry.ok()?.path();
            if !dir.is_dir() {
                return None;
            }
            let raw = fs::read_to_string(dir.join("plugin.toml")).ok()?;
            let manifest = parse_manifest(&raw)?;
            Some(Plugin {
                root: dir,
                manifest,
            })
        })
        .collect()
}
