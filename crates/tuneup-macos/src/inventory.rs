use tuneup_core::{TuneupError, model::InventorySnapshot};

/// Collects installed application bundles and launchd jobs on macOS.
pub struct MacOsInventoryProvider;

impl MacOsInventoryProvider {
    /// Builds a best-effort inventory. Unreadable roots and malformed plists are
    /// recorded in `warnings`; one bad object does not discard other records.
    pub fn snapshot(&self) -> Result<InventorySnapshot, TuneupError> {
        imp::snapshot()
    }
}

impl tuneup_platform::InventoryProvider for MacOsInventoryProvider {
    fn snapshot(&self) -> Result<InventorySnapshot, TuneupError> {
        MacOsInventoryProvider::snapshot(self)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::*;

    pub fn snapshot() -> Result<InventorySnapshot, TuneupError> {
        Ok(InventorySnapshot::default())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{
        collections::HashSet,
        fs,
        path::{Path, PathBuf},
        process::Command,
    };

    use tuneup_core::model::{
        AutoStartEntry, ExecutableRef, InstallRecord, InstallSource, LaunchdDomain, LoginItemKind,
    };

    use super::*;
    use crate::path_util::{protected_label, protected_path};

    pub fn snapshot() -> Result<InventorySnapshot, TuneupError> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut snapshot = InventorySnapshot::default();
        let mut seen_apps = HashSet::new();
        for root in [
            Some(PathBuf::from("/Applications")),
            Some(PathBuf::from("/System/Applications")),
            home.as_ref().map(|path| path.join("Applications")),
        ]
        .into_iter()
        .flatten()
        {
            scan_apps(
                &root,
                &mut seen_apps,
                &mut snapshot.installs,
                &mut snapshot.warnings,
            );
        }

        for (domain, root) in [
            (
                LaunchdDomain::UserAgent,
                home.as_ref().map(|path| path.join("Library/LaunchAgents")),
            ),
            (
                LaunchdDomain::SystemAgent,
                Some(PathBuf::from("/Library/LaunchAgents")),
            ),
            (
                LaunchdDomain::SystemDaemon,
                Some(PathBuf::from("/Library/LaunchDaemons")),
            ),
        ] {
            let Some(root) = root else { continue };
            scan_launchd(
                &root,
                domain,
                &mut snapshot.autostart,
                &mut snapshot.warnings,
            );
        }
        scan_login_items(&mut snapshot.autostart, &mut snapshot.warnings);
        Ok(snapshot)
    }

    fn scan_login_items(entries: &mut Vec<AutoStartEntry>, warnings: &mut Vec<String>) {
        // Use application id so AppleScript does not show "Choose Application"
        // when resolving "System Events" by display name.
        let script = r#"tell application id "com.apple.systemevents"
set resultText to ""
repeat with loginEntry in every login item
set resultText to resultText & (name of loginEntry) & tab & (path of loginEntry) & linefeed
end repeat
return resultText
end tell"#;
        let output = match Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .output()
        {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                warnings.push(format!(
                    "Login Items: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
                return;
            }
            Err(error) => {
                warnings.push(format!("Login Items: {error}"));
                return;
            }
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let Some((name, raw_path)) = line.split_once('\t') else {
                continue;
            };
            let path = PathBuf::from(raw_path.trim());
            if !path.is_absolute() {
                continue;
            }
            entries.push(AutoStartEntry::LoginItem {
                kind: LoginItemKind::Legacy,
                identifier: name.trim().to_owned(),
                display_name: Some(name.trim().to_owned()),
                target: ExecutableRef {
                    raw: raw_path.trim().to_owned(),
                    resolved_path: fs::canonicalize(&path).ok().or(Some(path)),
                },
                mutable: true,
            });
        }
    }

    fn scan_apps(
        root: &Path,
        seen: &mut HashSet<PathBuf>,
        records: &mut Vec<InstallRecord>,
        warnings: &mut Vec<String>,
    ) {
        let Ok(children) = fs::read_dir(root) else {
            return;
        };
        for child in children.filter_map(Result::ok) {
            let path = child.path();
            if path.extension().is_some_and(|extension| extension == "app") {
                match app_record(&path) {
                    Ok(record) if seen.insert(record.install_root.clone()) => records.push(record),
                    Ok(_) => {}
                    Err(error) if !path.starts_with("/System") => warnings.push(error),
                    Err(_) => {}
                }
            } else if child
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            {
                scan_apps(&path, seen, records, warnings);
            }
        }
    }

    fn app_record(path: &Path) -> Result<InstallRecord, String> {
        let root =
            fs::canonicalize(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let info = read_plist_dictionary(&root.join("Contents/Info.plist"))
            .ok_or_else(|| format!("{}: Info.plist не читается", path.display()))?;
        let fallback = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Application")
            .to_owned();
        let id = dictionary_string(&info, "CFBundleIdentifier")
            .unwrap_or_else(|| root.display().to_string());
        let bundle_name = dictionary_string(&info, "CFBundleDisplayName")
            .or_else(|| dictionary_string(&info, "CFBundleName"));
        // Prefer the Finder/.app name when the bundle label is a short Electron
        // stub such as "Code" — users search for "Visual Studio Code".
        let display_name = match bundle_name {
            Some(name) if is_generic_bundle_label(&name) && fallback.len() > name.len() => {
                Some(fallback.clone())
            }
            Some(name) => Some(name),
            None => Some(fallback.clone()),
        };
        Ok(InstallRecord {
            source: InstallSource::MacOsBundle,
            id,
            display_name,
            install_root: root,
            version: dictionary_string(&info, "CFBundleShortVersionString")
                .or_else(|| dictionary_string(&info, "CFBundleVersion")),
            publisher: None,
        })
    }

    fn is_generic_bundle_label(name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "code" | "electron" | "helper" | "app" | "application"
        )
    }

    fn scan_launchd(
        root: &Path,
        domain: LaunchdDomain,
        entries: &mut Vec<AutoStartEntry>,
        warnings: &mut Vec<String>,
    ) {
        let Ok(children) = fs::read_dir(root) else {
            return;
        };
        for child in children.filter_map(Result::ok) {
            let path = child.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "plist")
            {
                continue;
            }
            match launchd_entry_from_path(&path, domain) {
                Ok(entry) => entries.push(entry),
                Err(error) => warnings.push(error),
            }
        }
    }

    fn launchd_entry_from_path(
        plist_path: &Path,
        domain: LaunchdDomain,
    ) -> Result<AutoStartEntry, String> {
        let parsed = parse_launchd_plist_path(plist_path)?;
        launchd_entry(plist_path, domain, parsed)
    }

    fn launchd_entry(
        plist_path: &Path,
        domain: LaunchdDomain,
        parsed: ParsedLaunchd,
    ) -> Result<AutoStartEntry, String> {
        let resolved_path = resolve_program(&parsed.program);
        let protected =
            protected_label(&parsed.label) || resolved_path.as_deref().is_some_and(protected_path);
        Ok(AutoStartEntry::Launchd {
            domain,
            loaded: launchd_loaded(domain, &parsed.label),
            protected,
            label: parsed.label,
            plist_path: plist_path.to_path_buf(),
            target: ExecutableRef {
                raw: parsed.program,
                resolved_path,
            },
        })
    }

    fn resolve_program(raw: &str) -> Option<PathBuf> {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return None;
        }
        fs::canonicalize(&path).ok().or(Some(path))
    }

