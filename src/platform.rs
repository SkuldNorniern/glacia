use std::env::var_os;
use std::path::PathBuf;

/// Platform user config path.
/// - Windows: `%APPDATA%\glacia\config.toml`
/// - Unix: `$XDG_CONFIG_HOME/glacia/config.toml` or `~/.config/glacia/config.toml`
pub fn user_config_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        var_os("APPDATA").map(PathBuf::from)
    } else {
        var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("glacia").join("config.toml"))
}

/// Platform plugins directory.
/// - Windows: `%APPDATA%\glacia\plugins\`
/// - Unix: `$XDG_CONFIG_HOME/glacia/plugins/` or `~/.config/glacia/plugins/`
pub fn plugins_dir() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        var_os("APPDATA").map(PathBuf::from)
    } else {
        var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }?;
    Some(base.join("glacia").join("plugins"))
}

/// Safe built-in primary monospace font for this platform.
///
/// Each choice ships with the OS and requires no extra installs. Users can
/// override with `font.family` in `config.toml`.
///
/// - Windows: Consolas (since Vista)
/// - macOS: Menlo (since 10.6)
/// - Linux: DejaVu Sans Mono (standard on most distributions)
pub fn default_primary_font() -> &'static str {
    if cfg!(windows) {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}
