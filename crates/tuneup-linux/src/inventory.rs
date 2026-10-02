//! Linux package and autostart inventory.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use tuneup_core::{
    TuneupError,
    model::{
        AutoStartEntry, AutostartScope, ExecutableRef, InstallRecord, InstallSource,
        InventorySnapshot, XdgAutostartLocation,
    },
};

/// Collects DEB/RPM packages, XDG autostart files, and systemd units.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxInventoryProvider;

impl LinuxInventoryProvider {
    /// Creates a best-effort inventory snapshot.
    ///
    /// A missing package manager, inaccessible directory, or unavailable
    /// systemd user bus is recorded in `warnings`; other inventory sources are
    /// still returned.
    pub fn snapshot(&self) -> Result<InventorySnapshot, TuneupError> {
        imp::snapshot()
    }
}

impl tuneup_platform::InventoryProvider for LinuxInventoryProvider {
    fn snapshot(&self) -> Result<InventorySnapshot, TuneupError> {
        LinuxInventoryProvider::snapshot(self)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DesktopData {
    name: String,
    exec: String,
    hidden: bool,
    no_display: bool,
    is_application: bool,
}

fn parse_desktop(bytes: &[u8]) -> Result<DesktopData, String> {
    let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let mut in_desktop = false;
    let mut name = None;
    let mut localized_name = None;
    let mut exec = None;
    let mut hidden = false;
    let mut no_display = false;
    let mut is_application = true;
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
            "Type" => {
                is_application = value.eq_ignore_ascii_case("Application");
            }
            "Name" => {
                if !value.is_empty() {
                    name = Some(value.to_owned());
                }
            }
            key if key.starts_with("Name[") && key.ends_with(']') => {
                if localized_name.is_none() && !value.is_empty() {
                    localized_name = Some(value.to_owned());
                }
            }
            "Exec" => {
                if !value.is_empty() {
                    exec = Some(value.to_owned());
                }
            }
            "Hidden" => {
                if value.eq_ignore_ascii_case("true") {
                    hidden = true;
                }
            }
            "NoDisplay" => {
                if value.eq_ignore_ascii_case("true") {
                    no_display = true;
                }
            }
            _ => {}
        }
    }
    Ok(DesktopData {
        name: name.or(localized_name).unwrap_or_default(),
        exec: exec.ok_or_else(|| "нет Exec в [Desktop Entry]".to_owned())?,
        hidden,
        no_display,
        is_application,
    })
}

/// Resolves a product install root from a launcher binary.
///
/// Examples:
/// - `/opt/PhpStorm-262.10315.130/bin/phpstorm` → `/opt/PhpStorm-262.10315.130`
/// - `/usr/share/dbeaver-ce/dbeaver` → `/usr/share/dbeaver-ce`
///
/// System PATH binaries (`/usr/bin/...`) and overly broad roots (`~/.local`) are skipped.
fn desktop_install_root(exec_path: &Path) -> Option<PathBuf> {
    let parent = exec_path.parent()?;
    if is_shared_binary_directory(parent) {
        return None;
    }
    let root = if parent
        .file_name()
        .is_some_and(|name| name == "bin" || name == "sbin")
    {
        parent.parent()?.to_path_buf()
    } else {
        parent.to_path_buf()
    };
    (!is_overly_broad_install_root(&root)).then_some(root)
}

fn is_shared_binary_directory(path: &Path) -> bool {
    [
        Path::new("/bin"),
        Path::new("/sbin"),
        Path::new("/usr/bin"),
        Path::new("/usr/sbin"),
        Path::new("/usr/local/bin"),
        Path::new("/snap/bin"),
    ]
    .contains(&path)
}

