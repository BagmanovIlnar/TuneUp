use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use tuneup_core::error::PlatformError;

const PROTECTED_LABEL_PREFIXES: &[&str] =
    &["com.apple.", "com.tuneup.", "io.tuneup.", "dev.tuneup."];

/// Returns whether a launchd label belongs to macOS or TuneUp itself.
pub fn protected_label(label: &str) -> bool {
    let label = label.to_ascii_lowercase();
    PROTECTED_LABEL_PREFIXES
        .iter()
        .any(|prefix| label.starts_with(prefix))
}

/// Returns whether a canonical path is in an operating-system-owned location.
pub fn protected_path(path: &Path) -> bool {
    [
        Path::new("/System"),
        Path::new("/bin"),
        Path::new("/sbin"),
        Path::new("/usr/bin"),
        Path::new("/usr/lib"),
        Path::new("/usr/sbin"),
        Path::new("/private"),
        Path::new("/Library/Apple"),
    ]
    .iter()
    .any(|root| path == *root || path.starts_with(root))
}

/// Canonicalizes an existing absolute path and rejects paths containing
/// parent-directory components before filesystem resolution.
pub fn canonical_existing(path: &Path) -> Result<PathBuf, PlatformError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(PlatformError::ProtectedObject(path.display().to_string()));
    }
    fs::canonicalize(path).map_err(|error| PlatformError::Mutation {
        path: path.to_path_buf(),
        detail: format!("не удалось получить canonical path: {error}"),
    })
}

/// Matches a target only when both paths exist, canonicalize successfully, and
/// the target equals the install root or is a descendant by path components.
pub fn exact_install_root_match(target: &Path, install_root: &Path) -> bool {
    let Ok(target) = canonical_existing(target) else {
        return false;
    };
    let Ok(root) = canonical_existing(install_root) else {
        return false;
    };
    target == root || target.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_apple_and_tuneup_labels() {
        assert!(protected_label("com.apple.WindowServer"));
        assert!(protected_label("io.tuneup.helper"));
        assert!(!protected_label("com.example.agent"));
    }

    #[test]
    fn path_protection_uses_components() {
        assert!(protected_path(Path::new("/System/Library/CoreServices")));
        assert!(!protected_path(Path::new("/Systematic/App")));
        assert!(!protected_path(Path::new("/Applications/Example.app")));
    }

    #[test]
    fn canonical_matching_rejects_prefix_siblings() {
        let base =
            std::env::temp_dir().join(format!("tuneup-macos-path-test-{}", std::process::id()));
        let app = base.join("Demo.app");
        let sibling = base.join("Demo.app.other");
        let binary = app.join("Contents/MacOS");
        fs::create_dir_all(&binary).expect("create app fixture");
        fs::create_dir_all(&sibling).expect("create sibling fixture");
        assert!(exact_install_root_match(&binary, &app));
        assert!(!exact_install_root_match(&sibling, &app));
        fs::remove_dir_all(base).expect("remove path fixture");
    }
}
