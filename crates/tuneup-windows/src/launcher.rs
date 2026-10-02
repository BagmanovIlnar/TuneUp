//! Launches installed applications from their install root (Sleep Mode wake).

use std::path::Path;

use tuneup_core::{TuneupError, error::PlatformError};

/// Starts a Windows application from an install root.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsAppLauncher;

impl WindowsAppLauncher {
    /// Opens `install_root` (exe or folder) with the shell.
    ///
    /// # Example
    ///
    /// ```ignore
    /// WindowsAppLauncher.launch(Path::new(r"C:\Program Files\App"), "App")?;
    /// ```
    pub fn launch(&self, install_root: &Path, display_name: &str) -> Result<(), TuneupError> {
        #[cfg(windows)]
        {
            let _ = display_name;
            platform::launch(install_root)
        }
        #[cfg(not(windows))]
        {
            let _ = (install_root, display_name);
            Err(PlatformError::Unsupported(
                "запуск приложений доступен только в Windows".to_owned(),
            )
            .into())
        }
    }
}

impl tuneup_platform::AppLauncher for WindowsAppLauncher {
    fn launch(&self, install_root: &Path, display_name: &str) -> Result<(), TuneupError> {
        WindowsAppLauncher::launch(self, install_root, display_name)
    }
}

#[cfg(windows)]
mod platform {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::Path};

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::*;

    pub fn launch(install_root: &Path) -> Result<(), TuneupError> {
        let target = resolve_target(install_root);
        let wide = to_wide(&target);
        let operation = to_wide(OsStr::new("open"));
        // SAFETY: NUL-terminated wide strings; ShellExecuteW does not take ownership.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if result as isize <= 32 {
            return Err(PlatformError::Mutation {
                path: target,
                detail: format!("ShellExecuteW failed with code {}", result as isize),
            }
            .into());
        }
        Ok(())
    }

    fn resolve_target(install_root: &Path) -> std::path::PathBuf {
        if install_root.is_file() {
            return install_root.to_path_buf();
        }
        let bin = install_root.join("bin");
        if bin.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&bin) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                    {
                        return path;
                    }
                }
            }
        }
        if let Ok(entries) = std::fs::read_dir(install_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                {
                    return path;
                }
            }
        }
        install_root.to_path_buf()
    }

    fn to_wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }
}
