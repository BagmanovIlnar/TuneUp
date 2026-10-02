//! Registers TuneUp itself to start at user login on macOS.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use tuneup_core::{TuneupError, error::PlatformError};
use tuneup_platform::SelfAutostartProvider;

const LABEL: &str = "com.tuneup.app";

/// User LaunchAgent based login autostart for TuneUp.
pub struct MacOsSelfAutostart;

impl MacOsSelfAutostart {
    /// Creates a provider that manages `~/Library/LaunchAgents/com.tuneup.app.plist`.
    pub const fn new() -> Self {
        Self
    }
}

impl Default for MacOsSelfAutostart {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfAutostartProvider for MacOsSelfAutostart {
    fn is_enabled(&self) -> Result<bool, TuneupError> {
        Ok(plist_path()?.exists())
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), TuneupError> {
        let path = plist_path()?;
        if enabled {
            let exe = current_exe()?;
            let agents = path
                .parent()
                .ok_or_else(|| PlatformError::AccessDenied("нет каталога LaunchAgents".into()))?;
            fs::create_dir_all(agents).map_err(|error| PlatformError::Mutation {
                path: agents.to_path_buf(),
                detail: error.to_string(),
            })?;
            let contents = launch_agent_plist(&exe);
            fs::write(&path, contents).map_err(|error| PlatformError::Mutation {
                path: path.clone(),
                detail: error.to_string(),
            })?;
            let _ = bootstrap_user_agent(&path);
            Ok(())
        } else {
            if path.exists() {
                let _ = bootout_user_agent(&path);
                fs::remove_file(&path).map_err(|error| PlatformError::Mutation {
                    path,
                    detail: error.to_string(),
                })?;
            }
            Ok(())
        }
    }
}

fn plist_path() -> Result<PathBuf, TuneupError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| PlatformError::AccessDenied("HOME не задан".into()))?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn current_exe() -> Result<PathBuf, TuneupError> {
    std::env::current_exe().map_err(|error| {
        PlatformError::AccessDenied(format!("не удалось определить путь TuneUp: {error}")).into()
    })
}

fn launch_agent_plist(executable: &Path) -> String {
    let program = xml_escape(&executable.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{program}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn launch_domain() -> String {
    #[cfg(target_os = "macos")]
    {
        format!("gui/{}", unsafe { libc::getuid() })
    }
    #[cfg(not(target_os = "macos"))]
    {
        "gui/501".to_owned()
    }
}

fn bootstrap_user_agent(plist: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/bin/launchctl")
            .args(["bootstrap", &launch_domain()])
            .arg(plist)
            .output()
            .map_err(|error| PlatformError::Mutation {
                path: plist.to_path_buf(),
                detail: error.to_string(),
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(PlatformError::Partial(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = plist;
        Err(PlatformError::Unsupported(
            "LaunchAgent доступен только в macOS".into(),
        ))
    }
}

fn bootout_user_agent(plist: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/bin/launchctl")
            .args(["bootout", &launch_domain()])
            .arg(plist)
            .output()
            .map_err(|error| PlatformError::Mutation {
                path: plist.to_path_buf(),
                detail: error.to_string(),
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(PlatformError::Partial(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = plist;
        Err(PlatformError::Unsupported(
            "LaunchAgent доступен только в macOS".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_label_and_escaped_path() {
        let plist = launch_agent_plist(Path::new(
            "/Applications/Tune & Up.app/Contents/MacOS/tuneup",
        ));
        assert!(plist.contains(LABEL));
        assert!(plist.contains("Tune &amp; Up.app"));
        assert!(plist.contains("<true/>"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn enable_disable_round_trip() {
        let provider = MacOsSelfAutostart::new();
        let previous = provider.is_enabled().unwrap_or(false);
        provider.set_enabled(true).expect("enable");
        assert!(provider.is_enabled().unwrap());
        assert!(plist_path().unwrap().exists());
        provider.set_enabled(false).expect("disable");
        assert!(!provider.is_enabled().unwrap());
        if previous {
            let _ = provider.set_enabled(true);
        }
    }
}
