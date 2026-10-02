use tuneup_core::{
    error::PlatformError,
    model::{PlatformBackup, ProcessGroup},
};
pub use tuneup_platform::PlatformChangeReport;

/// Disables and restores launchd entries associated with an exact install root.
pub struct MacOsPlatformMutator;

impl MacOsPlatformMutator {
    /// Creates a stateless macOS mutator.
    pub const fn new() -> Self {
        Self
    }

    /// Disables matching launchd jobs. If a later operation fails, completed
    /// operations are rolled back and only unrestored backup records remain.
    pub fn disable_for_group(&mut self, group: &ProcessGroup) -> PlatformChangeReport {
        let mut report = imp::disable_for_group(group);
        if !report.errors.is_empty() && !report.backup.is_empty() {
            let rollback = imp::restore(report.backup);
            report.backup = rollback.backup;
            report.errors.extend(rollback.errors);
        }
        report
    }

    /// Restores jobs from a complete `LaunchdBackup`, retaining records whose
    /// restore failed so callers can retry safely.
    pub fn restore(&mut self, backup: PlatformBackup) -> PlatformChangeReport {
        imp::restore(backup)
    }
}

impl Default for MacOsPlatformMutator {
    fn default() -> Self {
        Self::new()
    }
}

impl tuneup_platform::PlatformMutator for MacOsPlatformMutator {
    fn disable_for_group(&mut self, group: &ProcessGroup) -> PlatformChangeReport {
        MacOsPlatformMutator::disable_for_group(self, group)
    }

