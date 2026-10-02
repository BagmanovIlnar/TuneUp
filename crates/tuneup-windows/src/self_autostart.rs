//! Registers TuneUp itself to start at user login on Windows.

use std::path::{Path, PathBuf};

use tuneup_core::{TuneupError, error::PlatformError};
use tuneup_platform::SelfAutostartProvider;

const VALUE_NAME: &str = "TuneUp";
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// HKCU Run-key provider for TuneUp login autostart.
pub struct WindowsSelfAutostart;

impl WindowsSelfAutostart {
    /// Creates a provider that manages the current-user Run registry value.
    pub const fn new() -> Self {
        Self
    }
}

impl Default for WindowsSelfAutostart {
    fn default() -> Self {
        Self::new()
    }
}

impl SelfAutostartProvider for WindowsSelfAutostart {
    fn is_enabled(&self) -> Result<bool, TuneupError> {
        imp::is_enabled()
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), TuneupError> {
        if enabled {
            imp::enable(&current_exe()?)
        } else {
            imp::disable()
        }
    }
}

fn current_exe() -> Result<PathBuf, TuneupError> {
    std::env::current_exe().map_err(|error| {
        PlatformError::AccessDenied(format!("не удалось определить путь TuneUp: {error}")).into()
    })
}

#[cfg(windows)]
mod imp {
    use super::*;
    use winreg::{
        RegKey,
        enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, KEY_WRITE},
    };

    pub fn is_enabled() -> Result<bool, TuneupError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu.open_subkey_with_flags(RUN_SUBKEY, KEY_READ) {
            Ok(key) => key,
            Err(_) => return Ok(false),
        };
        Ok(key.get_value::<String, _>(VALUE_NAME).is_ok())
    }

    pub fn enable(executable: &Path) -> Result<(), TuneupError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey_with_flags(RUN_SUBKEY, KEY_READ | KEY_WRITE | KEY_SET_VALUE)
            .map_err(|error| PlatformError::AccessDenied(error.to_string()))?;
        let command = quoted_command(executable);
        key.set_value(VALUE_NAME, &command)
            .map_err(|error| PlatformError::Mutation {
                path: PathBuf::from(RUN_SUBKEY),
                detail: error.to_string(),
            })?;
        Ok(())
    }

    pub fn disable() -> Result<(), TuneupError> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key =
            match hkcu.open_subkey_with_flags(RUN_SUBKEY, KEY_READ | KEY_WRITE | KEY_SET_VALUE) {
                Ok(key) => key,
                Err(_) => return Ok(()),
            };
        match key.delete_value(VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PlatformError::Mutation {
                path: PathBuf::from(RUN_SUBKEY),
                detail: error.to_string(),
            }
            .into()),
        }
    }

    fn quoted_command(executable: &Path) -> String {
        let path = executable.to_string_lossy();
        if path.contains(' ') {
            format!("\"{path}\"")
        } else {
            path.into_owned()
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn is_enabled() -> Result<bool, TuneupError> {
        Err(PlatformError::Unsupported("Run-ключ доступен только в Windows".into()).into())
    }

    pub fn enable(_executable: &Path) -> Result<(), TuneupError> {
        Err(PlatformError::Unsupported("Run-ключ доступен только в Windows".into()).into())
    }

    pub fn disable() -> Result<(), TuneupError> {
        Err(PlatformError::Unsupported("Run-ключ доступен только в Windows".into()).into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::imp::quoted_command;
    use std::path::Path;

    #[test]
    fn quotes_paths_with_spaces() {
        assert_eq!(
            quoted_command(Path::new(r"C:\Program Files\TuneUp\tuneup.exe")),
            r#""C:\Program Files\TuneUp\tuneup.exe""#
        );
    }
}