/// Roots that must never own unrelated apps (e.g. JetBrains under `~/.local/share`).
fn is_overly_broad_install_root(path: &Path) -> bool {
    const ABSOLUTE: &[&str] = &[
        "/",
        "/usr",
        "/usr/local",
        "/usr/share",
        "/usr/lib",
        "/usr/lib64",
        "/opt",
        "/home",
        "/var",
        "/var/lib",
    ];
    if ABSOLUTE.iter().any(|candidate| path == Path::new(candidate)) {
        return true;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    [
        home.clone(),
        home.join(".local"),
        home.join(".local/share"),
        home.join(".local/bin"),
        home.join(".config"),
        home.join(".cache"),
    ]
    .iter()
    .any(|candidate| candidate == path)
}

fn application_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local/share/applications"));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        dirs.push(data_home.join("applications"));
    }
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    for dir in data_dirs.split(':').filter(|value| !value.is_empty()) {
        dirs.push(PathBuf::from(dir).join("applications"));
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn desktop_exec_path(exec: &str) -> Option<PathBuf> {
    let token = desktop_exec_binary_token(exec)?;
    let path = PathBuf::from(&token);
    if path.is_absolute() {
        return std::fs::canonicalize(&path).ok().or(Some(path));
    }
    if path.components().count() != 1 {
        return None;
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .map(|directory| directory.join(&path))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| std::fs::canonicalize(candidate).ok())
}

/// Picks the real binary from an Exec= line, skipping wrappers like `env VAR=1`.
///
/// Example: `env NO_AT_BRIDGE=1 /usr/share/dbeaver-ce/dbeaver %U` → dbeaver path.
/// Without this, the first token `env` resolves to `~/.local/bin/env` and the
/// install root collapses to `~/.local`, swallowing JetBrains Daemon processes.
fn desktop_exec_binary_token(exec: &str) -> Option<String> {
    let tokens = split_command_line(exec);
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if token.is_empty() || token.starts_with('%') {
            break;
        }
        if token == "env" {
            index += 1;
            while index < tokens.len()
                && tokens[index].contains('=')
                && !tokens[index].starts_with('/')
                && !tokens[index].starts_with('%')
            {
                index += 1;
            }
            continue;
        }
        if token.contains('=') && !token.starts_with('/') {
            index += 1;
            continue;
        }
        if token == "flatpak" || token == "snap" || token == "nice" || token == "nohup" {
            index += 1;
            continue;
        }
        return Some(tokens[index].clone());
    }
    None
}

fn split_command_line(value: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push(character);
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
                current.push(character);
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn parse_packages(output: &str, source: InstallSource, default_root: &Path) -> Vec<InstallRecord> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next()?.trim();
            let version = fields
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let publisher = fields
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let root = fields
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if id.is_empty() {
                return None;
            }
            Some(InstallRecord {
                source,
                id: id.to_owned(),
                display_name: Some(id.to_owned()),
                install_root: root
                    .map(PathBuf::from)
                    .unwrap_or_else(|| default_root.to_path_buf()),
                version: version.map(str::to_owned),
                publisher: publisher.map(str::to_owned),
            })
        })
        .collect()
}

fn parse_unit_file_list(output: &str) -> Vec<String> {
    let mut units = output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let unit = fields.next()?;
            let state = fields.next()?;
            (state.starts_with("enabled") && valid_unit_name(unit)).then(|| unit.to_owned())
        })
        .collect::<Vec<_>>();
    units.sort();
    units.dedup();
    units
}

fn valid_unit_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.starts_with('-')
        && value.ends_with(".service")
        && !is_uninstantiated_template(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.@:-\\".contains(&byte))
}

/// Template units like `getty@.service` cannot be queried with `systemctl show`
/// until an instance name is present (`getty@tty1.service`).
fn is_uninstantiated_template(name: &str) -> bool {
    match name.rfind('@') {
        Some(at) => name[at + 1..].starts_with('.'),
        None => false,
    }
}