    fn restore(&mut self, backup: PlatformBackup) -> PlatformChangeReport {
        MacOsPlatformMutator::restore(self, backup)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn disable_for_group(_group: &ProcessGroup) -> PlatformChangeReport {
        PlatformChangeReport {
            backup: PlatformBackup::default(),
            errors: vec![
                PlatformError::Unsupported("launchd доступен только в macOS".to_owned()).into(),
            ],
        }
    }

    pub fn restore(backup: PlatformBackup) -> PlatformChangeReport {
        if backup.launchd.is_empty() {
            PlatformChangeReport {
                backup,
                errors: Vec::new(),
            }
        } else {
            PlatformChangeReport {
                backup,
                errors: vec![
                    PlatformError::Unsupported("launchd доступен только в macOS".to_owned()).into(),
                ],
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    use tuneup_core::{
        TuneupError,
        model::{AutoStartEntry, LaunchdBackup, LaunchdDomain, LoginItemBackup, LoginItemKind},
    };

    use super::*;
    use crate::path_util::{
        canonical_existing, exact_install_root_match, protected_label, protected_path,
    };

    pub fn disable_for_group(group: &ProcessGroup) -> PlatformChangeReport {
        let Some(root) = group.install_root.as_deref() else {
            return error_report(PlatformError::ProtectedObject(
                "у группы нет точного install root".to_owned(),
            ));
        };
        let root = match canonical_existing(root) {
            Ok(root) if !protected_path(&root) => root,
            Ok(root) => {
                return error_report(PlatformError::ProtectedObject(root.display().to_string()));
            }
            Err(error) => return error_report(error),
        };

        let mut backup = PlatformBackup::default();
        let mut errors = Vec::new();
        for entry in &group.autostart_entries {
            match entry {
                AutoStartEntry::Launchd {
                    domain,
                    label,
                    plist_path,
                    loaded,
                    protected,
                    target,
                } => {
                    let Some(target_path) = target.resolved_path.as_deref() else {
                        errors.push(
                            PlatformError::ProtectedObject(format!(
                                "{label}: цель не является абсолютным путём"
                            ))
                            .into(),
                        );
                        continue;
                    };
                    if *protected
                        || protected_label(label)
                        || !exact_install_root_match(target_path, &root)
                    {
                        errors.push(PlatformError::ProtectedObject(label.clone()).into());
                        continue;
                    }
                    match disable_launchd(*domain, label, plist_path, *loaded) {
                        Ok(value) => backup.launchd.push(value),
                        Err(error) => errors.push(error.into()),
                    }
                }
                AutoStartEntry::LoginItem {
                    kind,
                    identifier,
                    target,
                    mutable: true,
                    ..
                } => {
                    let Some(target_path) = target.resolved_path.as_deref() else {
                        continue;
                    };
                    if !exact_install_root_match(target_path, &root) {
                        errors.push(PlatformError::ProtectedObject(identifier.clone()).into());
                        continue;
                    }
                    match disable_login_item(*kind, identifier, target_path) {
                        Ok(value) => backup.login_items.push(value),
                        Err(error) => errors.push(error.into()),
                    }
                }
                _ => {}
            }
        }
        PlatformChangeReport { backup, errors }
    }

    pub fn restore(mut backup: PlatformBackup) -> PlatformChangeReport {
        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        for value in backup.launchd.drain(..) {
            if let Err(error) = restore_launchd(&value) {
                errors.push(error.into());
                remaining.push(value);
            }
        }
        backup.launchd = remaining;
        let mut remaining_login = Vec::new();
        for value in backup.login_items.drain(..) {
            if let Err(error) = restore_login_item(&value) {
                errors.push(error.into());
                remaining_login.push(value);
            }
        }
        backup.login_items = remaining_login;
        PlatformChangeReport { backup, errors }
    }

    fn disable_login_item(
        kind: LoginItemKind,
        identifier: &str,
        target: &Path,
    ) -> Result<LoginItemBackup, PlatformError> {
        let script = format!(
            "tell application id \"com.apple.systemevents\" to delete login item \"{}\"",
            apple_script_string(identifier)
        );
        run_osascript(&script, target)?;
        Ok(LoginItemBackup {
            kind,
            identifier: identifier.to_owned(),
            target: Some(target.to_path_buf()),
        })
    }

    fn restore_login_item(backup: &LoginItemBackup) -> Result<(), PlatformError> {
        let target = backup.target.as_deref().ok_or_else(|| {
            PlatformError::ProtectedObject(format!("Login Item {}", backup.identifier))
        })?;
        let script = format!(
            "tell application id \"com.apple.systemevents\" to make login item at end with properties {{path:\"{}\", hidden:false}}",
            apple_script_string(&target.to_string_lossy())
        );
        run_osascript(&script, target)
    }

    fn run_osascript(script: &str, path: &Path) -> Result<(), PlatformError> {
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .output()
            .map_err(|error| mutation(path, &error.to_string()))?;
        command_result(output, path)
    }

    fn disable_launchd(
        domain: LaunchdDomain,
        label: &str,
        source: &Path,
        was_loaded: bool,
    ) -> Result<LaunchdBackup, PlatformError> {
        let source = canonical_existing(source)?;
        validate_plist_path(&source, domain)?;
        if protected_label(label) {
            return Err(PlatformError::ProtectedObject(label.to_owned()));
        }
        let parent = source
            .parent()
            .ok_or_else(|| mutation(&source, "нет parent-каталога"))?;
        let disabled_dir = parent.join("TuneUpDisabled");
        let file_name = source
            .file_name()
            .ok_or_else(|| mutation(&source, "нет имени plist"))?;
        let destination = unique_destination(&disabled_dir, file_name);

        match domain {
            LaunchdDomain::UserAgent => {
                disable_user(&source, &disabled_dir, &destination, was_loaded)?
            }
            LaunchdDomain::SystemAgent | LaunchdDomain::SystemDaemon => {
                disable_privileged(domain, &source, &disabled_dir, &destination, was_loaded)?
            }
        }
        Ok(LaunchdBackup {
            domain,
            label: label.to_owned(),
            original_path: source,
            disabled_path: destination,
            was_loaded,
        })
    }

    fn restore_launchd(backup: &LaunchdBackup) -> Result<(), PlatformError> {
        validate_backup(backup)?;
        if !backup.disabled_path.exists() && backup.original_path.exists() {
            return Ok(());
        }
        if backup.original_path.exists() {
            return Err(mutation(
                &backup.original_path,
                "целевой plist уже существует",
            ));
        }
        match backup.domain {
            LaunchdDomain::UserAgent => restore_user(backup),
            LaunchdDomain::SystemAgent | LaunchdDomain::SystemDaemon => restore_privileged(backup),
        }
    }

    fn disable_user(
        source: &Path,
        disabled_dir: &Path,
        destination: &Path,
        was_loaded: bool,
    ) -> Result<(), PlatformError> {
        if was_loaded {
            launchctl("bootout", &launch_domain(LaunchdDomain::UserAgent), source)?;
        }
        let result = fs::create_dir_all(disabled_dir)
            .and_then(|()| fs::rename(source, destination))
            .map_err(|error| mutation(source, &error.to_string()));
        if let Err(error) = result {
            if was_loaded {
                let _ = launchctl(
                    "bootstrap",
                    &launch_domain(LaunchdDomain::UserAgent),
                    source,
                );
            }
            return Err(error);
        }
        Ok(())
    }

    fn restore_user(backup: &LaunchdBackup) -> Result<(), PlatformError> {
        fs::rename(&backup.disabled_path, &backup.original_path)
            .map_err(|error| mutation(&backup.disabled_path, &error.to_string()))?;
        if backup.was_loaded
            && let Err(error) = launchctl(
                "bootstrap",
                &launch_domain(backup.domain),
                &backup.original_path,
            )
        {
            let rollback = fs::rename(&backup.original_path, &backup.disabled_path);
            return Err(with_rollback(error, rollback.err()));
        }
        Ok(())
    }

    fn disable_privileged(
        domain: LaunchdDomain,
        source: &Path,
        disabled_dir: &Path,
        destination: &Path,
        was_loaded: bool,
    ) -> Result<(), PlatformError> {
        let mut commands = Vec::new();
        if was_loaded {
            commands.push(fixed_launchctl("bootout", domain, source));
        }
        commands.push(format!(
            "/bin/mkdir -p {}",
            shell_quote(disabled_dir.as_os_str())
        ));
        commands.push(format!(
            "/bin/mv {} {}",
            shell_quote(source.as_os_str()),
            shell_quote(destination.as_os_str())
        ));
        if let Err(error) = privileged(&commands.join(" && "), source) {
            if was_loaded {
                let rollback = privileged(&fixed_launchctl("bootstrap", domain, source), source);
                return Err(with_platform_rollback(error, rollback.err()));
            }
            return Err(error);
        }
        Ok(())
    }

    fn restore_privileged(backup: &LaunchdBackup) -> Result<(), PlatformError> {
        let mut commands = vec![format!(
            "/bin/mv {} {}",
            shell_quote(backup.disabled_path.as_os_str()),
            shell_quote(backup.original_path.as_os_str())
        )];
        if backup.was_loaded {
            commands.push(fixed_launchctl(
                "bootstrap",
                backup.domain,
                &backup.original_path,
            ));
        }
        if let Err(error) = privileged(&commands.join(" && "), &backup.disabled_path) {
            let rollback = privileged(
                &format!(
                    "/bin/mv {} {}",
                    shell_quote(backup.original_path.as_os_str()),
                    shell_quote(backup.disabled_path.as_os_str())
                ),
                &backup.original_path,
            );
            return Err(with_platform_rollback(error, rollback.err()));
        }
        Ok(())
    }

    fn launchctl(action: &str, domain: &str, plist: &Path) -> Result<(), PlatformError> {
        let output = Command::new("/bin/launchctl")
            .arg(action)
            .arg(domain)
            .arg(plist)
            .output()
            .map_err(|error| mutation(plist, &error.to_string()))?;
        command_result(output, plist)
    }

    fn privileged(shell_command: &str, path: &Path) -> Result<(), PlatformError> {
        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            apple_script_string(shell_command)
        );
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", &script])
            .output()
            .map_err(|error| PlatformError::HelperDenied(error.to_string()))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if detail.contains("User canceled") || detail.contains("(-128)") {
            Err(PlatformError::AccessDenied(detail))
        } else {
            Err(mutation(path, &detail))
        }
    }

    fn command_result(output: Output, path: &Path) -> Result<(), PlatformError> {
        if output.status.success() {
            Ok(())
        } else {
            Err(mutation(
                path,
                String::from_utf8_lossy(&output.stderr).trim(),
            ))
        }
    }

    fn fixed_launchctl(action: &str, domain: LaunchdDomain, plist: &Path) -> String {
        format!(
            "/bin/launchctl {} {} {}",
            action,
            launch_domain(domain),
            shell_quote(plist.as_os_str())
        )
    }

    fn launch_domain(domain: LaunchdDomain) -> String {
        match domain {
            LaunchdDomain::UserAgent | LaunchdDomain::SystemAgent => {
                format!("gui/{}", unsafe { libc::getuid() })
            }
            LaunchdDomain::SystemDaemon => "system".to_owned(),
        }
    }

    fn validate_plist_path(path: &Path, domain: LaunchdDomain) -> Result<(), PlatformError> {
        let canonical = canonical_existing(path)?;
        let expected = expected_root(domain)?;
        if canonical.parent() != Some(expected.as_path())
            || canonical.extension() != Some(OsStr::new("plist"))
        {
            return Err(PlatformError::ProtectedObject(path.display().to_string()));
        }
        Ok(())
    }

    fn validate_backup(backup: &LaunchdBackup) -> Result<(), PlatformError> {
        if protected_label(&backup.label) {
            return Err(PlatformError::ProtectedObject(backup.label.clone()));
        }
        let expected = expected_root(backup.domain)?;
        if backup.original_path.parent() != Some(expected.as_path())
            || backup.disabled_path.parent() != Some(expected.join("TuneUpDisabled").as_path())
            || backup.original_path.extension() != Some(OsStr::new("plist"))
            || backup.disabled_path.file_name().is_none()
        {
            return Err(PlatformError::ProtectedObject(
                backup.original_path.display().to_string(),
            ));
        }
        Ok(())
    }

    fn expected_root(domain: LaunchdDomain) -> Result<PathBuf, PlatformError> {
        let path = match domain {
            LaunchdDomain::UserAgent => std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join("Library/LaunchAgents"))
                .ok_or_else(|| PlatformError::AccessDenied("HOME не задан".to_owned()))?,
            LaunchdDomain::SystemAgent => PathBuf::from("/Library/LaunchAgents"),
            LaunchdDomain::SystemDaemon => PathBuf::from("/Library/LaunchDaemons"),
        };
        canonical_existing(&path)
    }

    fn shell_quote(value: &OsStr) -> String {
        let value = value.to_string_lossy();
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    fn apple_script_string(value: &str) -> String {
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\r', "\\r")
            .replace('\n', "\\n")
    }

    fn unique_destination(folder: &Path, name: &OsStr) -> PathBuf {
        let initial = folder.join(name);
        if !initial.exists() {
            return initial;
        }
        for index in 1..10_000 {
            let candidate = folder.join(format!("{}.{}", name.to_string_lossy(), index));
            if !candidate.exists() {
                return candidate;
            }
        }
        folder.join(format!("{}.{}", name.to_string_lossy(), std::process::id()))
    }

    fn mutation(path: &Path, detail: &str) -> PlatformError {
        PlatformError::Mutation {
            path: path.to_path_buf(),
            detail: detail.to_owned(),
        }
    }

    fn with_rollback(error: PlatformError, rollback: Option<std::io::Error>) -> PlatformError {
        match rollback {
            Some(rollback) => {
                PlatformError::Partial(format!("{error}; rollback также не выполнен: {rollback}"))
            }
            None => error,
        }
    }

    fn with_platform_rollback(
        error: PlatformError,
        rollback: Option<PlatformError>,
    ) -> PlatformError {
        match rollback {
            Some(rollback) => {
                PlatformError::Partial(format!("{error}; rollback также не выполнен: {rollback}"))
            }
            None => error,
        }
    }

    fn error_report(error: PlatformError) -> PlatformChangeReport {
        PlatformChangeReport {
            backup: PlatformBackup::default(),
            errors: vec![TuneupError::Platform(error)],
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn shell_quote_handles_single_quotes() {
            assert_eq!(
                shell_quote(OsStr::new("/tmp/a'b.plist")),
                "'/tmp/a'\\''b.plist'"
            );
        }

        #[test]
        fn apple_script_string_escapes_code_delimiters() {
            assert_eq!(apple_script_string("a\\b\"c"), "a\\\\b\\\"c");
        }
    }
}
