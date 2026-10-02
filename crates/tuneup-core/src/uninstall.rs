//! Cross-platform uninstall and leftover data models.
//!
//! Platform providers discover installed applications and related leftover
//! paths. Profiles and settings are never selected by default; only caches and
//! temporary leftovers are.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::InstallSource;

/// How an application can be removed on the current OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UninstallKind {
    /// Windows UninstallString / QuietUninstallString executable.
    WindowsCommand,
    /// Windows MSI product code via `msiexec`.
    WindowsMsi,
    /// Windows Store / AppX package.
    WindowsStore,
    /// macOS `.app` bundle removal.
    MacOsBundle,
    /// Homebrew Cask uninstall.
    MacOsHomebrewCask,
    /// Debian/Ubuntu package via apt/dpkg.
    LinuxDeb,
    /// RPM package via dnf/rpm.
    LinuxRpm,
    /// Flatpak application.
    LinuxFlatpak,
    /// Snap package.
    LinuxSnap,
}

impl UninstallKind {
    /// Human-readable Russian label.
    ///
    /// # Examples
    ///
    /// ```
    /// use tuneup_core::uninstall::UninstallKind;
    /// assert_eq!(UninstallKind::MacOsBundle.label(), "macOS приложение");
    /// ```
    pub const fn label(self) -> &'static str {
        match self {
            Self::WindowsCommand => "Windows Uninstall",
            Self::WindowsMsi => "Windows MSI",
            Self::WindowsStore => "Microsoft Store",
            Self::MacOsBundle => "macOS приложение",
            Self::MacOsHomebrewCask => "Homebrew Cask",
            Self::LinuxDeb => "DEB/APT",
            Self::LinuxRpm => "RPM/DNF",
            Self::LinuxFlatpak => "Flatpak",
            Self::LinuxSnap => "Snap",
        }
    }
}

/// Installed application discovered by a platform inventory provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledApplication {
    /// Stable identifier unique within the current OS inventory.
    pub id: String,
    /// Display name shown in the uninstall list.
    pub name: String,
    /// Optional version string.
    pub version: Option<String>,
    /// Optional publisher / vendor.
    pub publisher: Option<String>,
    /// Package / install source used for grouping filters.
    pub source: InstallSource,
    /// Exact install root when known; never a shared OS root like `/usr`.
    pub install_root: Option<PathBuf>,
    /// Approximate installed size when available.
    pub estimated_size: Option<u64>,
    /// Mechanism used to remove the application.
    pub uninstall_kind: UninstallKind,
    /// Opaque platform identifier (product code, bundle id, package name).
    pub uninstall_identifier: String,
    /// Whether the install is user-scoped rather than system-wide.
    pub user_scope: bool,
    /// Whether TuneUp may offer removal.
    pub removable: bool,
    /// Whether the application is a protected system component.
    pub protected: bool,
}

/// Kind of leftover discovered after or before uninstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeftoverKind {
    /// Application cache directory.
    Cache,
    /// Temporary files tied to the application.
    Temp,
    /// Logs related to the application.
    Logs,
    /// User preferences / settings.
    Preferences,
    /// Full user profile / application support data.
    Profile,
    /// Other residual files under an allowlisted root.
    Other,
}

impl LeftoverKind {
    /// Whether leftovers of this kind are selected after discovery by default.
    ///
    /// Profiles and preferences require an explicit user opt-in.
    ///
    /// # Examples
    ///
    /// ```
    /// use tuneup_core::uninstall::LeftoverKind;
    /// assert!(LeftoverKind::Cache.selected_by_default());
    /// assert!(!LeftoverKind::Profile.selected_by_default());
    /// assert!(!LeftoverKind::Preferences.selected_by_default());
    /// ```
    pub const fn selected_by_default(self) -> bool {
        matches!(self, Self::Cache | Self::Temp | Self::Logs)
    }

    /// Human-readable Russian label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cache => "Кэш",
            Self::Temp => "Временные файлы",
            Self::Logs => "Журналы",
            Self::Preferences => "Настройки",
            Self::Profile => "Профиль",
            Self::Other => "Прочее",
        }
    }

    /// Whether removing this leftover may delete personal data.
    pub const fn is_user_data(self) -> bool {
        matches!(self, Self::Preferences | Self::Profile)
    }
}

/// Residual path associated with an installed application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeftoverItem {
    /// Stable identifier used by selection and deletion requests.
    pub id: String,
    /// Absolute path re-validated before deletion.
    pub path: PathBuf,
    /// Classification of the leftover.
    pub kind: LeftoverKind,
    /// Approximate size in bytes.
    pub size_bytes: u64,
    /// Short description.
    pub description: String,
    /// Whether the leftover is checked by default.
    pub selected_by_default: bool,
}

/// Request to uninstall an application and optionally remove leftovers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallRequest {
    /// Application identifier from [`InstalledApplication::id`].
    pub application_id: String,
    /// Leftover identifiers selected by the user.
    pub leftover_ids: Vec<String>,
    /// Explicit confirmation that user profiles/preferences may be deleted.
    pub include_user_data: bool,
}

/// Outcome of an uninstall operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UninstallReport {
    /// Whether the primary uninstall command completed successfully.
    pub uninstalled: bool,
    /// Bytes freed by leftover cleanup.
    pub leftovers_freed_bytes: u64,
    /// Remaining leftover paths that could not be deleted.
    pub remaining_leftovers: Vec<String>,
    /// Non-fatal messages.
    pub warnings: Vec<String>,
    /// Fatal or blocking errors.
    pub errors: Vec<String>,
}

/// Selection of leftover identifiers for a cleanup-after-uninstall pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeftoverSelection {
    /// Application these leftovers belong to.
    pub application_id: String,
    /// Leftover identifiers previously returned by a leftovers scan.
    pub leftover_ids: Vec<String>,
    /// Explicit confirmation for profile/preferences deletion.
    pub include_user_data: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftover_defaults_protect_profiles() {
        assert!(LeftoverKind::Cache.selected_by_default());
        assert!(LeftoverKind::Temp.selected_by_default());
        assert!(!LeftoverKind::Profile.selected_by_default());
        assert!(!LeftoverKind::Preferences.selected_by_default());
        assert!(LeftoverKind::Profile.is_user_data());
    }
}
