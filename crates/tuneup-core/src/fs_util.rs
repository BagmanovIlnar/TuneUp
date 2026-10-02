//! Shared filesystem helpers used by cleanup and uninstall providers.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::error::PlatformError;

/// Recursively estimates the size of a file or directory.
///
/// Symlinks are not followed. Unreadable entries contribute zero bytes.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use tuneup_core::fs_util::directory_size;
/// let _ = directory_size(Path::new("/tmp"));
/// ```
pub fn directory_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.file_type().is_symlink() {
        return metadata.len();
    }
    if metadata.is_file() {
        return metadata.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| directory_size(&entry.path()))
        .sum()
}

/// Returns a canonical absolute path or a typed platform error.
pub fn canonicalize_existing(path: &Path) -> Result<PathBuf, PlatformError> {
    fs::canonicalize(path).map_err(|error| PlatformError::Mutation {
        path: path.to_path_buf(),
        detail: error.to_string(),
    })
}

/// Ensures `candidate` resolves inside `root` after canonicalization.
///
/// # Examples
///
/// ```no_run
/// use std::path::Path;
/// use tuneup_core::fs_util::ensure_within;
/// let _ = ensure_within(Path::new("/tmp/a"), Path::new("/tmp"));
/// ```
pub fn ensure_within(candidate: &Path, root: &Path) -> Result<PathBuf, PlatformError> {
    let candidate = canonicalize_existing(candidate)?;
    let root = canonicalize_existing(root)?;
    if candidate == root || candidate.starts_with(&root) {
        Ok(candidate)
    } else {
        Err(PlatformError::ProtectedObject(
            candidate.display().to_string(),
        ))
    }
}

/// Removes a file or directory after validating it still exists under `root`.
pub fn remove_within(path: &Path, root: &Path) -> Result<u64, PlatformError> {
    let canonical = ensure_within(path, root)?;
    let size = directory_size(&canonical);
    let metadata = fs::symlink_metadata(&canonical).map_err(|error| PlatformError::Mutation {
        path: canonical.clone(),
        detail: error.to_string(),
    })?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(&canonical).map_err(|error| PlatformError::Mutation {
            path: canonical,
            detail: error.to_string(),
        })?;
    } else {
        fs::remove_file(&canonical).map_err(|error| PlatformError::Mutation {
            path: canonical,
            detail: error.to_string(),
        })?;
    }
    Ok(size)
}

/// Returns true when `path` matches one of the protected OS roots.
pub fn is_protected_root(path: &Path) -> bool {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let trimmed = normalized.trim_end_matches('/');
    matches!(
        trimmed,
        "" | "/"
            | "/bin"
            | "/boot"
            | "/dev"
            | "/etc"
            | "/lib"
            | "/lib64"
            | "/proc"
            | "/root"
            | "/run"
            | "/sbin"
            | "/sys"
            | "/usr"
            | "/var"
            | "/system"
            | "/library"
            | "/applications"
            | "c:"
            | "c:/windows"
            | "c:/program files"
            | "c:/program files (x86)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_roots_cover_os_bases() {
        assert!(is_protected_root(Path::new("/")));
        assert!(is_protected_root(Path::new("/usr")));
        assert!(is_protected_root(Path::new("C:\\Windows")));
        assert!(!is_protected_root(Path::new("/tmp/tuneup-cache")));
    }
}
