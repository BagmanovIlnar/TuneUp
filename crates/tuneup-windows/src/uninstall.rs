//! Windows application inventory, uninstall and leftover discovery.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

use tuneup_core::{
    TuneupError,
    error::PlatformError,
    fs_util::{directory_size, is_protected_root},
    model::InstallSource,
    uninstall::{
        InstalledApplication, LeftoverItem, LeftoverKind, LeftoverSelection, UninstallKind,
        UninstallReport, UninstallRequest,
    },
};
use tuneup_platform::{UninstallProvider, uninstall_util::remove_selected_leftovers};

use crate::inventory::InventoryAggregator;

struct CachedApp {
    app: InstalledApplication,
    uninstall_command: Option<String>,
}

/// Lists Windows programs and removes them through native uninstallers.
pub struct WindowsUninstallProvider {
    apps: Mutex<BTreeMap<String, CachedApp>>,
    leftovers: Mutex<BTreeMap<String, Vec<LeftoverItem>>>,
}

impl WindowsUninstallProvider {
    /// Creates an empty provider; applications are loaded on demand.
    pub fn new() -> Self {
        Self {
            apps: Mutex::new(BTreeMap::new()),
            leftovers: Mutex::new(BTreeMap::new()),
        }
    }

    fn load_applications(&self) -> Result<Vec<InstalledApplication>, TuneupError> {
        let snapshot = InventoryAggregator.snapshot()?;
        let mut apps = BTreeMap::new();
        let mut list = Vec::new();
        for record in snapshot.uninstall {
            let name = record
                .display_name
                .clone()
                .unwrap_or_else(|| record.key_name.clone());
            let id = format!("win:{}", record.key_name);
            let install_root = record
                .install_location
                .filter(|path| path.is_absolute() && !is_protected_root(path) && path.exists());
            let protected = is_windows_protected(&name, record.publisher.as_deref());
            let app = InstalledApplication {
                id: id.clone(),
                name,
                version: None,
                publisher: record.publisher,
                source: InstallSource::WindowsRegistry,
                install_root,
                estimated_size: None,
                uninstall_kind: UninstallKind::WindowsCommand,
                uninstall_identifier: record.key_name.clone(),
                user_scope: matches!(record.hive, tuneup_core::model::RegistryHive::CurrentUser),
                removable: !protected,
                protected,
            };
            list.push(app.clone());
            apps.insert(
                id,
                CachedApp {
                    app,
                    uninstall_command: None,
                },
            );
        }
        // Enrich with QuietUninstallString / UninstallString when on Windows.
        #[cfg(windows)]
        enrich_uninstall_commands(&mut apps);
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
        let mut leftovers = Vec::new();
        let slug = sanitize_name(&cached.app.name);
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            push_leftover(
                &mut leftovers,
                &local.join(&cached.app.name),
                LeftoverKind::Profile,
                "LocalAppData профиль",
            );
            push_leftover(
                &mut leftovers,
                &local.join(&slug),
                LeftoverKind::Profile,
                "LocalAppData профиль",
            );
            push_leftover(
                &mut leftovers,
                &local.join("Temp").join(&slug),
                LeftoverKind::Temp,
                "Временные файлы приложения",
            );
        }
        if let Some(roaming) = std::env::var_os("APPDATA").map(PathBuf::from) {
            push_leftover(
                &mut leftovers,
                &roaming.join(&cached.app.name),
                LeftoverKind::Preferences,
                "Roaming настройки",
            );
        }
        if let Some(root) = &cached.app.install_root {
            push_leftover(
                &mut leftovers,
                &root.join("cache"),
                LeftoverKind::Cache,
                "Кэш в каталоге установки",
            );
            push_leftover(
                &mut leftovers,
                &root.join("logs"),
                LeftoverKind::Logs,
                "Журналы в каталоге установки",
            );
        }
        drop(apps);
        if let Ok(mut guard) = self.leftovers.lock() {
            guard.insert(application_id.to_owned(), leftovers.clone());
        }
        Ok(leftovers)
    }
}

