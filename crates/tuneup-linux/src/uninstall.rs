//! Linux package inventory and removal via apt/dnf/flatpak/snap.

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

struct CachedApp {
    app: InstalledApplication,
}

/// Lists Linux packages and removes them through fixed package-manager binaries.
pub struct LinuxUninstallProvider {
    apps: Mutex<BTreeMap<String, CachedApp>>,
    leftovers: Mutex<BTreeMap<String, Vec<LeftoverItem>>>,
}

impl LinuxUninstallProvider {
    /// Creates an empty provider; applications are loaded on demand.
    pub fn new() -> Self {
        Self {
            apps: Mutex::new(BTreeMap::new()),
            leftovers: Mutex::new(BTreeMap::new()),
        }
    }

    fn load_applications(&self) -> Result<Vec<InstalledApplication>, TuneupError> {
        // Do NOT dump every DEB/RPM package (thousands of libs). Uninstall UI is for
        // user-facing apps: XDG .desktop entries + Flatpak + Snap.
        let mut apps = BTreeMap::new();
        let mut list = Vec::new();
        append_desktop_applications(&mut apps, &mut list);
        append_flatpak(&mut apps, &mut list);
        append_snap(&mut apps, &mut list);
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
        let name = cached.app.uninstall_identifier.clone();
        drop(apps);
        let mut leftovers = Vec::new();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".cache")));
        if let Some(cache) = cache {
            push_leftover(
                &mut leftovers,
                &cache.join(&name),
                LeftoverKind::Cache,
                "XDG cache",
            );
        }
        if let Some(home) = home {
            push_leftover(
                &mut leftovers,
                &home.join(".config").join(&name),
                LeftoverKind::Preferences,
                "XDG config",
            );
            push_leftover(
                &mut leftovers,
                &home.join(".local/share").join(&name),
                LeftoverKind::Profile,
                "XDG data",
            );
            push_leftover(
                &mut leftovers,
                &home.join(".local/state").join(&name),
                LeftoverKind::Logs,
                "XDG state",
            );
        }
        if let Ok(mut guard) = self.leftovers.lock() {
            guard.insert(application_id.to_owned(), leftovers.clone());
        }
        Ok(leftovers)
    }
}

impl Default for LinuxUninstallProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl UninstallProvider for LinuxUninstallProvider {
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
        let (kind, identifier, name, protected) = {
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
                cached.app.name.clone(),
                cached.app.protected,
            )
        };
        let _ = protected;
        let mut report = UninstallReport::default();
        if let Err(error) = remove_package(kind, &identifier) {
            report
                .errors
                .push(format!("не удалось удалить «{name}»: {error}"));
            return Ok(report);
        }
        report.uninstalled = true;
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

#[derive(Debug, Default)]
struct DesktopApplication {
    name: String,
    exec: String,
    hidden: bool,
    no_display: bool,
    is_application: bool,
}

fn parse_application_desktop(bytes: &[u8]) -> Option<DesktopApplication> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut in_desktop = false;
    let mut parsed = DesktopApplication {
        is_application: true,
        ..DesktopApplication::default()
    };
    let mut localized_name = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "Type" => parsed.is_application = value.eq_ignore_ascii_case("Application"),
            "Name" if !value.is_empty() => parsed.name = value.to_owned(),
            key if key.starts_with("Name[") && key.ends_with(']') && !value.is_empty() => {
                if localized_name.is_none() {
                    localized_name = Some(value.to_owned());
                }
            }
            "Exec" if !value.is_empty() => parsed.exec = value.to_owned(),
            "Hidden" => parsed.hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => parsed.no_display = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    if parsed.name.is_empty() {
        parsed.name = localized_name.unwrap_or_default();
    }
    (parsed.is_application && !parsed.name.is_empty() && !parsed.exec.is_empty()).then_some(parsed)
}

fn desktop_exec_binary(exec: &str) -> Option<PathBuf> {
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in exec.chars() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            } else {
                token.push(character);
            }
            continue;
        }
        if character == '\'' || character == '"' {
            quote = Some(character);
            continue;
        }
        if character.is_whitespace() {
            break;
        }
        // Desktop Exec field codes (%f, %u, …) are not part of the binary path.
        if character == '%' {
            break;
        }
        token.push(character);
    }
    if token.is_empty() || token == "env" || token == "flatpak" || token == "snap" {
        return None;
    }
    let path = PathBuf::from(&token);
    if path.is_absolute() {
        return std::fs::canonicalize(&path).ok().or(Some(path));
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(&path))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| std::fs::canonicalize(candidate).ok())
}

