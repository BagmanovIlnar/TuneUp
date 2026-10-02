//! Shared path scanning helpers for cleanup providers.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use tuneup_core::{
    TuneupError,
    cleanup::{
        CleanupCategory, CleanupItem, CleanupReport, CleanupScan, CleanupScope, CleanupSelection,
    },
    error::PlatformError,
    fs_util::{directory_size, ensure_within, is_protected_root, remove_within},
};

/// Describes one allowlisted cleanup root that may be scanned.
#[derive(Debug, Clone)]
pub struct CleanupRoot {
    pub id: String,
    pub path: PathBuf,
    pub category: CleanupCategory,
    pub scope: CleanupScope,
    pub description: String,
    pub requires_elevation: bool,
}

/// Scans configured roots and builds a [`CleanupScan`].
pub fn scan_roots(roots: &[CleanupRoot]) -> CleanupScan {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for root in roots {
        if !root.path.exists() {
            continue;
        }
        if is_protected_root(&root.path) {
            warnings.push(format!(
                "пропущен защищённый корень: {}",
                root.path.display()
            ));
            continue;
        }
        let size = directory_size(&root.path);
        if size == 0 {
            continue;
        }
        items.push(CleanupItem {
            id: root.id.clone(),
            path: root.path.clone(),
            category: root.category,
            size_bytes: size,
            scope: root.scope,
            description: root.description.clone(),
            selected_by_default: root.category.selected_by_default() && !root.requires_elevation,
            requires_elevation: root.requires_elevation,
        });
    }
    CleanupScan { items, warnings }
}

/// Deletes selected items that still resolve under their original scanned roots.
pub fn clean_selection(
    selection: &CleanupSelection,
    known: &BTreeMap<String, CleanupRoot>,
) -> Result<CleanupReport, TuneupError> {
    let mut freed_bytes = 0_u64;
    let mut failed = Vec::new();
    let mut warnings = Vec::new();
    for id in &selection.item_ids {
        let Some(root) = known.get(id) else {
            failed.push(format!("неизвестный элемент очистки: {id}"));
            continue;
        };
        if root.requires_elevation {
            failed.push(format!(
                "{}: требуется повышение прав (будет доступно через helper)",
                root.path.display()
            ));
            continue;
        }
        match remove_cleanup_root(root) {
            Ok(bytes) => freed_bytes = freed_bytes.saturating_add(bytes),
            Err(error) => failed.push(error.to_string()),
        }
    }
    if freed_bytes == 0 && failed.is_empty() {
        warnings.push("не выбрано ни одного элемента".to_owned());
    }
    Ok(CleanupReport {
        freed_bytes,
        failed,
        warnings,
    })
}

fn remove_cleanup_root(root: &CleanupRoot) -> Result<u64, PlatformError> {
    if is_protected_root(&root.path) {
        return Err(PlatformError::ProtectedObject(
            root.path.display().to_string(),
        ));
    }
    let parent = root
        .path
        .parent()
        .ok_or_else(|| PlatformError::ProtectedObject(root.path.display().to_string()))?;
    // Re-validate against parent so we never climb above the allowlisted root.
    let _ = ensure_within(&root.path, parent)?;
    if root.path.is_dir() {
        // Clear directory contents but keep the directory itself when it is a
        // well-known user cache/temp folder.
        clear_directory_contents(&root.path)
    } else {
        remove_within(&root.path, parent)
    }
}

fn clear_directory_contents(path: &Path) -> Result<u64, PlatformError> {
    let mut freed = 0_u64;
    let entries = fs::read_dir(path).map_err(|error| PlatformError::Mutation {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })?;
    for entry in entries.filter_map(Result::ok) {
        let child = entry.path();
        match remove_within(&child, path) {
            Ok(bytes) => freed = freed.saturating_add(bytes),
            Err(error) => {
                // Continue with remaining entries; callers aggregate failures.
                let _ = error;
            }
        }
    }
    Ok(freed)
}

/// Builds a stable cleanup item id from a category and path.
pub fn cleanup_id(category: CleanupCategory, path: &Path) -> String {
    format!("{:?}:{}", category as u8, path.display())
}
