//! Reversible Linux XDG and systemd mutations.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use tuneup_core::{
    TuneupError,
    error::PlatformError,
    model::{
        AutoStartEntry, AutostartScope, PlatformBackup, ProcessGroup, SystemdUnitBackup,
        XdgAutostartLocation, XdgDesktopBackup,
    },
};
pub use tuneup_platform::PlatformChangeReport;

/// A validated request for a privileged helper.
///
/// A helper may transport these requests through `pkexec`, D-Bus, or another
/// authenticated channel. It must independently validate paths, unit names,
/// authorization, and the supplied original bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemMutationRequest {
    DisableXdg {
        path: PathBuf,
        original_bytes: Vec<u8>,
    },
    RestoreXdg {
        path: PathBuf,
        original_bytes: Vec<u8>,
    },
    SetSystemdUnit {
        unit_name: String,
        enabled: bool,
    },
    /// Clears contents of an allowlisted system cleanup root.
    CleanPath {
        path: PathBuf,
    },
    /// Removes a package through a fixed package-manager binary.
    UninstallPackage {
        kind: String,
        identifier: String,
    },
}

/// Executes already validated system-wide mutations with elevated privileges.
pub trait SystemMutationHelper: Send + Sync {
    /// Applies one request or returns a typed platform error.
    fn execute(&self, request: &SystemMutationRequest) -> Result<(), PlatformError>;
}

/// Helper used by default; it rejects every privileged operation.
#[derive(Debug, Default)]
pub struct NoSystemMutationHelper;

impl SystemMutationHelper for NoSystemMutationHelper {
    fn execute(&self, _request: &SystemMutationRequest) -> Result<(), PlatformError> {
        Err(PlatformError::HelperUnavailable)
    }
}

#[derive(Debug, Default)]
pub struct PkexecSystemMutationHelper;

impl SystemMutationHelper for PkexecSystemMutationHelper {
    fn execute(&self, request: &SystemMutationRequest) -> Result<(), PlatformError> {
        execute_privileged(request)
    }
}

/// Mutates user autostart directly and delegates system operations to a helper.
pub struct LinuxPlatformMutator {
    helper: Arc<dyn SystemMutationHelper>,
}

impl Default for LinuxPlatformMutator {
    fn default() -> Self {
        Self::new(Arc::new(NoSystemMutationHelper))
    }
}

impl LinuxPlatformMutator {
    /// Creates a mutator backed by the supplied privileged helper.
    pub fn new(helper: Arc<dyn SystemMutationHelper>) -> Self {
        Self { helper }
    }

    /// Disables matching XDG entries and systemd units atomically as a group.
    ///
    /// Every target must be canonically contained by the group's canonical
    /// install root. Protected roots, paths, and units are rejected. If any
    /// operation fails, all completed changes are rolled back.
    pub fn disable_for_group(&mut self, group: &ProcessGroup) -> PlatformChangeReport {
        if !cfg!(target_os = "linux") {
            return unsupported_report();
        }
        let root = match group
            .install_root
            .as_deref()
            .ok_or_else(|| PlatformError::ProtectedObject("нет install root".to_owned()))
            .and_then(canonical_install_root)
        {
            Ok(root) => root,
            Err(error) => return error_report(error),
        };
        let mut backup = PlatformBackup::default();
        let mut errors = Vec::new();
        for entry in &group.autostart_entries {
            let result = match entry {
                AutoStartEntry::XdgDesktop {
                    location,
                    desktop_path,
                    hidden: false,
                    target,
                    ..
                } => self.disable_xdg(
                    *location,
                    desktop_path,
                    target.resolved_path.as_deref(),
                    &root,
                ),
                AutoStartEntry::SystemdUnit {
                    scope,
                    unit_name,
                    enabled: true,
                    active,
                    protected: false,
                    target,
                } => self.disable_unit(
                    *scope,
                    unit_name,
                    *active,
                    target.resolved_path.as_deref(),
                    &root,
                ),
                _ => continue,
            };
            match result {
                Ok(BackupItem::Xdg(value)) => backup.xdg_desktop.push(value),
                Ok(BackupItem::Unit(value)) => backup.systemd_units.push(value),
                Err(error) => {
                    errors.push(error.into());
                    break;
                }
            }
        }
        if errors.is_empty() {
            return PlatformChangeReport { backup, errors };
        }
        let rollback = self.restore(backup);
        errors.extend(rollback.errors);
        PlatformChangeReport {
            backup: rollback.backup,
            errors,
        }
    }