fn application_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local/share/applications"));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        dirs.push(data_home.join("applications"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for dir in data_dirs.split(':').filter(|value| !value.is_empty()) {
        dirs.push(PathBuf::from(dir).join("applications"));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn package_owning_path(path: &Path) -> Option<(InstallSource, String, Option<String>)> {
    if let Ok(output) = Command::new("dpkg-query")
        .args(["-S", &path.display().to_string()])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        // "firefox: /usr/bin/firefox" or "pkg1, pkg2: /path"
        if let Some(prefix) = text.split(':').next() {
            let package = prefix
                .split(',')
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let version = package_version_deb(package);
            return Some((InstallSource::Deb, package.to_owned(), version));
        }
    }
    if let Ok(output) = Command::new("rpm")
        .args(["-qf", "--qf", "%{NAME}\t%{VERSION}-%{RELEASE}\n", &path.display().to_string()])
        .output()
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let mut fields = text.lines().next()?.split('\t');
        let package = fields.next()?.trim();
        if package.is_empty() || package.contains("not owned") {
            return None;
        }
        let version = fields
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        return Some((InstallSource::Rpm, package.to_owned(), version));
    }
    None
}

fn package_version_deb(package: &str) -> Option<String> {
    let output = Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", package])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_owned();
    (!version.is_empty()).then_some(version)
}

/// Builds uninstall entries from XDG `.desktop` application launchers.
///
/// This keeps the uninstall list to user-facing apps (~hundreds) instead of every
/// installed DEB/RPM package (often thousands of libraries).
fn append_desktop_applications(
    apps: &mut BTreeMap<String, CachedApp>,
    list: &mut Vec<InstalledApplication>,
) {
    let mut seen_desktop = std::collections::HashSet::new();
    for directory in application_data_dirs() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_owned();
            if !seen_desktop.insert(file_name.clone()) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Some(desktop) = parse_application_desktop(&bytes) else {
                continue;
            };
            // Skip launcher stubs for sandboxed runtimes — Flatpak/Snap have dedicated scanners.
            let exec_lower = desktop.exec.to_ascii_lowercase();
            if exec_lower.contains("flatpak ")
                || exec_lower.starts_with("flatpak")
                || exec_lower.contains("/snap/")
                || desktop.hidden
                || desktop.no_display
            {
                continue;
            }
            let binary = desktop_exec_binary(&desktop.exec);
            let package = binary.as_ref().and_then(|path| package_owning_path(path));
            let (source, uninstall_kind, uninstall_identifier, version, protected) =
                if let Some((source, package, version)) = package {
                    let kind = match source {
                        InstallSource::Deb | InstallSource::XdgDesktop => UninstallKind::LinuxDeb,
                        InstallSource::Rpm => UninstallKind::LinuxRpm,
                        InstallSource::WindowsRegistry | InstallSource::MacOsBundle => {
                            UninstallKind::LinuxDeb
                        }
                    };
                    let protected = is_linux_protected(&package);
                    (source, kind, package, version, protected)
                } else {
                    // Desktop entry without a package manager owner — still listed, not removable via apt/dnf.
                    (
                        InstallSource::Deb,
                        UninstallKind::LinuxDeb,
                        file_name.trim_end_matches(".desktop").to_owned(),
                        None,
                        true,
                    )
                };
            let id = format!("{source:?}:{uninstall_identifier}");
            if apps.contains_key(&id) {
                continue;
            }
            let app = InstalledApplication {
                id: id.clone(),
                name: desktop.name,
                version,
                publisher: None,
                source,
                install_root: binary.as_ref().and_then(|path| path.parent().map(Path::to_path_buf)),
                estimated_size: None,
                uninstall_kind,
                uninstall_identifier,
                user_scope: directory.starts_with(
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default(),
                ),
                removable: !protected,
                protected,
            };
            list.push(app.clone());
            apps.insert(id, CachedApp { app });
        }
    }
}