fn parse_systemd_show(output: &str, scope: AutostartScope) -> Result<AutoStartEntry, String> {
    let mut id = None;
    let mut unit_state = None;
    let mut active_state = None;
    let mut exec_start = None;
    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Id" => id = Some(value.to_owned()),
            "UnitFileState" => unit_state = Some(value.to_owned()),
            "ActiveState" => active_state = Some(value.to_owned()),
            "ExecStart" => exec_start = extract_systemd_exec(value),
            _ => {}
        }
    }
    let unit_name = id.ok_or_else(|| "systemctl show не вернул Id".to_owned())?;
    if !valid_unit_name(&unit_name) {
        return Err(format!("небезопасное имя unit: {unit_name}"));
    }
    let protected = protected_unit(&unit_name);
    let raw = exec_start
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    Ok(AutoStartEntry::SystemdUnit {
        scope,
        unit_name,
        enabled: unit_state
            .as_deref()
            .is_some_and(|state| state.starts_with("enabled")),
        active: active_state.as_deref() == Some("active"),
        protected,
        target: ExecutableRef {
            raw,
            resolved_path: exec_start,
        },
    })
}

fn extract_systemd_exec(value: &str) -> Option<PathBuf> {
    let marker = "path=";
    let start = value.find(marker)? + marker.len();
    let rest = value[start..].trim_start();
    let end = rest
        .find(|character: char| character == ';' || character.is_whitespace())
        .unwrap_or(rest.len());
    let path = PathBuf::from(&rest[..end]);
    path.is_absolute().then_some(path)
}

