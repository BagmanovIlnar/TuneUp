//! macOS cleanup of user caches, logs, temporary files and Trash.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use tuneup_core::{
    TuneupError,
    cleanup::{CleanupCategory, CleanupReport, CleanupScan, CleanupScope, CleanupSelection},
};
use tuneup_platform::{
    CleanupProvider,
    cleanup_util::{CleanupRoot, clean_selection, cleanup_id, scan_roots},
};

/// Scans and cleans disposable macOS user and system cache data.
pub struct MacOsCleanupProvider {
    last_roots: Mutex<BTreeMap<String, CleanupRoot>>,
}

impl MacOsCleanupProvider {
    /// Creates a provider with an empty scan cache.
    pub fn new() -> Self {
        Self {
            last_roots: Mutex::new(BTreeMap::new()),
        }
    }

    fn discover_roots() -> Vec<CleanupRoot> {
        let mut roots = Vec::new();
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return roots;
        };
        push_root(
            &mut roots,
            &home.join("Library/Caches"),
            CleanupCategory::UserCache,
            CleanupScope::User,
            "Library/Caches",
            false,
        );
        push_root(
            &mut roots,
            &home.join("Library/Logs"),
            CleanupCategory::UserLogs,
            CleanupScope::User,
            "Library/Logs",
            false,
        );
        push_root(
            &mut roots,
            &home.join(".Trash"),
            CleanupCategory::Trash,
            CleanupScope::User,
            "Корзина",
            false,
        );
        if let Ok(tmpdir) = std::env::temp_dir().canonicalize() {
            // Only clear content under the process temp dir when it lives in /var/folders.
            if tmpdir.starts_with("/var/folders") || tmpdir.starts_with("/private/var/folders") {
                push_root(
                    &mut roots,
                    &tmpdir,
                    CleanupCategory::UserTemp,
                    CleanupScope::User,
                    "Временный каталог пользователя",
                    false,
                );
            }
        }
        push_root(
            &mut roots,
            Path::new("/Library/Caches"),
            CleanupCategory::SystemCache,
            CleanupScope::System,
            "/Library/Caches",
            true,
        );
        roots
    }
}

impl Default for MacOsCleanupProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanupProvider for MacOsCleanupProvider {
    fn scan(&self) -> Result<CleanupScan, TuneupError> {
        let roots = Self::discover_roots();
        let scan = scan_roots(&roots);
        if let Ok(mut guard) = self.last_roots.lock() {
            *guard = roots
                .into_iter()
                .map(|root| (root.id.clone(), root))
                .collect();
        }
        Ok(scan)
    }

    fn clean(&self, selection: &CleanupSelection) -> Result<CleanupReport, TuneupError> {
        let known = self
            .last_roots
            .lock()
            .map_err(|_| TuneupError::Inventory("cleanup state poisoned".into()))?
            .clone();
        clean_selection(selection, &known)
    }
}

fn push_root(
    roots: &mut Vec<CleanupRoot>,
    path: &Path,
    category: CleanupCategory,
    scope: CleanupScope,
    description: &str,
    requires_elevation: bool,
) {
    if !path.exists() {
        return;
    }
    roots.push(CleanupRoot {
        id: cleanup_id(category, path),
        path: path.to_path_buf(),
        category,
        scope,
        description: description.to_owned(),
        requires_elevation,
    });
}