    /// Restores all backups and returns only items that could not be restored.
    pub fn restore(&mut self, backup: PlatformBackup) -> PlatformChangeReport {
        if !cfg!(target_os = "linux") {
            return PlatformChangeReport {
                backup,
                errors: vec![
                    PlatformError::Unsupported(
                        "Linux mutator недоступен на этой платформе".to_owned(),
                    )
                    .into(),
                ],
            };
        }
        let mut remaining = PlatformBackup::default();
        let mut errors = Vec::new();
        for value in backup.xdg_desktop {
            if let Err(error) = self.restore_xdg(&value) {
                errors.push(error.into());
                remaining.xdg_desktop.push(value);
            }
        }
        for value in backup.systemd_units {
            if let Err(error) = self.restore_unit(&value) {
                errors.push(error.into());
                remaining.systemd_units.push(value);
            }
        }
        remaining.registry = backup.registry;
        remaining.startup_moves = backup.startup_moves;
        remaining.tasks = backup.tasks;
        remaining.services = backup.services;
        remaining.launchd = backup.launchd;
        remaining.login_items = backup.login_items;
        PlatformChangeReport {
            backup: remaining,
            errors,
        }
    }

    fn disable_xdg(
        &self,
        location: XdgAutostartLocation,
        path: &Path,
        target: Option<&Path>,
        root: &Path,
    ) -> Result<BackupItem, PlatformError> {
        require_exact_target(target, root)?;
        let canonical_path = fs::canonicalize(path).map_err(|error| mutation(path, error))?;
        if protected_path(&canonical_path) && location == XdgAutostartLocation::UserConfig {
            return Err(PlatformError::ProtectedObject(
                canonical_path.display().to_string(),
            ));
        }
        let original_bytes = fs::read(&canonical_path).map_err(|error| mutation(path, error))?;
        let backup = XdgDesktopBackup {
            location,
            desktop_path: canonical_path.clone(),
            original_bytes: original_bytes.clone(),
        };
        match location {
            XdgAutostartLocation::UserConfig => {
                let changed = set_hidden(&original_bytes, true).map_err(|detail| {
                    PlatformError::Mutation {
                        path: canonical_path.clone(),
                        detail,
                    }
                })?;
                atomic_write(&canonical_path, &changed)?;
            }
            XdgAutostartLocation::SystemConfig => {
                self.helper.execute(&SystemMutationRequest::DisableXdg {
                    path: canonical_path,
                    original_bytes,
                })?
            }
        }
        Ok(BackupItem::Xdg(backup))
    }

    fn disable_unit(
        &self,
        scope: AutostartScope,
        unit_name: &str,
        was_active: bool,
        target: Option<&Path>,
        root: &Path,
    ) -> Result<BackupItem, PlatformError> {
        require_exact_target(target, root)?;
        validate_mutable_unit(unit_name)?;
        set_unit_enabled(scope, unit_name, false, self.helper.as_ref())?;
        Ok(BackupItem::Unit(SystemdUnitBackup {
            scope,
            unit_name: unit_name.to_owned(),
            was_enabled: true,
            was_active,
        }))
    }

    fn restore_xdg(&self, backup: &XdgDesktopBackup) -> Result<(), PlatformError> {
        match backup.location {
            XdgAutostartLocation::UserConfig => {
                if protected_path(&backup.desktop_path) {
                    return Err(PlatformError::ProtectedObject(
                        backup.desktop_path.display().to_string(),
                    ));
                }
                atomic_write(&backup.desktop_path, &backup.original_bytes)
            }
            XdgAutostartLocation::SystemConfig => {
                self.helper.execute(&SystemMutationRequest::RestoreXdg {
                    path: backup.desktop_path.clone(),
                    original_bytes: backup.original_bytes.clone(),
                })
            }
        }
    }

    fn restore_unit(&self, backup: &SystemdUnitBackup) -> Result<(), PlatformError> {
        validate_mutable_unit(&backup.unit_name)?;
        set_unit_enabled(
            backup.scope,
            &backup.unit_name,
            backup.was_enabled,
            self.helper.as_ref(),
        )
    }
}

