//! Linux cleanup of XDG caches, temporary files and the FreeDesktop trash.

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

/// Scans and cleans disposable Linux user and system cache data.
pub struct LinuxCleanupProvider {
    last_roots: Mutex<BTreeMap<String, CleanupRoot>>,
}

impl LinuxCleanupProvider {
    /// Creates a provider with an empty scan cache.
    pub fn new() -> Self {
        Self {
            last_roots: Mutex::new(BTreeMap::new()),
        }
    }

    fn discover_roots() -> Vec<CleanupRoot> {
        let mut roots = Vec::new();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".cache")));
        if let Some(cache) = cache {
            push_root(
                &mut roots,
                &cache,
                CleanupCategory::UserCache,
                CleanupScope::User,
                "XDG cache",
                false,
            );
        }
        if let Some(home) = &home {
            push_root(
                &mut roots,
                &home.join(".local/share/Trash/files"),
                CleanupCategory::Trash,
                CleanupScope::User,
                "Корзина FreeDesktop",
                false,
            );
        }
        let tmp = std::env::temp_dir();
        if tmp.starts_with("/tmp") || tmp.starts_with("/var/tmp") {
            // Only clear a TuneUp-owned subdirectory marker if present; otherwise
            // expose /tmp contents owned by the user via a dedicated subfolder scan.
            push_root(
                &mut roots,
                &tmp.join(format!("tuneup-{}", users_name())),
                CleanupCategory::UserTemp,
                CleanupScope::User,
                "Временные файлы TuneUp",
                false,
            );
        }
        push_root(
            &mut roots,
            Path::new("/var/tmp"),
            CleanupCategory::SystemTemp,
            CleanupScope::System,
            "/var/tmp",
            true,
        );
        push_root(
            &mut roots,
            Path::new("/var/cache"),
            CleanupCategory::SystemCache,
            CleanupScope::System,
            "/var/cache",
            true,
        );
        roots
    }
}

impl Default for LinuxCleanupProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanupProvider for LinuxCleanupProvider {
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

fn users_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| std::process::id().to_string())
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
