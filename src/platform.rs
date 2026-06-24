use std::env::var_os;
use std::ffi::OsString;
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

/// Find the preferred interactive shell, trying modern shells before the OS
/// default. On Windows this avoids landing in cmd.exe when PowerShell is
/// available. On Unix `None` is returned so Vanta uses `$SHELL` / `/bin/sh`.
pub fn preferred_shell() -> Option<String> {
    #[cfg(windows)]
    {
        for name in &["pwsh", "powershell"] {
            if which_exe(name) {
                return Some(name.to_string());
            }
        }
    }
    None
}

#[cfg(windows)]
fn which_exe(name: &str) -> bool {
    use std::env;
    let Ok(path_var) = env::var("PATH") else {
        return false;
    };
    for dir in env::split_paths(&path_var) {
        if dir.join(format!("{name}.exe")).exists() {
            return true;
        }
    }
    false
}

/// Extra args passed to the shell on Unix so it starts as a login shell,
/// sourcing `/etc/profile`, `~/.zprofile`, `~/.bash_profile`, etc.
/// Without this, GUI-launched apps inherit a bare system PATH that omits
/// Homebrew, pyenv, cargo, and any other user-installed tool directories.
pub fn shell_args() -> Vec<OsString> {
    #[cfg(unix)]
    {
        vec![OsString::from("-l")]
    }
    #[cfg(not(unix))]
    {
        vec![]
    }
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