impl Default for WindowsUninstallProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UninstallProvider for WindowsUninstallProvider {
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
        let (protected, command, name) = {
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
                cached.app.protected,
                cached.uninstall_command.clone(),
                cached.app.name.clone(),
            )
        };
        let _ = protected;
        let mut report = UninstallReport::default();
        match run_windows_uninstall(command.as_deref()) {
            Ok(()) => report.uninstalled = true,
            Err(error) => {
                report
                    .errors
                    .push(format!("не удалось удалить «{name}»: {error}"));
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
        report.errors.extend(leftover_report.errors);
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

fn push_leftover(out: &mut Vec<LeftoverItem>, path: &Path, kind: LeftoverKind, description: &str) {
    if !path.exists() || is_protected_root(path) {
        return;
    }
    out.push(LeftoverItem {
        id: format!("{:?}:{}", kind as u8, path.display()),
        path: path.to_path_buf(),
        kind,
        size_bytes: directory_size(path),
        description: description.to_owned(),
        selected_by_default: kind.selected_by_default(),
    });
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn is_windows_protected(name: &str, publisher: Option<&str>) -> bool {
    let lowered = name.to_ascii_lowercase();
    let publisher = publisher.unwrap_or("").to_ascii_lowercase();
    lowered.contains("microsoft visual c++")
        || lowered.contains("microsoft .net")
        || lowered.starts_with("windows ")
        || (lowered.contains("kb") && publisher.contains("microsoft"))
        || (publisher == "microsoft corporation"
            && (lowered.contains("edge") || lowered.contains("defender")))
}

fn run_windows_uninstall(command: Option<&str>) -> Result<(), PlatformError> {
    let Some(command) = command.filter(|value| !value.trim().is_empty()) else {
        return Err(PlatformError::Unsupported(
            "команда удаления недоступна для этой записи".to_owned(),
        ));
    };
    let (program, args) = split_command_line(command).ok_or_else(|| {
        PlatformError::ProtectedObject(format!("небезопасная команда удаления: {command}"))
    })?;
    if !Path::new(&program).is_absolute() {
        return Err(PlatformError::ProtectedObject(program));
    }
    let status = Command::new(&program)
        .args(&args)
        .status()
        .map_err(|error| PlatformError::Mutation {
            path: PathBuf::from(&program),
            detail: error.to_string(),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(PlatformError::Partial(format!(
            "деинсталлятор завершился с кодом {status}"
        )))
    }
}

fn split_command_line(value: &str) -> Option<(String, Vec<String>)> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for character in value.chars() {
        match character {
            '"' => in_quotes = !in_quotes,
            ' ' if !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    let mut iter = tokens.into_iter();
    let program = iter.next()?;
    Some((program, iter.collect()))
}

#[cfg(windows)]
fn enrich_uninstall_commands(apps: &mut BTreeMap<String, CachedApp>) {
    use winreg::{
        HKCU, HKLM,
        enums::{KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY},
    };
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    for (root, _) in [(HKCU, "hkcu"), (HKLM, "hklm")] {
        for flags in [0, KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
            let Ok(uninstall) = root.open_subkey_with_flags(UNINSTALL_KEY, KEY_READ | flags) else {
                continue;
            };
            for key_name in uninstall.enum_keys().filter_map(Result::ok) {
                let id = format!("win:{key_name}");
                let Some(cached) = apps.get_mut(&id) else {
                    continue;
                };
                let Ok(key) = uninstall.open_subkey_with_flags(&key_name, KEY_READ) else {
                    continue;
                };
                let quiet = key.get_value::<String, _>("QuietUninstallString").ok();
                let normal = key.get_value::<String, _>("UninstallString").ok();
                cached.uninstall_command = quiet.or(normal);
                if let Ok(version) = key.get_value::<String, _>("DisplayVersion") {
                    cached.app.version = Some(version);
                }
                if let Ok(size_kb) = key.get_value::<u32, _>("EstimatedSize") {
                    cached.app.estimated_size = Some(u64::from(size_kb) * 1024);
                }
                if key_name.starts_with('{') && key_name.ends_with('}') {
                    cached.app.uninstall_kind = UninstallKind::WindowsMsi;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_quoted_uninstall_command() {
        let (program, args) =
            split_command_line(r#""C:\Program Files\App\uninstall.exe" /S"#).unwrap();
        assert_eq!(program, r"C:\Program Files\App\uninstall.exe");
        assert_eq!(args, vec!["/S"]);
    }

    #[test]
    fn protects_microsoft_runtime_packages() {
        assert!(is_windows_protected(
            "Microsoft Visual C++ 2015 Redistributable",
            Some("Microsoft Corporation")
        ));
        assert!(!is_windows_protected("Acme Editor", Some("Acme Inc")));
    }
}
