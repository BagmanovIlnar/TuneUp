//! Registers TuneUp itself to start at user login on Linux.

use std::{
    fs,
    path::{Path, PathBuf},
};

use tuneup_core::{TuneupError, error::PlatformError};
use tuneup_platform::SelfAutostartProvider;

const DESKTOP_NAME: &str = "tuneup.desktop";

/// XDG autostart `.desktop` provider for TuneUp.
pub struct LinuxSelfAutostart;

impl LinuxSelfAutostart {
    /// Creates a provider that manages `~/.config/autostart/tuneup.desktop`.
    pub const fn new() -> Self {
        Self
    }
}

impl Default for LinuxSelfAutostart {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfAutostartProvider for LinuxSelfAutostart {
    fn is_enabled(&self) -> Result<bool, TuneupError> {
        let path = desktop_path()?;
        if !path.exists() {
            return Ok(false);
        }
        let contents = fs::read_to_string(&path).map_err(|error| PlatformError::Mutation {
            path,
            detail: error.to_string(),
        })?;
        Ok(!contents.lines().any(|line| {
            let trimmed = line.trim();
            trimmed.eq_ignore_ascii_case("Hidden=true")
                || trimmed.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
        }))
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), TuneupError> {
        let path = desktop_path()?;
        if enabled {
            let exe = current_exe()?;
            let parent = path
                .parent()
                .ok_or_else(|| PlatformError::AccessDenied("нет каталога autostart".into()))?;
            fs::create_dir_all(parent).map_err(|error| PlatformError::Mutation {
                path: parent.to_path_buf(),
                detail: error.to_string(),
            })?;
            let contents = desktop_entry(&exe);
            fs::write(&path, contents).map_err(|error| PlatformError::Mutation {
                path,
                detail: error.to_string(),
            })?;
            Ok(())
        } else if path.exists() {
            fs::remove_file(&path).map_err(|error| PlatformError::Mutation {
                path,
                detail: error.to_string(),
            })?;
            Ok(())
        } else {
            Ok(())
        }
    }
}

fn desktop_path() -> Result<PathBuf, TuneupError> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| PlatformError::AccessDenied("HOME/XDG_CONFIG_HOME не задан".into()))?;
    Ok(config.join("autostart").join(DESKTOP_NAME))
}

fn current_exe() -> Result<PathBuf, TuneupError> {
    std::env::current_exe().map_err(|error| {
        PlatformError::AccessDenied(format!("не удалось определить путь TuneUp: {error}")).into()
    })
}

fn desktop_entry(executable: &Path) -> String {
    let exec = escape_desktop_exec(&executable.to_string_lossy());
    format!(
        "[Desktop Entry]\n\
Type=Application\n\
Version=1.0\n\
Name=TuneUp\n\
Comment=TuneUp system utility\n\
Exec={exec}\n\
Terminal=false\n\
Categories=Utility;System;\n\
X-GNOME-Autostart-enabled=true\n\
Hidden=false\n"
    )
}

fn escape_desktop_exec(value: &str) -> String {
    if value.chars().any(|character| character.is_whitespace()) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quotes_paths_with_spaces() {
        let entry = desktop_entry(Path::new("/opt/Tune Up/tuneup"));
        assert!(entry.contains("Exec=\"/opt/Tune Up/tuneup\""));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true"));
    }
}
