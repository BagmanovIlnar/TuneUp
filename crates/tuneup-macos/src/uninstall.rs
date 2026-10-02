//! macOS application inventory, bundle removal and leftover discovery.
//!
//! Leftover discovery mirrors App Cleaner–style behaviour: scan known Library
//! roots and match folders/files by bundle identifier and application name.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use tuneup_core::{
    TuneupError,
    error::PlatformError,
    fs_util::{directory_size, is_protected_root, remove_within},
    model::InstallSource,
    uninstall::{
        InstalledApplication, LeftoverItem, LeftoverKind, LeftoverSelection, UninstallKind,
        UninstallReport, UninstallRequest,
    },
};
use tuneup_platform::{UninstallProvider, uninstall_util::remove_selected_leftovers};

use crate::inventory::MacOsInventoryProvider;

struct CachedApp {
    app: InstalledApplication,
}

/// Lists macOS apps and removes bundles / Homebrew casks with leftover cleanup.
pub struct MacOsUninstallProvider {
    apps: Mutex<BTreeMap<String, CachedApp>>,
    leftovers: Mutex<BTreeMap<String, Vec<LeftoverItem>>>,
}

impl MacOsUninstallProvider {
    /// Creates an empty provider; applications are loaded on demand.
    pub fn new() -> Self {
        Self {
            apps: Mutex::new(BTreeMap::new()),
            leftovers: Mutex::new(BTreeMap::new()),
        }
    }

    fn load_applications(&self) -> Result<Vec<InstalledApplication>, TuneupError> {
        let snapshot = MacOsInventoryProvider.snapshot()?;
        let mut apps = BTreeMap::new();
        let mut list = Vec::new();
        for record in snapshot.installs {
            let protected = record.install_root.starts_with("/System")
                || is_protected_root(&record.install_root);
            let user_scope = record.install_root.starts_with(
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default(),
            );
            let id = format!("mac:{}", record.id);
            let app = InstalledApplication {
                id: id.clone(),
                name: record
                    .display_name
                    .clone()
                    .unwrap_or_else(|| record.id.clone()),
                version: record.version.clone(),
                publisher: record.publisher.clone(),
                source: InstallSource::MacOsBundle,
                install_root: Some(record.install_root.clone()),
                estimated_size: Some(directory_size(&record.install_root)),
                uninstall_kind: UninstallKind::MacOsBundle,
                uninstall_identifier: record.id.clone(),
                user_scope,
                removable: !protected,
                protected,
            };
            list.push(app.clone());
            apps.insert(id, CachedApp { app });
        }
        if let Ok(output) = Command::new("brew").args(["list", "--cask"]).output()
            && output.status.success()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                let name = line.trim();
                if name.is_empty() {
                    continue;
                }
                let id = format!("brew:{name}");
                if apps.contains_key(&id) {
                    continue;
                }
                let app = InstalledApplication {
                    id: id.clone(),
                    name: name.to_owned(),
                    version: None,
                    publisher: Some("Homebrew".into()),
                    source: InstallSource::MacOsBundle,
                    install_root: None,
                    estimated_size: None,
                    uninstall_kind: UninstallKind::MacOsHomebrewCask,
                    uninstall_identifier: name.to_owned(),
                    user_scope: true,
                    removable: true,
                    protected: false,
                };
                list.push(app.clone());
                apps.insert(id, CachedApp { app });
            }
        }
        if let Ok(mut guard) = self.apps.lock() {
            *guard = apps;
        }
        list.sort_by_key(|app| app.name.to_ascii_lowercase());
        Ok(list)
    }

    fn leftovers_for(&self, application_id: &str) -> Result<Vec<LeftoverItem>, TuneupError> {
        let apps = self
            .apps
            .lock()
            .map_err(|_| TuneupError::Inventory("uninstall state poisoned".into()))?;
        let Some(cached) = apps.get(application_id) else {
            return Err(TuneupError::Inventory(format!(
                "приложение не найдено: {application_id}"
            )));
        };
        let bundle_id = cached.app.uninstall_identifier.clone();
        let name = cached.app.name.clone();
        let install_root = cached.app.install_root.clone();
        drop(apps);

        let leftovers = discover_leftovers(&bundle_id, &name, install_root.as_deref());
        if let Ok(mut guard) = self.leftovers.lock() {
            guard.insert(application_id.to_owned(), leftovers.clone());
        }
        Ok(leftovers)
    }
}

