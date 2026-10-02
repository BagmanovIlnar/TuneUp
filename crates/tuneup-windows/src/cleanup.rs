//! Windows cleanup of temporary files, caches and the Recycle Bin.

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

/// Scans and cleans disposable Windows user/system temporary data.
pub struct WindowsCleanupProvider {
    last_roots: Mutex<BTreeMap<String, CleanupRoot>>,
}

impl WindowsCleanupProvider {
    /// Creates a provider with an empty scan cache.
    pub fn new() -> Self {
        Self {
            last_roots: Mutex::new(BTreeMap::new()),
        }
    }

    fn discover_roots() -> Vec<CleanupRoot> {
        let mut roots = Vec::new();
        if let Some(temp) = std::env::var_os("TEMP").map(PathBuf::from) {
            push_root(
                &mut roots,
                &temp,
                CleanupCategory::UserTemp,
                CleanupScope::User,
                "Каталог TEMP пользователя",
                false,
            );
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            push_root(
                &mut roots,
                &local.join("Temp"),
                CleanupCategory::UserTemp,
                CleanupScope::User,
                "LocalAppData\\Temp",
                false,
            );
            // Well-known safe application caches only.
            for (name, relative) in [
                (
                    "Microsoft\\Windows\\INetCache",
                    "Кэш Internet Explorer/Edge",
                ),
                ("Microsoft\\Windows\\Explorer", "Кэш эскизов Explorer"),
                ("D3DSCache", "Кэш Direct3D shader"),
            ] {
                push_root(
                    &mut roots,
                    &local.join(name),
                    CleanupCategory::UserCache,
                    CleanupScope::User,
                    relative,
                    false,
                );
            }
        }
        if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
            push_root(
                &mut roots,
                &home.join("AppData\\Local\\Temp"),
                CleanupCategory::UserTemp,
                CleanupScope::User,
                "Профиль\\AppData\\Local\\Temp",
                false,
            );
        }
        // System Temp requires elevation and stays unselected by default.
        push_root(
            &mut roots,
            Path::new(r"C:\Windows\Temp"),
            CleanupCategory::SystemTemp,
            CleanupScope::System,
            "C:\\Windows\\Temp",
            true,
        );
        roots
    }
}

impl Default for WindowsCleanupProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanupProvider for WindowsCleanupProvider {
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