impl tuneup_platform::PlatformMutator for LinuxPlatformMutator {
    fn disable_for_group(&mut self, group: &ProcessGroup) -> PlatformChangeReport {
        LinuxPlatformMutator::disable_for_group(self, group)
    }

    fn restore(&mut self, backup: PlatformBackup) -> PlatformChangeReport {
        LinuxPlatformMutator::restore(self, backup)
    }
}

enum BackupItem {
    Xdg(XdgDesktopBackup),
    Unit(SystemdUnitBackup),
}

fn canonical_install_root(root: &Path) -> Result<PathBuf, PlatformError> {
    if !root.is_absolute() {
        return Err(PlatformError::ProtectedObject(root.display().to_string()));
    }
    let canonical = fs::canonicalize(root).map_err(|error| mutation(root, error))?;
    if protected_root(&canonical) {
        return Err(PlatformError::ProtectedObject(
            canonical.display().to_string(),
        ));
    }
    Ok(canonical)
}

fn require_exact_target(target: Option<&Path>, root: &Path) -> Result<PathBuf, PlatformError> {
    let target =
        target.ok_or_else(|| PlatformError::ProtectedObject("нет target path".to_owned()))?;
    let canonical = fs::canonicalize(target).map_err(|error| mutation(target, error))?;
    let exact_file = root.is_file() && canonical == root;
    let contained_file = root.is_dir() && canonical != root && canonical.starts_with(root);
    if !exact_file && !contained_file {
        return Err(PlatformError::ProtectedObject(
            canonical.display().to_string(),
        ));
    }
    Ok(canonical)
}

fn protected_root(path: &Path) -> bool {
    const ROOTS: &[&str] = &[
        "/", "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/root", "/run", "/sbin",
        "/sys", "/usr", "/var",
    ];
    ROOTS.iter().any(|root| path == Path::new(root))
}

fn protected_path(path: &Path) -> bool {
    [
        "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/run", "/sbin", "/sys",
    ]
    .iter()
    .any(|root| path.starts_with(root))
        || path.starts_with("/usr/bin")
        || path.starts_with("/usr/sbin")
        || path.starts_with("/usr/lib")
}

fn validate_mutable_unit(name: &str) -> Result<(), PlatformError> {
    let valid = !name.is_empty()
        && name.len() <= 256
        && !name.starts_with('-')
        && name.ends_with(".service")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.@:-\\".contains(&byte));
    let protected = [
        "dbus.service",
        "systemd-logind.service",
        "systemd-journald.service",
        "systemd-udevd.service",
        "NetworkManager.service",
        "polkit.service",
        "sshd.service",
        "display-manager.service",
    ]
    .contains(&name)
        || name.starts_with("systemd-")
        || name.starts_with("user@")
        || name.starts_with("getty@");
    if !valid || protected {
        return Err(PlatformError::ProtectedObject(name.to_owned()));
    }
    Ok(())
}

fn set_hidden(bytes: &[u8], hidden: bool) -> Result<Vec<u8>, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines = text.lines().map(str::to_owned).collect::<Vec<_>>();
    let section = lines
        .iter()
        .position(|line| line.trim() == "[Desktop Entry]")
        .ok_or_else(|| "нет секции [Desktop Entry]".to_owned())?;
    let end = lines[section + 1..]
        .iter()
        .position(|line| line.trim().starts_with('['))
        .map(|offset| section + 1 + offset)
        .unwrap_or(lines.len());
    if let Some(index) = (section + 1..end).find(|index| {
        lines[*index]
            .split_once('=')
            .is_some_and(|(key, _)| key.trim() == "Hidden")
    }) {
        lines[index] = format!("Hidden={hidden}");
    } else {
        lines.insert(end, format!("Hidden={hidden}"));
    }
    let mut result = lines.join(newline);
    if text.ends_with('\n') {
        result.push_str(newline);
    }
    Ok(result.into_bytes())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
    let parent = path.parent().ok_or_else(|| PlatformError::Mutation {
        path: path.to_path_buf(),
        detail: "нет родительского каталога".to_owned(),
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| PlatformError::Mutation {
            path: path.to_path_buf(),
            detail: "нет имени файла".to_owned(),
        })?
        .to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.tuneup-{}.tmp", std::process::id()));
    fs::write(&temporary, bytes).map_err(|error| mutation(&temporary, error))?;
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(&temporary, metadata.permissions());
    }
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        mutation(path, error)
    })
}