    fn launchd_loaded(domain: LaunchdDomain, label: &str) -> bool {
        let target = match domain {
            LaunchdDomain::UserAgent | LaunchdDomain::SystemAgent => {
                format!("gui/{}/{}", unsafe { libc::getuid() }, label)
            }
            LaunchdDomain::SystemDaemon => format!("system/{label}"),
        };
        Command::new("/bin/launchctl")
            .args(["print", &target])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn parse_launchd_plist_path(path: &Path) -> Result<ParsedLaunchd, String> {
        let dictionary = read_plist_dictionary(path)
            .ok_or_else(|| format!("{}: launchd plist не читается", path.display()))?;
        parsed_from_dictionary(&dictionary)
            .map_err(|detail| format!("{}: {detail}", path.display()))
    }

    fn parsed_from_dictionary(dictionary: &plist::Dictionary) -> Result<ParsedLaunchd, String> {
        let label = dictionary_string(dictionary, "Label")
            .ok_or_else(|| "launchd plist не содержит Label".to_owned())?;
        let program = dictionary_string(dictionary, "Program")
            .or_else(|| program_arguments_zero(dictionary))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("launchd {label} не содержит Program"))?;
        Ok(ParsedLaunchd { label, program })
    }

    fn program_arguments_zero(dictionary: &plist::Dictionary) -> Option<String> {
        dictionary
            .get("ProgramArguments")
            .and_then(plist::Value::as_array)
            .and_then(|arguments| arguments.first())
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
    }

