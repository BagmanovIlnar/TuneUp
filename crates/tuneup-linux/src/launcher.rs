//! Launches installed applications from their install root (Sleep Mode wake).

use std::path::{Path, PathBuf};

use tuneup_core::{TuneupError, error::PlatformError};

/// Starts a Linux application from an install root or desktop entry.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxAppLauncher;

impl LinuxAppLauncher {
    /// Launches the app that lives under `install_root`.
    ///
    /// Order:
    /// 1. Matching XDG `.desktop` via `gtk-launch` (preferred — correct Exec/Name)
    /// 2. Known binaries under `install_root/bin` (e.g. `phpstorm`, `phpstorm.sh`)
    /// 3. Any executable under `bin/` that is not a helper (`fsnotifier`, …)
    ///
    /// Never uses `xdg-open` on a directory — that opens the file manager.
    ///
    /// # Example
    ///
    /// ```ignore
    /// LinuxAppLauncher.launch(Path::new("/opt/PhpStorm-262.10315.130"), "PhpStorm")?;
    /// ```
    pub fn launch(&self, install_root: &Path, display_name: &str) -> Result<(), TuneupError> {
        #[cfg(target_os = "linux")]
        {
            if let Some(desktop_id) = find_desktop_id_for_root(install_root) {
                if try_command(Path::new("gtk-launch"), &[desktop_id.as_ref()]).is_ok() {
                    return Ok(());
                }
            }
            if let Some(exe) = find_launch_executable(install_root, display_name) {
                return try_command(&exe, &[]).map_err(|detail| {
                    PlatformError::Mutation {
                        path: exe,
                        detail,
                    }
                    .into()
                });
            }
            Err(PlatformError::Mutation {
                path: install_root.to_path_buf(),
                detail: "не удалось найти исполняемый файл приложения".to_owned(),
            }
            .into())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (install_root, display_name);
            Err(PlatformError::Unsupported(
                "запуск приложений доступен только в Linux".to_owned(),
            )
            .into())
        }
    }
}

impl tuneup_platform::AppLauncher for LinuxAppLauncher {
    fn launch(&self, install_root: &Path, display_name: &str) -> Result<(), TuneupError> {
        LinuxAppLauncher::launch(self, install_root, display_name)
    }
}