fn append_flatpak(apps: &mut BTreeMap<String, CachedApp>, list: &mut Vec<InstalledApplication>) {
    let Ok(output) = Command::new("flatpak")
        .args(["list", "--app", "--columns=application,name,version"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split('\t');
        let Some(app_id) = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let name = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(app_id);
        let version = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let id = format!("flatpak:{app_id}");
        if apps.contains_key(&id) {
            continue;
        }
        let app = InstalledApplication {
            id: id.clone(),
            name: name.to_owned(),
            version,
            publisher: None,
            source: InstallSource::Deb,
            install_root: None,
            estimated_size: None,
            uninstall_kind: UninstallKind::LinuxFlatpak,
            uninstall_identifier: app_id.to_owned(),
            user_scope: true,
            removable: true,
            protected: false,
        };
        list.push(app.clone());
        apps.insert(id, CachedApp { app });
    }
}

fn append_snap(apps: &mut BTreeMap<String, CachedApp>, list: &mut Vec<InstalledApplication>) {
    let Ok(output) = Command::new("snap").args(["list"]).output() else {
        return;
    };
    if !output.status.success() {
        return;
    }
    for line in String::from_utf8_lossy(&output.stdout).lines().skip(1) {
        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else {
            continue;
        };
        let version = parts.next().map(str::to_owned);
        let id = format!("snap:{name}");
        if apps.contains_key(&id) || name == "snapd" {
            continue;
        }
        let protected = name == "core" || name.starts_with("core");
        let app = InstalledApplication {
            id: id.clone(),
            name: name.to_owned(),
            version,
            publisher: None,
            source: InstallSource::Deb,
            install_root: None,
            estimated_size: None,
            uninstall_kind: UninstallKind::LinuxSnap,
            uninstall_identifier: name.to_owned(),
            user_scope: false,
            removable: !protected,
            protected,
        };
        list.push(app.clone());
        apps.insert(id, CachedApp { app });
    }
}

fn remove_package(kind: UninstallKind, identifier: &str) -> Result<(), PlatformError> {
    validate_package_name(identifier)?;
    let (program, args): (&str, Vec<&str>) = match kind {
        UninstallKind::LinuxDeb => ("apt-get", vec!["remove", "-y", "--", identifier]),
        UninstallKind::LinuxRpm => {
            if Path::new("/usr/bin/dnf").exists() {
                ("dnf", vec!["remove", "-y", "--", identifier])
            } else {
                ("rpm", vec!["-e", "--", identifier])
            }
        }
        UninstallKind::LinuxFlatpak => ("flatpak", vec!["uninstall", "-y", "--", identifier]),
        UninstallKind::LinuxSnap => ("snap", vec!["remove", "--", identifier]),
        _ => {
            return Err(PlatformError::Unsupported(
                "неподдерживаемый тип удаления для Linux".into(),
            ));
        }
    };
    // Privileged package removal goes through pkexec for system packages.
    let needs_pkexec = matches!(
        kind,
        UninstallKind::LinuxDeb | UninstallKind::LinuxRpm | UninstallKind::LinuxSnap
    );
    let mut command = if needs_pkexec && Path::new("/usr/bin/pkexec").exists() {
        let mut command = Command::new("/usr/bin/pkexec");
        command.arg(program);
        command.args(&args);
        command
    } else {
        let mut command = Command::new(program);
        command.args(&args);
        command
    };
    let output = command
        .output()
        .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PlatformError::Partial(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn validate_package_name(name: &str) -> Result<(), PlatformError> {
    let valid = !name.is_empty()
        && name.len() <= 256
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | '+' | '@' | ':' | '/')
        });
    if valid {
        Ok(())
    } else {
        Err(PlatformError::ProtectedObject(name.to_owned()))
    }
}

fn is_linux_protected(name: &str) -> bool {
    [
        "bash",
        "coreutils",
        "dash",
        "glibc",
        "libc6",
        "systemd",
        "udev",
        "login",
        "passwd",
        "sudo",
        "polkit",
        "policykit-1",
        "dbus",
        "linux-image",
        "linux-firmware",
    ]
    .iter()
    .any(|item| name == *item || name.starts_with(&format!("{item}-")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shell_metacharacters_in_package_names() {
        assert!(validate_package_name("foo;rm").is_err());
        assert!(validate_package_name("firefox").is_ok());
    }

    #[test]
    fn protects_base_system_packages() {
        assert!(is_linux_protected("systemd"));
        assert!(is_linux_protected("libc6"));
        assert!(!is_linux_protected("vlc"));
    }

    #[test]
    fn parses_desktop_application_entries() {
        let parsed = parse_application_desktop(
            b"[Desktop Entry]\nType=Application\nName=Demo\nExec=\"/opt/Demo App/bin/demo\" %u\n",
        )
        .unwrap();
        assert_eq!(parsed.name, "Demo");
        assert_eq!(
            desktop_exec_binary(&parsed.exec),
            Some(PathBuf::from("/opt/Demo App/bin/demo"))
        );
    }

    #[test]
    fn marks_hidden_and_rejects_non_application_desktop_entries() {
        let hidden = parse_application_desktop(
            b"[Desktop Entry]\nType=Application\nName=Hidden\nExec=true\nNoDisplay=true\n",
        )
        .unwrap();
        assert!(hidden.no_display);
        assert!(
            parse_application_desktop(b"[Desktop Entry]\nType=Link\nName=Site\nExec=true\n")
                .is_none()
        );
    }
}