    /// Reads a plist (XML or binary) without spawning `plutil`.
    fn read_plist_dictionary(path: &Path) -> Option<plist::Dictionary> {
        plist::Value::from_file(path).ok()?.into_dictionary()
    }

    fn dictionary_string(dictionary: &plist::Dictionary, key: &str) -> Option<String> {
        dictionary
            .get(key)
            .and_then(plist::Value::as_string)
            .map(str::to_owned)
            .filter(|value| !value.is_empty())
    }

    struct ParsedLaunchd {
        label: String,
        program: String,
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn dictionary(entries: Vec<(&str, plist::Value)>) -> plist::Dictionary {
            let mut dictionary = plist::Dictionary::new();
            for (key, value) in entries {
                dictionary.insert(key.to_owned(), value);
            }
            dictionary
        }

        fn text(value: &str) -> plist::Value {
            plist::Value::from(value)
        }

        #[test]
        fn parses_program_arguments_fallback() {
            let parsed = parsed_from_dictionary(&dictionary(vec![
                ("Label", text("com.example.worker")),
                (
                    "ProgramArguments",
                    plist::Value::Array(vec![
                        text("/Applications/Demo.app/Contents/MacOS/Demo"),
                        text("--quiet"),
                    ]),
                ),
            ]))
            .expect("parse launchd fixture");
            assert_eq!(parsed.label, "com.example.worker");
            assert_eq!(parsed.program, "/Applications/Demo.app/Contents/MacOS/Demo");
        }

        #[test]
        fn program_takes_precedence_over_arguments() {
            let parsed = parsed_from_dictionary(&dictionary(vec![
                ("Label", text("com.example.worker")),
                ("Program", text("/opt/example/worker")),
                (
                    "ProgramArguments",
                    plist::Value::Array(vec![text("/wrong")]),
                ),
            ]))
            .expect("parse launchd fixture");
            assert_eq!(parsed.program, "/opt/example/worker");
        }

        #[test]
        fn rejects_missing_label() {
            let parsed = parsed_from_dictionary(&dictionary(vec![("Program", text("/bin/echo"))]));
            assert!(parsed.is_err());
        }

        #[test]
        fn reads_real_binary_plist_without_plutil() {
            // System bundles ship binary Info.plist — prove the native reader works.
            let path = PathBuf::from("/System/Applications/Stocks.app/Contents/Info.plist");
            if !path.exists() {
                return;
            }
            let info = read_plist_dictionary(&path).expect("binary Info.plist must parse natively");
            assert!(dictionary_string(&info, "CFBundleIdentifier").is_some());
        }

        #[test]
        fn stocks_system_bundle_does_not_require_json_plist() {
            let path = PathBuf::from("/System/Applications/Stocks.app");
            if !path.exists() {
                return;
            }
            let record = app_record(&path).expect("system bundle must use string key extraction");
            assert_eq!(record.install_root, path);
            assert!(
                record
                    .display_name
                    .as_deref()
                    .is_some_and(|name| !name.is_empty())
            );
        }
    }
}