#[cfg(target_os = "linux")]
fn try_command(program: &Path, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    use std::process::{Command, Stdio};

    // Detach: IDEs stay up after TuneUp's wait would otherwise block UI briefly.
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    // gtk-launch / scripts usually exit quickly after spawning the real app.
    match child.try_wait() {
        Ok(Some(status)) if !status.success() => Err(format!("код выхода {:?}", status.code())),
        Ok(_) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn find_desktop_id_for_root(install_root: &Path) -> Option<String> {
    use std::fs;

    let root = normalize(install_root);
    for directory in application_data_dirs() {
        let Ok(entries) = fs::read_dir(directory) else {
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
            let Some(exec) = desktop_exec(&bytes) else {
                continue;
            };
            let Some(binary) = first_exec_token(&exec) else {
                continue;
            };
            let binary_path = PathBuf::from(&binary);
            if !binary_path.is_absolute() {
                continue;
            }
            let resolved = fs::canonicalize(&binary_path).unwrap_or(binary_path);
            if path_is_within(&resolved, Path::new(&root)) {
                return path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.trim_end_matches(".desktop").to_owned());
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn desktop_exec(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut in_desktop = false;
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
        if key.trim() == "Exec" && !value.trim().is_empty() {
            return Some(value.trim().to_owned());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn first_exec_token(exec: &str) -> Option<String> {
    let mut tokens = Vec::new();
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
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
            continue;
        }
        if character == '%' {
            break;
        }
        token.push(character);
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    let mut index = 0;
    while index < tokens.len() {
        let current = tokens[index].as_str();
        if current == "env" {
            index += 1;
            while index < tokens.len()
                && tokens[index].contains('=')
                && !tokens[index].starts_with('/')
            {
                index += 1;
            }
            continue;
        }
        if current.contains('=') && !current.starts_with('/') {
            index += 1;
            continue;
        }
        if current == "flatpak" || current == "snap" || current == "nice" || current == "nohup" {
            index += 1;
            continue;
        }
        return Some(tokens[index].clone());
    }
    None
}

#[cfg(target_os = "linux")]
fn find_launch_executable(root: &Path, display_name: &str) -> Option<PathBuf> {
    use std::fs;

    if root.is_file() {
        return Some(root.to_path_buf());
    }
    let bin = root.join("bin");
    if !bin.is_dir() {
        return None;
    }
    let candidates = preferred_binary_names(root, display_name);
    for name in &candidates {
        let path = bin.join(name);
        if is_executable_file(&path) {
            return Some(path);
        }
    }
    let mut fallback = None;
    if let Ok(entries) = fs::read_dir(&bin) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_executable_file(&path) {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if is_helper_binary(&name) {
                continue;
            }
            // Prefer names without extension, then `.sh`.
            if !name.contains('.') {
                return Some(path);
            }
            if fallback.is_none() && name.ends_with(".sh") {
                fallback = Some(path);
            }
        }
    }
    fallback
}

#[cfg(target_os = "linux")]
fn preferred_binary_names(root: &Path, display_name: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut push = |value: &str| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return;
        }
        let lower = trimmed.to_ascii_lowercase().replace(' ', "");
        if !names.iter().any(|existing| existing == &lower) {
            names.push(lower.clone());
        }
        let script = format!("{lower}.sh");
        if !names.iter().any(|existing| existing == &script) {
            names.push(script);
        }
    };
    push(display_name);
    if let Some(folder) = root.file_name().and_then(|value| value.to_str()) {
        // PhpStorm-262.10315.130 → phpstorm
        let product = folder
            .split_once('-')
            .filter(|(_, rest)| rest.starts_with(|c: char| c.is_ascii_digit()))
            .map(|(product, _)| product)
            .unwrap_or(folder);
        push(product);
    }
    names
}

#[cfg(target_os = "linux")]
fn is_helper_binary(name: &str) -> bool {
    const HELPERS: &[&str] = &[
        "fsnotifier",
        "restarter",
        "remote-dev-server",
        "inspect.sh",
        "format.sh",
        "ltedit.sh",
        "brokenplugins.db",
    ];
    HELPERS.iter().any(|helper| name == *helper)
        || name.ends_with(".vmoptions")
        || name.ends_with(".properties")
        || name.ends_with(".png")
        || name.ends_with(".svg")
        || name.ends_with(".db")
}

#[cfg(target_os = "linux")]
fn is_executable_file(path: &Path) -> bool {
    use std::{fs, os::unix::fs::PermissionsExt};

    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(target_os = "linux")]
fn path_is_within(path: &Path, root: &Path) -> bool {
    let path = normalize(path);
    let root = normalize(root);
    path == root || path.starts_with(&(root + "/"))
}

#[cfg(target_os = "linux")]
fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn prefers_product_binary_over_fsnotifier() {
        let dir = tempfile_bin_dir();
        fs::write(dir.join("fsnotifier"), b"x").unwrap();
        set_exec(&dir.join("fsnotifier"));
        fs::write(dir.join("phpstorm"), b"x").unwrap();
        set_exec(&dir.join("phpstorm"));
        let root = dir.parent().unwrap();
        let exe = find_launch_executable(root, "PhpStorm").expect("phpstorm");
        assert_eq!(exe.file_name().unwrap(), "phpstorm");
    }

    #[test]
    fn preferred_names_strip_jetbrains_version() {
        let names = preferred_binary_names(
            Path::new("/opt/PhpStorm-262.10315.130"),
            "PhpStorm",
        );
        assert!(names.iter().any(|name| name == "phpstorm"));
        assert!(names.iter().any(|name| name == "phpstorm.sh"));
    }

    #[test]
    fn parses_quoted_desktop_exec() {
        assert_eq!(
            first_exec_token(r#""/opt/PhpStorm-262.10315.130/bin/phpstorm" %f"#),
            Some("/opt/PhpStorm-262.10315.130/bin/phpstorm".into())
        );
        assert_eq!(
            first_exec_token("env NO_AT_BRIDGE=1 /usr/share/dbeaver-ce/dbeaver %U"),
            Some("/usr/share/dbeaver-ce/dbeaver".into())
        );
    }

    fn tempfile_bin_dir() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "tuneup-launcher-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        bin
    }

    fn set_exec(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}
