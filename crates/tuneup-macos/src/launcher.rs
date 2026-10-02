//! Launches installed applications from their install root (Sleep Mode wake).

use std::{io, path::Path, process::Command};

use tuneup_core::{TuneupError, error::PlatformError};

/// Starts a macOS application via `/usr/bin/open`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacOsAppLauncher;

impl MacOsAppLauncher {
    /// Opens `install_root` (typically an `.app` bundle) with Launch Services.
    ///
    /// # Example
    ///
    /// ```ignore
    /// MacOsAppLauncher.launch(Path::new("/Applications/RustRover.app"), "RustRover")?;
    /// ```
    pub fn launch(&self, install_root: &Path, display_name: &str) -> Result<(), TuneupError> {
        #[cfg(target_os = "macos")]
        {
            let _ = display_name;
            let output = Command::new("/usr/bin/open")
                .arg(install_root)
                .output()
                .map_err(|error| {
                    TuneupError::from(PlatformError::Mutation {
                        path: install_root.to_path_buf(),
                        detail: error.to_string(),
                    })
                })?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(PlatformError::Mutation {
                path: install_root.to_path_buf(),
                detail: if stderr.trim().is_empty() {
                    io::Error::from_raw_os_error(output.status.code().unwrap_or(1)).to_string()
                } else {
                    stderr.trim().to_owned()
                },
            }
            .into())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (install_root, display_name);
            Err(PlatformError::Unsupported(
                "запуск приложений доступен только в macOS".to_owned(),
            )
            .into())
        }
    }
}

impl tuneup_platform::AppLauncher for MacOsAppLauncher {
    fn launch(&self, install_root: &Path, display_name: &str) -> Result<(), TuneupError> {
        MacOsAppLauncher::launch(self, install_root, display_name)
    }
}