#[cfg(target_os = "linux")]
fn execute_privileged(request: &SystemMutationRequest) -> Result<(), PlatformError> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    let current = std::env::current_exe().map_err(|_| PlatformError::HelperUnavailable)?;
    let local = current
        .parent()
        .map(|directory| directory.join("tuneup-helper"))
        .filter(|path| path.is_file());
    let helper = local.unwrap_or_else(|| PathBuf::from("/usr/libexec/tuneup-helper"));
    let mut child = Command::new("/usr/bin/pkexec")
        .arg(helper)
        .arg("--linux-system-mutation")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
    let payload = serde_json::to_vec(request)
        .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
    child
        .stdin
        .take()
        .ok_or_else(|| PlatformError::HelperDenied("нет stdin helper".to_owned()))?
        .write_all(&payload)
        .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
    let output = child
        .wait_with_output()
        .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PlatformError::HelperDenied(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(target_os = "linux")]
pub fn execute_system_mutation(request: &SystemMutationRequest) -> Result<(), PlatformError> {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };

    return match request {
        SystemMutationRequest::DisableXdg {
            path,
            original_bytes,
        } => {
            validate_system_xdg(path)?;
            let changed =
                set_hidden(original_bytes, true).map_err(|detail| PlatformError::Mutation {
                    path: path.clone(),
                    detail,
                })?;
            privileged_write(path, &changed)
        }
        SystemMutationRequest::RestoreXdg {
            path,
            original_bytes,
        } => {
            validate_system_xdg(path)?;
            privileged_write(path, original_bytes)
        }
        SystemMutationRequest::SetSystemdUnit { unit_name, enabled } => {
            validate_mutable_unit(unit_name)?;
            let action = if *enabled { "enable" } else { "disable" };
            let output = Command::new("/usr/bin/systemctl")
                .arg(action)
                .arg("--")
                .arg(unit_name)
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
        SystemMutationRequest::CleanPath { path } => {
            validate_cleanup_root(path)?;
            clear_dir_contents(path)
        }
        SystemMutationRequest::UninstallPackage { kind, identifier } => {
            uninstall_fixed_package(kind, identifier)
        }
    };

    fn privileged_write(path: &Path, bytes: &[u8]) -> Result<(), PlatformError> {
        let mut child = Command::new("/usr/bin/tee")
            .arg("--")
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
        child
            .stdin
            .take()
            .ok_or_else(|| PlatformError::HelperDenied("нет stdin helper".to_owned()))?
            .write_all(bytes)
            .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
        let output = child
            .wait_with_output()
            .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(PlatformError::HelperDenied(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ))
        }
    }

    fn validate_system_xdg(path: &Path) -> Result<(), PlatformError> {
        let canonical = fs::canonicalize(path).map_err(|error| mutation(path, error))?;
        if canonical.parent() != Some(Path::new("/etc/xdg/autostart"))
            || canonical
                .extension()
                .is_none_or(|extension| extension != "desktop")
        {
            return Err(PlatformError::ProtectedObject(path.display().to_string()));
        }
        Ok(())
    }

    fn validate_cleanup_root(path: &Path) -> Result<(), PlatformError> {
        let canonical = fs::canonicalize(path).map_err(|error| mutation(path, error))?;
        let allowed = [Path::new("/var/tmp"), Path::new("/var/cache")];
        if allowed
            .iter()
            .any(|root| canonical == *root || canonical.starts_with(root))
        {
            Ok(())
        } else {
            Err(PlatformError::ProtectedObject(
                canonical.display().to_string(),
            ))
        }
    }

    fn clear_dir_contents(path: &Path) -> Result<(), PlatformError> {
        for entry in fs::read_dir(path).map_err(|error| mutation(path, error))? {
            let entry = entry.map_err(|error| mutation(path, error))?;
            let child = entry.path();
            let metadata = fs::symlink_metadata(&child).map_err(|error| mutation(&child, error))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(&child).map_err(|error| mutation(&child, error))?;
            } else {
                fs::remove_file(&child).map_err(|error| mutation(&child, error))?;
            }
        }
        Ok(())
    }

    fn uninstall_fixed_package(kind: &str, identifier: &str) -> Result<(), PlatformError> {
        let valid = !identifier.is_empty()
            && identifier.len() <= 256
            && identifier.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '-' | '_' | '.' | '+' | '@' | ':' | '/')
            });
        if !valid {
            return Err(PlatformError::ProtectedObject(identifier.to_owned()));
        }
        let (program, args): (&str, Vec<&str>) = match kind {
            "deb" => ("/usr/bin/apt-get", vec!["remove", "-y", "--", identifier]),
            "rpm" => {
                if Path::new("/usr/bin/dnf").exists() {
                    ("/usr/bin/dnf", vec!["remove", "-y", "--", identifier])
                } else {
                    ("/usr/bin/rpm", vec!["-e", "--", identifier])
                }
            }
            "flatpak" => (
                "/usr/bin/flatpak",
                vec!["uninstall", "-y", "--", identifier],
            ),
            "snap" => ("/usr/bin/snap", vec!["remove", "--", identifier]),
            other => {
                return Err(PlatformError::Unsupported(format!(
                    "неизвестный linux uninstall kind: {other}"
                )));
            }
        };
        let output = Command::new(program)
            .args(args)
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
}