impl Default for MacOsUninstallProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UninstallProvider for MacOsUninstallProvider {
    fn installed_applications(&self) -> Result<Vec<InstalledApplication>, TuneupError> {
        self.load_applications()
    }

    fn find_leftovers(&self, application_id: &str) -> Result<Vec<LeftoverItem>, TuneupError> {
        if self
            .apps
            .lock()
            .map_err(|_| TuneupError::Inventory("uninstall state poisoned".into()))?
            .is_empty()
        {
            let _ = self.load_applications()?;
        }
        self.leftovers_for(application_id)
    }

    fn uninstall(&self, request: &UninstallRequest) -> Result<UninstallReport, TuneupError> {
        let (kind, identifier, root, name, protected) = {
            let apps = self
                .apps
                .lock()
                .map_err(|_| TuneupError::Inventory("uninstall state poisoned".into()))?;
            let Some(cached) = apps.get(&request.application_id) else {
                return Err(TuneupError::Inventory(format!(
                    "приложение не найдено: {}",
                    request.application_id
                )));
            };
            if cached.app.protected || !cached.app.removable {
                return Ok(UninstallReport {
                    uninstalled: false,
                    errors: vec![format!(
                        "«{}» защищено и не может быть удалено TuneUp",
                        cached.app.name
                    )],
                    ..UninstallReport::default()
                });
            }
            (
                cached.app.uninstall_kind,
                cached.app.uninstall_identifier.clone(),
                cached.app.install_root.clone(),
                cached.app.name.clone(),
                cached.app.protected,
            )
        };
        let _ = protected;
        let mut report = UninstallReport::default();
        match kind {
            UninstallKind::MacOsHomebrewCask => {
                if let Err(error) = run_brew_cask_uninstall(&identifier) {
                    report
                        .errors
                        .push(format!("не удалось удалить «{name}»: {error}"));
                    return Ok(report);
                }
                report.uninstalled = true;
            }
            UninstallKind::MacOsBundle => {
                let Some(root) = root else {
                    report.errors.push(format!("нет install root для «{name}»"));
                    return Ok(report);
                };
                if let Err(error) = remove_app_bundle(&root) {
                    report
                        .errors
                        .push(format!("не удалось удалить «{name}»: {error}"));
                    return Ok(report);
                }
                report.uninstalled = true;
            }
            _ => {
                report
                    .errors
                    .push("неподдерживаемый тип удаления для macOS".into());
                return Ok(report);
            }
        }
        let leftover_report = self.remove_leftovers(&LeftoverSelection {
            application_id: request.application_id.clone(),
            leftover_ids: request.leftover_ids.clone(),
            include_user_data: request.include_user_data,
        })?;
        report.leftovers_freed_bytes = leftover_report.leftovers_freed_bytes;
        report.remaining_leftovers = leftover_report.remaining_leftovers;
        report.warnings.extend(leftover_report.warnings);
        Ok(report)
    }

    fn remove_leftovers(
        &self,
        selection: &LeftoverSelection,
    ) -> Result<UninstallReport, TuneupError> {
        let leftovers = {
            let guard = self
                .leftovers
                .lock()
                .map_err(|_| TuneupError::Inventory("uninstall state poisoned".into()))?;
            guard
                .get(&selection.application_id)
                .cloned()
                .unwrap_or_default()
        };
        remove_selected_leftovers(
            &leftovers,
            &selection.leftover_ids,
            selection.include_user_data,
        )
    }
}