fn protected_unit(name: &str) -> bool {
    const PROTECTED: &[&str] = &[
        "dbus.service",
        "systemd-logind.service",
        "systemd-udevd.service",
        "systemd-journald.service",
        "NetworkManager.service",
        "polkit.service",
        "sshd.service",
        "display-manager.service",
    ];
    PROTECTED.contains(&name)
        || name.starts_with("systemd-")
        || name.starts_with("user@")
        || name.starts_with("getty@")
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub fn snapshot() -> Result<InventorySnapshot, TuneupError> {
        Ok(InventorySnapshot {
            warnings: vec!["Linux inventory недоступен на этой платформе".to_owned()],
            ..InventorySnapshot::default()
        })
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{fs, process::Command};

    use super::*;

    const DPKG_ARGS: &[&str] = &[
        "-W",
        "-f=${binary:Package}\t${Version}\t${Maintainer}\t/usr\n",
    ];
    const RPM_ARGS: &[&str] = &[
        "-qa",
        "--qf",
        "%{NAME}\t%{VERSION}-%{RELEASE}\t%{VENDOR}\t%{PREFIXES}\n",
    ];
    const LIST_ARGS: &[&str] = &[
        "list-unit-files",
        "--type=service",
        "--state=enabled",
        "--no-legend",
        "--no-pager",
    ];

    pub fn snapshot() -> Result<InventorySnapshot, TuneupError> {
        let mut result = InventorySnapshot::default();
        scan_packages(&mut result);
        scan_desktop_applications(&mut result);
        scan_xdg(&mut result);
        scan_systemd(&mut result, AutostartScope::User);
        scan_systemd(&mut result, AutostartScope::System);
        Ok(result)
    }

    /// Seeds Sleep Mode groups from XDG application launchers (Name=PhpStorm).
    ///
    /// Tarball IDEs under `/opt` are invisible to dpkg/rpm; without this they only
    /// appear while running, and the group title falls back to a folder/thread name.
    fn scan_desktop_applications(snapshot: &mut InventorySnapshot) {
        let mut seen_roots = HashSet::new();
        for directory in application_data_dirs() {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("desktop") {
                    continue;
                }
                let Ok(bytes) = fs::read(&path) else {
                    continue;
                };
                let Ok(desktop) = parse_desktop(&bytes) else {
                    continue;
                };
                if !desktop.is_application || desktop.hidden || desktop.no_display {
                    continue;
                }
                if desktop.name.is_empty() {
                    continue;
                }
                let exec_lower = desktop.exec.to_ascii_lowercase();
                if exec_lower.contains("flatpak ")
                    || exec_lower.starts_with("flatpak")
                    || exec_lower.contains("/snap/")
                {
                    continue;
                }
                let Some(exec_path) = desktop_exec_path(&desktop.exec) else {
                    continue;
                };
                let Some(install_root) = desktop_install_root(&exec_path) else {
                    continue;
                };
                let root_key = install_root.to_string_lossy().to_ascii_lowercase();
                if !seen_roots.insert(root_key) {
                    continue;
                }
                let id = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(desktop.name.as_str())
                    .to_owned();
                snapshot.installs.push(InstallRecord {
                    source: InstallSource::XdgDesktop,
                    id,
                    display_name: Some(desktop.name),
                    install_root,
                    version: None,
                    publisher: None,
                });
            }
        }
    }

    fn scan_packages(snapshot: &mut InventorySnapshot) {
        match optional_fixed_output("dpkg-query", DPKG_ARGS) {
            Ok(Some(output)) => snapshot.installs.extend(parse_packages(
                &output,
                InstallSource::Deb,
                Path::new("/usr"),
            )),
            Ok(None) => {}
            Err(error) => snapshot.warnings.push(format!("dpkg-query: {error}")),
        }
        match optional_fixed_output("rpm", RPM_ARGS) {
            Ok(Some(output)) => snapshot.installs.extend(parse_packages(
                &output,
                InstallSource::Rpm,
                Path::new("/usr"),
            )),
            Ok(None) => {}
            Err(error) => snapshot.warnings.push(format!("rpm: {error}")),
        }
    }

    fn scan_xdg(snapshot: &mut InventorySnapshot) {
        let mut locations = Vec::new();
        if let Some(home) = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .filter(|path| path.is_absolute())
        {
            locations.push((XdgAutostartLocation::UserConfig, home.join("autostart")));
        }
        let dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".to_owned());
        locations.extend(
            dirs.split(':')
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .map(|path| (XdgAutostartLocation::SystemConfig, path.join("autostart"))),
        );
        let mut seen = HashSet::new();
        for (location, directory) in locations {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("desktop")
                    || !seen.insert(path.clone())
                {
                    continue;
                }
                match fs::read(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|bytes| parse_desktop(&bytes).map(|desktop| (bytes, desktop)))
                {
                    Ok((_, desktop)) => {
                        let name = if desktop.name.is_empty() {
                            path.file_stem()
                                .and_then(|stem| stem.to_str())
                                .unwrap_or("unknown")
                                .to_owned()
                        } else {
                            desktop.name
                        };
                        snapshot.autostart.push(AutoStartEntry::XdgDesktop {
                            location,
                            desktop_path: path,
                            name,
                            hidden: desktop.hidden,
                            target: ExecutableRef {
                                resolved_path: desktop_exec_path(&desktop.exec),
                                raw: desktop.exec,
                            },
                        });
                    }
                    // Broken desktop files are common; skip without flooding the UI.
                    Err(_) => {}
                }
            }
        }
    }

    fn scan_systemd(snapshot: &mut InventorySnapshot, scope: AutostartScope) {
        let mut command = Command::new("systemctl");
        if scope == AutostartScope::User {
            command.arg("--user");
        }
        let output = command.args(LIST_ARGS).output();
        let output = match output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).into_owned()
            }
            Ok(output) => {
                snapshot.warnings.push(format!(
                    "systemctl {:?}: {}",
                    scope,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
                return;
            }
            Err(error) => {
                snapshot
                    .warnings
                    .push(format!("systemctl {:?}: {error}", scope));
                return;
            }
        };
        for unit in parse_unit_file_list(&output) {
            let mut command = Command::new("systemctl");
            if scope == AutostartScope::User {
                command.arg("--user");
            }
            let shown = command
                .args([
                    "show",
                    "--no-pager",
                    "--property=Id,UnitFileState,ActiveState,ExecStart",
                    "--",
                    &unit,
                ])
                .output();
            match shown {
                Ok(value) if value.status.success() => {
                    match parse_systemd_show(&String::from_utf8_lossy(&value.stdout), scope) {
                        Ok(entry) => snapshot.autostart.push(entry),
                        Err(error) => snapshot.warnings.push(error),
                    }
                }
                Ok(value) => snapshot.warnings.push(format!(
                    "systemctl show {unit}: {}",
                    String::from_utf8_lossy(&value.stderr).trim()
                )),
                Err(error) => snapshot
                    .warnings
                    .push(format!("systemctl show {unit}: {error}")),
            }
        }
    }

    /// Runs a package-manager query. Missing binaries (`rpm` on Debian, etc.)
    /// return `Ok(None)` so the other manager can still contribute installs.
    fn optional_fixed_output(program: &str, args: &[&str]) -> Result<Option<String>, String> {
        let output = match Command::new(program).args(args).output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_desktop_entry_and_quoted_exec() {
        let parsed = parse_desktop(
            b"[Desktop Entry]\nName=Example\nExec=\"/opt/Example App/bin/app\" --quiet\nHidden=false\n",
        )
        .unwrap();
        assert_eq!(parsed.name, "Example");
        assert!(!parsed.hidden);
        assert!(!parsed.no_display);
        assert!(parsed.is_application);
        assert_eq!(
            desktop_exec_path(&parsed.exec),
            Some(PathBuf::from("/opt/Example App/bin/app"))
        );
    }

    #[test]
    fn desktop_install_root_lifts_jetbrains_bin() {
        assert_eq!(
            desktop_install_root(Path::new("/opt/PhpStorm-262.10315.130/bin/phpstorm")),
            Some(PathBuf::from("/opt/PhpStorm-262.10315.130"))
        );
        assert_eq!(desktop_install_root(Path::new("/usr/bin/firefox")), None);
        assert_eq!(
            desktop_install_root(Path::new("/usr/share/dbeaver-ce/dbeaver")),
            Some(PathBuf::from("/usr/share/dbeaver-ce"))
        );
    }

    #[test]
    fn skips_env_wrapper_in_desktop_exec() {
        assert_eq!(
            desktop_exec_binary_token("env NO_AT_BRIDGE=1 /usr/share/dbeaver-ce/dbeaver %U"),
            Some("/usr/share/dbeaver-ce/dbeaver".into())
        );
        assert_eq!(
            desktop_exec_path("env NO_AT_BRIDGE=1 /usr/share/dbeaver-ce/dbeaver %U"),
            Some(PathBuf::from("/usr/share/dbeaver-ce/dbeaver"))
        );
    }

    #[test]
    fn rejects_broad_local_install_root() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        if !home.as_os_str().is_empty() {
            assert!(is_overly_broad_install_root(&home.join(".local")));
            assert!(is_overly_broad_install_root(
                &home.join(".local").join("share")
            ));
        }
        assert!(is_overly_broad_install_root(Path::new("/usr/share")));
    }

    #[test]
    fn accepts_localized_name_and_missing_plain_name() {
        let parsed = parse_desktop(
            b"[Desktop Entry]\nType=Application\nName[en_US]=Example Localized\nExec=rustdesk --tray\n",
        )
        .unwrap();
        assert_eq!(parsed.name, "Example Localized");
        assert_eq!(parsed.exec, "rustdesk --tray");
    }

    #[test]
    fn accepts_desktop_without_name_when_exec_present() {
        let parsed =
            parse_desktop(b"[Desktop Entry]\nType=Application\nExec=rustdesk --tray\n").unwrap();
        assert!(parsed.name.is_empty());
        assert_eq!(parsed.exec, "rustdesk --tray");
    }

    #[test]
    fn parses_enabled_units_only() {
        let units = parse_unit_file_list(
            "foo.service enabled enabled\nbar.timer enabled enabled\nbad.service disabled disabled\ngetty@.service enabled enabled\ngetty@tty1.service enabled enabled\n",
        );
        assert_eq!(units, vec!["foo.service", "getty@tty1.service"]);
    }

    #[test]
    fn rejects_uninstantiated_templates() {
        assert!(is_uninstantiated_template("getty@.service"));
        assert!(!is_uninstantiated_template("getty@tty1.service"));
        assert!(!is_uninstantiated_template("sshd.service"));
    }

    #[test]
    fn parses_systemd_exec_path() {
        assert_eq!(
            extract_systemd_exec("{ path=/opt/acme/bin/agent ; argv[]=/opt/acme/bin/agent ; }"),
            Some(PathBuf::from("/opt/acme/bin/agent"))
        );
    }
}