#[cfg(not(target_os = "linux"))]
fn execute_privileged(_request: &SystemMutationRequest) -> Result<(), PlatformError> {
    Err(PlatformError::Unsupported(
        "pkexec доступен только в Linux".to_owned(),
    ))
}

#[cfg(not(target_os = "linux"))]
pub fn execute_system_mutation(_request: &SystemMutationRequest) -> Result<(), PlatformError> {
    Err(PlatformError::Unsupported(
        "Linux helper недоступен на этой платформе".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn set_unit_enabled(
    scope: AutostartScope,
    unit_name: &str,
    enabled: bool,
    helper: &dyn SystemMutationHelper,
) -> Result<(), PlatformError> {
    use std::process::Command;

    if scope == AutostartScope::System {
        return helper.execute(&SystemMutationRequest::SetSystemdUnit {
            unit_name: unit_name.to_owned(),
            enabled,
        });
    }
    let action = if enabled { "enable" } else { "disable" };
    let output = Command::new("systemctl")
        .args(["--user", action, "--", unit_name])
        .output()
        .map_err(|error| PlatformError::Mutation {
            path: PathBuf::from(unit_name),
            detail: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(PlatformError::Mutation {
            path: PathBuf::from(unit_name),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn set_unit_enabled(
    _scope: AutostartScope,
    _unit_name: &str,
    _enabled: bool,
    _helper: &dyn SystemMutationHelper,
) -> Result<(), PlatformError> {
    Err(PlatformError::Unsupported(
        "systemctl доступен только в Linux".to_owned(),
    ))
}

fn mutation(path: &Path, error: impl std::fmt::Display) -> PlatformError {
    PlatformError::Mutation {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn error_report(error: PlatformError) -> PlatformChangeReport {
    PlatformChangeReport {
        backup: PlatformBackup::default(),
        errors: vec![TuneupError::Platform(error)],
    }
}

fn unsupported_report() -> PlatformChangeReport {
    error_report(PlatformError::Unsupported(
        "Linux mutator недоступен на этой платформе".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_change_is_reversible_byte_for_byte() {
        let original = b"[Desktop Entry]\nName=Agent\nExec=/opt/acme/agent\n";
        let changed = set_hidden(original, true).unwrap();
        assert!(String::from_utf8(changed).unwrap().contains("Hidden=true"));
        assert_eq!(
            original,
            &b"[Desktop Entry]\nName=Agent\nExec=/opt/acme/agent\n"[..]
        );
    }

    #[test]
    fn rejects_traversal_by_canonical_target() {
        let base = std::env::temp_dir().join(format!("tuneup-linux-test-{}", std::process::id()));
        let root = base.join("app");
        let outside = base.join("outside");
        fs::create_dir_all(&root).unwrap();
        fs::write(&outside, b"x").unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();
        assert!(require_exact_target(Some(&root.join("../outside")), &canonical_root).is_err());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn rejects_protected_units_and_option_injection() {
        assert!(validate_mutable_unit("systemd-logind.service").is_err());
        assert!(validate_mutable_unit("--now.service").is_err());
        assert!(validate_mutable_unit("vendor-agent.service").is_ok());
    }
}