/// Discovers related files for an application under common Library roots.
fn discover_leftovers(
    bundle_id: &str,
    display_name: &str,
    install_root: Option<&Path>,
) -> Vec<LeftoverItem> {
    let tokens = match_tokens(bundle_id, display_name);
    let mut by_path = BTreeMap::<String, LeftoverItem>::new();

    let mut roots: Vec<(PathBuf, LeftoverKind, &'static str)> = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let library = home.join("Library");
        roots.extend([
            (
                library.join("Caches"),
                LeftoverKind::Cache,
                "Кэш пользователя",
            ),
            (
                library.join("Logs"),
                LeftoverKind::Logs,
                "Журналы пользователя",
            ),
            (
                library.join("Preferences"),
                LeftoverKind::Preferences,
                "Настройки",
            ),
            (
                library.join("Application Support"),
                LeftoverKind::Profile,
                "Application Support",
            ),
            (
                library.join("Containers"),
                LeftoverKind::Profile,
                "Containers",
            ),
            (
                library.join("Group Containers"),
                LeftoverKind::Profile,
                "Group Containers",
            ),
            (
                library.join("Saved Application State"),
                LeftoverKind::Other,
                "Saved Application State",
            ),
            (
                library.join("HTTPStorages"),
                LeftoverKind::Cache,
                "HTTPStorages",
            ),
            (library.join("WebKit"), LeftoverKind::Cache, "WebKit"),
            (library.join("Cookies"), LeftoverKind::Cache, "Cookies"),
            (
                library.join("Application Scripts"),
                LeftoverKind::Other,
                "Application Scripts",
            ),
            (
                library.join("LaunchAgents"),
                LeftoverKind::Other,
                "LaunchAgents",
            ),
            (library.join("Services"), LeftoverKind::Other, "Services"),
            (library.join("QuickLook"), LeftoverKind::Other, "QuickLook"),
        ]);
    }
    roots.extend([
        (
            PathBuf::from("/Library/Caches"),
            LeftoverKind::Cache,
            "Системный кэш",
        ),
        (
            PathBuf::from("/Library/Logs"),
            LeftoverKind::Logs,
            "Системные журналы",
        ),
        (
            PathBuf::from("/Library/Application Support"),
            LeftoverKind::Profile,
            "Системный Application Support",
        ),
        (
            PathBuf::from("/Library/Preferences"),
            LeftoverKind::Preferences,
            "Системные настройки",
        ),
        (
            PathBuf::from("/Library/LaunchAgents"),
            LeftoverKind::Other,
            "Системные LaunchAgents",
        ),
        (
            PathBuf::from("/Library/LaunchDaemons"),
            LeftoverKind::Other,
            "LaunchDaemons",
        ),
    ]);

    for (root, kind, description) in roots {
        collect_matching_entries(&root, kind, description, bundle_id, &tokens, &mut by_path);
    }

    // Exact preferences plist fallback when directory scan misses dotted names.
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        push_unique(
            &mut by_path,
            &home
                .join("Library/Preferences")
                .join(format!("{bundle_id}.plist")),
            LeftoverKind::Preferences,
            "Preferences",
        );
    }

    // Never treat the .app bundle itself as a leftover — it is removed by uninstall.
    if let Some(root) = install_root {
        let key = normalize_key(root);
        by_path.remove(&key);
    }

    let mut leftovers: Vec<_> = by_path.into_values().collect();
    leftovers.sort_by(|left, right| {
        left.kind
            .label()
            .cmp(right.kind.label())
            .then_with(|| left.path.cmp(&right.path))
    });
    leftovers
}

fn collect_matching_entries(
    root: &Path,
    kind: LeftoverKind,
    description: &str,
    bundle_id: &str,
    tokens: &[String],
    out: &mut BTreeMap<String, LeftoverItem>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !entry_matches(file_name, bundle_id, tokens) {
            continue;
        }
        push_unique(out, &path, kind, description);
    }
}

fn push_unique(
    out: &mut BTreeMap<String, LeftoverItem>,
    path: &Path,
    kind: LeftoverKind,
    description: &str,
) {
    if !path.exists() || is_protected_root(path) {
        return;
    }
    let key = normalize_key(path);
    if out.contains_key(&key) {
        return;
    }
    out.insert(
        key,
        LeftoverItem {
            id: format!("{:?}:{}", kind as u8, path.display()),
            path: path.to_path_buf(),
            kind,
            size_bytes: directory_size(path),
            description: description.to_owned(),
            selected_by_default: kind.selected_by_default(),
        },
    );
}

fn normalize_key(path: &Path) -> String {
    path.to_string_lossy().to_ascii_lowercase()
}

/// Builds match tokens from a bundle id and display name.
///
/// Short tokens (&lt; 4 chars) are dropped to reduce false positives.
fn match_tokens(bundle_id: &str, display_name: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bundle = bundle_id.trim().to_ascii_lowercase();
    if !bundle.is_empty() {
        tokens.push(bundle.clone());
        if let Some(last) = bundle.rsplit('.').next() {
            tokens.push(last.to_owned());
        }
        // com.vendor.App → vendor
        let parts: Vec<_> = bundle.split('.').collect();
        if parts.len() >= 2 {
            tokens.push(parts[parts.len() - 2].to_owned());
        }
    }
    let name = display_name.trim().to_ascii_lowercase();
    if !name.is_empty() {
        tokens.push(name.clone());
        tokens.push(name.replace(' ', ""));
        tokens.push(name.replace(' ', "-"));
        tokens.push(name.replace(' ', "_"));
        for part in name.split(|character: char| !character.is_ascii_alphanumeric()) {
            if part.len() >= 4 {
                tokens.push(part.to_owned());
            }
        }
    }
    tokens.sort();
    tokens.dedup();
    tokens.retain(|token| token.len() >= 4);
    tokens
}

