//! Cross-platform cleanup data models without platform-specific logic.
//!
//! Providers in `tuneup-windows`, `tuneup-macos`, and `tuneup-linux` scan for
//! candidates and fill these types. The GUI only selects identifiers and never
//! invents paths on its own.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// High-level cleanup bucket shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CleanupCategory {
    /// User temporary directories such as `%TEMP%` or `/tmp` owned by the user.
    UserTemp,
    /// Application cache directories under the user profile.
    UserCache,
    /// Safe user-scoped log files.
    UserLogs,
    /// Recycle Bin / Trash contents owned by the current user.
    Trash,
    /// System temporary directories that require elevation.
    SystemTemp,
    /// System cache directories that require elevation.
    SystemCache,
}

impl CleanupCategory {
    /// Human-readable Russian label for the category.
    ///
    /// # Examples
    ///
    /// ```
    /// use tuneup_core::cleanup::CleanupCategory;
    /// assert_eq!(CleanupCategory::UserTemp.label(), "Временные файлы");
    /// ```
    pub const fn label(self) -> &'static str {
        match self {
            Self::UserTemp => "Временные файлы",
            Self::UserCache => "Кэш приложений",
            Self::UserLogs => "Журналы",
            Self::Trash => "Корзина",
            Self::SystemTemp => "Системные временные файлы",
            Self::SystemCache => "Системный кэш",
        }
    }

    /// Whether items in this category should be selected after a scan by default.
    ///
    /// # Examples
    ///
    /// ```
    /// use tuneup_core::cleanup::CleanupCategory;
    /// assert!(CleanupCategory::UserTemp.selected_by_default());
    /// assert!(!CleanupCategory::SystemCache.selected_by_default());
    /// ```
    pub const fn selected_by_default(self) -> bool {
        matches!(
            self,
            Self::UserTemp | Self::UserCache | Self::UserLogs | Self::Trash
        )
    }
}

/// Ownership scope of a cleanup candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleanupScope {
    /// Owned by the current interactive user.
    User,
    /// Requires elevated privileges.
    System,
}

/// A single deletable cleanup candidate discovered by a platform provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupItem {
    /// Stable identifier used by the GUI selection and clean request.
    pub id: String,
    /// Absolute path that will be re-validated immediately before deletion.
    pub path: PathBuf,
    /// Category shown in the cleanup UI.
    pub category: CleanupCategory,
    /// Approximate size in bytes at scan time.
    pub size_bytes: u64,
    /// Whether the item is user-owned or system-owned.
    pub scope: CleanupScope,
    /// Short description shown next to the path.
    pub description: String,
    /// Whether the item should be checked after scan.
    pub selected_by_default: bool,
    /// Whether cleaning requires the privileged helper.
    pub requires_elevation: bool,
}

/// Result of a cleanup scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupScan {
    /// Discovered candidates.
    pub items: Vec<CleanupItem>,
    /// Non-fatal scan warnings.
    pub warnings: Vec<String>,
}

impl CleanupScan {
    /// Sum of all candidate sizes.
    ///
    /// # Examples
    ///
    /// ```
    /// use tuneup_core::cleanup::{CleanupCategory, CleanupItem, CleanupScan, CleanupScope};
    /// use std::path::PathBuf;
    ///
    /// let scan = CleanupScan {
    ///     items: vec![CleanupItem {
    ///         id: "1".into(),
    ///         path: PathBuf::from("/tmp/a"),
    ///         category: CleanupCategory::UserTemp,
    ///         size_bytes: 100,
    ///         scope: CleanupScope::User,
    ///         description: "temp".into(),
    ///         selected_by_default: true,
    ///         requires_elevation: false,
    ///     }],
    ///     warnings: Vec::new(),
    /// };
    /// assert_eq!(scan.total_bytes(), 100);
    /// ```
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|item| item.size_bytes).sum()
    }
}

/// User selection of cleanup candidates by stable identifier.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupSelection {
    /// Identifiers previously returned by [`CleanupScan`].
    pub item_ids: Vec<String>,
}

/// Outcome of a cleanup operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Bytes successfully removed.
    pub freed_bytes: u64,
    /// Paths that could not be removed.
    pub failed: Vec<String>,
    /// Non-fatal messages.
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_selection_excludes_system_cache() {
        assert!(CleanupCategory::UserCache.selected_by_default());
        assert!(!CleanupCategory::SystemCache.selected_by_default());
        assert!(!CleanupCategory::SystemTemp.selected_by_default());
    }

    #[test]
    fn totals_discovered_sizes() {
        let scan = CleanupScan {
            items: vec![
                CleanupItem {
                    id: "a".into(),
                    path: PathBuf::from("/tmp/a"),
                    category: CleanupCategory::UserTemp,
                    size_bytes: 40,
                    scope: CleanupScope::User,
                    description: "a".into(),
                    selected_by_default: true,
                    requires_elevation: false,
                },
                CleanupItem {
                    id: "b".into(),
                    path: PathBuf::from("/tmp/b"),
                    category: CleanupCategory::UserCache,
                    size_bytes: 60,
                    scope: CleanupScope::User,
                    description: "b".into(),
                    selected_by_default: true,
                    requires_elevation: false,
                },
            ],
            warnings: Vec::new(),
        };
        assert_eq!(scan.total_bytes(), 100);
    }
}