fn entry_matches(file_name: &str, bundle_id: &str, tokens: &[String]) -> bool {
    let lower = file_name.to_ascii_lowercase();
    let bundle = bundle_id.to_ascii_lowercase();
    if !bundle.is_empty() {
        if lower == bundle
            || lower == format!("{bundle}.plist")
            || lower == format!("{bundle}.savedstate")
            || lower.starts_with(&format!("{bundle}."))
            || lower.ends_with(&format!(".{bundle}"))
            || lower.contains(&format!(".{bundle}."))
            || lower.contains(&format!(".{bundle}"))
        {
            return true;
        }
        // Group Containers: TEAMID.bundle.id
        if lower.contains(&bundle) {
            return true;
        }
    }
    tokens.iter().any(|token| {
        if lower == *token
            || lower == format!("{token}.plist")
            || lower == format!("{token}.savedstate")
            || lower.starts_with(&format!("{token}."))
            || lower.starts_with(&format!("{token}-"))
            || lower.starts_with(&format!("{token}_"))
        {
            return true;
        }
        // Require longer tokens for substring matches to avoid "code" / "helper" noise.
        token.len() >= 6
            && (lower.contains(token.as_str())
                || lower.contains(&format!(".{token}"))
                || lower.contains(&format!("{token}.")))
    })
}

fn remove_app_bundle(root: &Path) -> Result<(), PlatformError> {
    if !root.extension().is_some_and(|extension| extension == "app") {
        return Err(PlatformError::ProtectedObject(root.display().to_string()));
    }
    if root.starts_with("/System") || is_protected_root(root) {
        return Err(PlatformError::ProtectedObject(root.display().to_string()));
    }
    let parent = root
        .parent()
        .ok_or_else(|| PlatformError::ProtectedObject(root.display().to_string()))?;
    if parent == Path::new("/Applications") || parent.ends_with("Applications") {
        match remove_within(root, parent) {
            Ok(_) => Ok(()),
            Err(_) if !cfg!(target_os = "macos") => Err(PlatformError::Unsupported(
                "удаление .app доступно только в macOS".into(),
            )),
            Err(_) => privileged_remove(root),
        }
    } else {
        remove_within(root, parent).map(|_| ())
    }
}

fn privileged_remove(path: &Path) -> Result<(), PlatformError> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "do shell script \"/bin/rm -rf {}\" with administrator privileges",
            shell_quote(&path.to_string_lossy())
        );
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output()
            .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(PlatformError::HelperDenied(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(PlatformError::Unsupported(
            "удаление .app доступно только в macOS".into(),
        ))
    }
}

fn run_brew_cask_uninstall(name: &str) -> Result<(), PlatformError> {
    if !name.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '@' | '.')
    }) {
        return Err(PlatformError::ProtectedObject(name.to_owned()));
    }
    let output = Command::new("brew")
        .args(["uninstall", "--cask", "--", name])
        .output()
        .map_err(|error| PlatformError::Mutation {
            path: PathBuf::from(name),
            detail: error.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PlatformError::Partial(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_cask_names() {
        assert!(run_brew_cask_uninstall("foo;rm").is_err());
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn tokens_include_bundle_tail_and_name() {
        let tokens = match_tokens("com.microsoft.VSCode", "Visual Studio Code");
        assert!(tokens.iter().any(|token| token == "vscode"));
        assert!(tokens.iter().any(|token| token == "microsoft"));
        assert!(tokens.iter().any(|token| token.contains("visual")));
    }

    #[test]
    fn entry_matches_bundle_and_group_container() {
        let tokens = match_tokens("com.microsoft.VSCode", "Visual Studio Code");
        assert!(entry_matches(
            "com.microsoft.VSCode",
            "com.microsoft.VSCode",
            &tokens
        ));
        assert!(entry_matches(
            "com.microsoft.VSCode.plist",
            "com.microsoft.VSCode",
            &tokens
        ));
        assert!(entry_matches(
            "ABCD1234.com.microsoft.VSCode",
            "com.microsoft.VSCode",
            &tokens
        ));
        assert!(!entry_matches(
            "unrelated.app",
            "com.microsoft.VSCode",
            &tokens
        ));
    }
}
