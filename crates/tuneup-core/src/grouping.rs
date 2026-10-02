use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use crate::model::{
    AutoStartEntry, GroupPolicy, GroupStatus, InstallRecord, InventorySnapshot, ProcessGroup,
    ProcessInfo, UninstallRecord, normalize_path, path_is_within,
};

pub struct GroupResolver {
    uninstall: Vec<UninstallRecord>,
    installs: Vec<InstallRecord>,
}

impl GroupResolver {
    pub fn new(mut uninstall: Vec<UninstallRecord>, mut installs: Vec<InstallRecord>) -> Self {
        uninstall.sort_by_key(|record| {
            std::cmp::Reverse(
                record
                    .install_location
                    .as_ref()
                    .map(|path| normalize_path(path).len())
                    .unwrap_or_default(),
            )
        });
        installs.retain(|record| !is_shared_install_root(&record.install_root));
        installs
            .sort_by_key(|record| std::cmp::Reverse(normalize_path(&record.install_root).len()));
        Self {
            uninstall,
            installs,
        }
    }

    pub fn resolve(
        &self,
        processes: &[ProcessInfo],
        inventory: &InventorySnapshot,
    ) -> Vec<ProcessGroup> {
        let by_pid = processes
            .iter()
            .map(|process| (process.pid, process))
            .collect::<HashMap<_, _>>();
        let mut assigned = HashMap::<u32, PathBuf>::new();
        for process in processes {
            if let Some(root) = self.root_for_process(process) {
                assigned.insert(process.pid, root);
            }
        }
        for process in processes {
            if assigned.contains_key(&process.pid) {
                continue;
            }
            let mut parent = process.parent_pid;
            while let Some(parent_pid) = parent {
                if let Some(root) = assigned.get(&parent_pid) {
                    assigned.insert(process.pid, root.clone());
                    break;
                }
                parent = by_pid
                    .get(&parent_pid)
                    .and_then(|process| process.parent_pid);
            }
        }

        // Attach helpers that live outside the .app (Application Support, JetBrains data).
        for process in processes {
            if assigned.contains_key(&process.pid) {
                continue;
            }
            if let Some(root) = self.root_for_related_support(process) {
                assigned.insert(process.pid, root);
            }
        }

        // Shared JetBrains daemon (ppid=1) — only when a single JetBrains IDE is active.
        attach_jetbrains_daemon(&mut assigned, processes);

        let mut groups = BTreeMap::<String, GroupBuilder>::new();
        self.seed_installed_groups(&mut groups);

        for process in processes {
            let root = assigned.get(&process.pid).cloned();
            let key = root
                .as_deref()
                .map(normalize_path)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("process:{}", process.name.to_ascii_lowercase()));
            let uninstall = root
                .as_deref()
                .and_then(|path| self.match_uninstall(path))
                .cloned();
            let install = root
                .as_deref()
                .and_then(|path| self.match_install(path))
                .cloned();
            let builder = groups.entry(key).or_insert_with(|| {
                GroupBuilder::new(
                    root.clone(),
                    uninstall.clone(),
                    install.clone(),
                    process.name.clone(),
                )
            });
            if builder.install.is_none() {
                builder.install = install;
            }
            if builder.uninstall.is_none() {
                builder.uninstall = uninstall;
            }
            if builder.root.is_none() {
                builder.root = root;
            }
            builder.processes.push(process.clone());
        }

        for entry in &inventory.autostart {
            let root = entry
                .target_path()
                .and_then(|path| self.install_root_for_path(path));
            let key = root
                .as_deref()
                .map(normalize_path)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("autostart:{}", stable_id(&entry.display_name())));
            let uninstall = root
                .as_deref()
                .and_then(|path| self.match_uninstall(path))
                .cloned();
            let install = root
                .as_deref()
                .and_then(|path| self.match_install(path))
                .cloned();
            let builder = groups.entry(key).or_insert_with(|| {
                GroupBuilder::new(
                    root.clone(),
                    uninstall.clone(),
                    install.clone(),
                    entry.display_name(),
                )
            });
            if builder.install.is_none() {
                builder.install = install;
            }
            if builder.uninstall.is_none() {
                builder.uninstall = uninstall;
            }
            if builder.root.is_none() {
                builder.root = root;
            }
            builder.autostart.push(entry.clone());
        }

        let mut result = groups
            .into_iter()
            .map(|(key, builder)| builder.finish(stable_id(&key)))
            .collect::<Vec<_>>();
        // Installed apps with running processes first, then stable A→Z.
        // Do NOT sort by load_score: CPU/RAM jitter every scan and the list jumps.
        result.sort_by(|left, right| {
            let left_running = !left.processes.is_empty();
            let right_running = !right.processes.is_empty();
            right_running.cmp(&left_running).then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
        });
        result
    }

    /// Creates empty groups for every known install so closed apps still appear.
    fn seed_installed_groups(&self, groups: &mut BTreeMap<String, GroupBuilder>) {
        for record in &self.installs {
            if is_shared_install_root(&record.install_root) {
                continue;
            }
            let key = normalize_path(&record.install_root);
            if key.is_empty() {
                continue;
            }
            groups.entry(key).or_insert_with(|| {
                GroupBuilder::new(
                    Some(record.install_root.clone()),
                    None,
                    Some(record.clone()),
                    record
                        .display_name
                        .clone()
                        .unwrap_or_else(|| record.id.clone()),
                )
            });
        }
        for record in &self.uninstall {
            let Some(root) = record.install_location.as_ref() else {
                continue;
            };
            if is_shared_install_root(root) {
                continue;
            }
            let key = normalize_path(root);
            if key.is_empty() {
                continue;
            }
            groups.entry(key).or_insert_with(|| {
                GroupBuilder::new(
                    Some(root.clone()),
                    Some(record.clone()),
                    None,
                    record
                        .display_name
                        .clone()
                        .unwrap_or_else(|| record.key_name.clone()),
                )
            });
        }
    }

    fn root_for_process(&self, process: &ProcessInfo) -> Option<PathBuf> {
        process
            .path
            .as_deref()
            .and_then(|path| self.install_root_for_path(path))
    }

    fn install_root_for_path(&self, path: &Path) -> Option<PathBuf> {
        self.match_uninstall(path)
            .and_then(|record| record.install_location.clone())
            .or_else(|| {
                self.match_install(path)
                    .map(|record| record.install_root.clone())
            })
            .or_else(|| fallback_root(path))
    }

    fn match_uninstall(&self, path: &Path) -> Option<&UninstallRecord> {
        self.uninstall.iter().find(|record| {
            record.install_location.as_deref().is_some_and(|root| {
                path_is_within(path, root) || normalize_path(path) == normalize_path(root)
            })
        })
    }

    fn match_install(&self, path: &Path) -> Option<&InstallRecord> {
        self.installs.iter().find(|record| {
            path_is_within(path, &record.install_root)
                || normalize_path(path) == normalize_path(&record.install_root)
        })
    }

    /// Matches processes under product Application Support / JetBrains data dirs.
    fn root_for_related_support(&self, process: &ProcessInfo) -> Option<PathBuf> {
        let path = process.path.as_deref()?;
        for record in &self.installs {
            let display = record
                .display_name
                .as_deref()
                .unwrap_or(record.id.as_str());
            for root in related_support_roots(&record.install_root, display) {
                if path_is_within(path, &root) || normalize_path(path) == normalize_path(&root) {
                    return Some(record.install_root.clone());
                }
            }
        }
        None
    }
}

/// Collects related processes that should be stopped together with a group.
///
/// Includes current group members, their descendants, helpers under related
/// support roots, and a shared JetBrains daemon when safe.
pub fn expand_sleep_processes(
    group: &ProcessGroup,
    all_processes: &[ProcessInfo],
) -> Vec<ProcessInfo> {
    let mut by_pid = all_processes
        .iter()
        .map(|process| (process.pid, process))
        .collect::<HashMap<_, _>>();
    let mut selected = HashMap::<u32, ProcessInfo>::new();
    for process in &group.processes {
        selected.insert(process.pid, process.clone());
        // Prefer fresher snapshot data when available.
        if let Some(live) = by_pid.remove(&process.pid) {
            selected.insert(process.pid, live.clone());
        }
    }

    let children = build_children_index(all_processes);
    let mut stack: Vec<u32> = selected.keys().copied().collect();
    while let Some(pid) = stack.pop() {
        let Some(kids) = children.get(&pid) else {
            continue;
        };
        for child in kids {
            if selected.contains_key(child) {
                continue;
            }
            if let Some(process) = all_processes.iter().find(|process| process.pid == *child) {
                selected.insert(process.pid, process.clone());
                stack.push(process.pid);
            }
        }
    }

    if let Some(install_root) = group.install_root.as_deref() {
        let roots = related_support_roots(install_root, &group.name);
        for process in all_processes {
            if selected.contains_key(&process.pid) {
                continue;
            }
            let Some(path) = process.path.as_deref() else {
                continue;
            };
            if roots
                .iter()
                .any(|root| path_is_within(path, root) || normalize_path(path) == normalize_path(root))
            {
                selected.insert(process.pid, process.clone());
            }
        }
        if looks_like_jetbrains_product(install_root, &group.name)
            && let Some(daemon) = all_processes.iter().find(|process| is_jetbrains_daemon(process))
            && !selected.contains_key(&daemon.pid)
            && jetbrains_active_install_roots(all_processes).len() <= 1
        {
            selected.insert(daemon.pid, daemon.clone());
        }
    }

    let mut processes = selected.into_values().collect::<Vec<_>>();
    processes.sort_by_key(|process| process.pid);
    processes
}

fn build_children_index(processes: &[ProcessInfo]) -> HashMap<u32, Vec<u32>> {
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for process in processes {
        if let Some(parent) = process.parent_pid {
            children.entry(parent).or_default().push(process.pid);
        }
    }
    children
}

fn related_support_roots(install_root: &Path, display_name: &str) -> Vec<PathBuf> {
    let stem = product_stem(install_root, display_name);
    let compact = stem.replace(' ', "");
    let display_compact = display_name.replace(' ', "");
    let mut roots = Vec::new();
    for base in user_data_bases() {
        roots.push(base.join(&stem));
        roots.push(base.join(&compact));
        roots.push(base.join(display_name));
        roots.push(base.join(&display_compact));
        let jetbrains = base.join("JetBrains");
        if let Ok(entries) = std::fs::read_dir(&jetbrains) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.eq_ignore_ascii_case("Daemon") {
                    continue;
                }
                if name.starts_with(&stem)
                    || name.starts_with(&compact)
                    || name
                        .to_ascii_lowercase()
                        .starts_with(&stem.to_ascii_lowercase())
                    || name
                        .to_ascii_lowercase()
                        .starts_with(&display_compact.to_ascii_lowercase())
                {
                    roots.push(entry.path());
                }
            }
        }
    }
    roots
        .into_iter()
        .filter(|path| !path.as_os_str().is_empty())
        .collect()
}

fn user_data_bases() -> Vec<PathBuf> {
    let mut bases = Vec::new();
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        bases.push(home.join("Library/Application Support"));
        bases.push(home.join(".config"));
        bases.push(home.join(".local/share"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        bases.push(appdata);
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        bases.push(local);
    }
    bases
}

fn product_stem(install_root: &Path, display_name: &str) -> String {
    // Prefer the full directory name: `file_stem` would turn
    // `PhpStorm-262.10315.130` into `PhpStorm-262.10315`.
    install_root
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| display_name.to_owned())
}

/// `PhpStorm-262.10315.130` → `PhpStorm`, otherwise the folder/file name.
fn humanize_install_root_name(root: &Path) -> Option<String> {
    let name = root.file_name()?.to_string_lossy();
    if name.is_empty() {
        return None;
    }
    if let Some((product, rest)) = name.split_once('-')
        && !product.is_empty()
        && rest.starts_with(|character: char| character.is_ascii_digit())
    {
        return Some(product.to_owned());
    }
    Some(name.into_owned())
}

fn looks_like_jetbrains_product(install_root: &Path, display_name: &str) -> bool {
    let stem = product_stem(install_root, display_name).to_ascii_lowercase();
    let name = display_name.to_ascii_lowercase();
    const PRODUCTS: &[&str] = &[
        "rustrover",
        "intellij",
        "idea",
        "webstorm",
        "pycharm",
        "clion",
        "goland",
        "phpstorm",
        "rider",
        "rubymine",
        "datagrip",
        "dataspell",
        "aqua",
        "writerside",
        "fleet",
    ];
    PRODUCTS
        .iter()
        .any(|product| stem.contains(product) || name.contains(product))
}

fn is_jetbrains_daemon(process: &ProcessInfo) -> bool {
    let name = process.name.to_ascii_lowercase();
    if name.contains("jetbrainsd") {
        return true;
    }
    process.path.as_deref().is_some_and(|path| {
        let text = normalize_path(path);
        text.contains("jetbrains/daemon") || text.contains("jetbrainsd")
    })
}

fn jetbrains_product_root_from_path(path: &Path) -> Option<PathBuf> {
    if let Some(app) = path.ancestors().find(|candidate| {
        candidate
            .extension()
            .is_some_and(|extension| extension == "app")
    }) {
        if looks_like_jetbrains_product(app, "") && !is_jetbrains_daemon_root(app) {
            return Some(app.to_path_buf());
        }
    }
    // Skip the executable itself: `/opt/PhpStorm-…/bin/phpstorm` would otherwise
    // match as a "product" because its file stem contains `phpstorm`.
    for ancestor in path.ancestors().skip(1) {
        if is_jetbrains_daemon_root(ancestor) {
            continue;
        }
        if looks_like_jetbrains_product(ancestor, "") {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn jetbrains_active_install_roots(processes: &[ProcessInfo]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for process in processes {
        let Some(path) = process.path.as_deref() else {
            continue;
        };
        let Some(root) = jetbrains_product_root_from_path(path) else {
            continue;
        };
        if !roots
            .iter()
            .any(|existing| normalize_path(existing) == normalize_path(&root))
        {
            roots.push(root);
        }
    }
    roots
}

fn is_jetbrains_daemon_root(root: &Path) -> bool {
    let stem = root
        .file_stem()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    stem == "jetbrainsd" || normalize_path(root).contains("jetbrains/daemon")
}

fn attach_jetbrains_daemon(assigned: &mut HashMap<u32, PathBuf>, processes: &[ProcessInfo]) {
    let Some(daemon) = processes.iter().find(|process| is_jetbrains_daemon(process)) else {
        return;
    };
    if assigned
        .get(&daemon.pid)
        .is_some_and(|root| !is_jetbrains_daemon_root(root))
    {
        // Already attached to a real IDE install root.
        return;
    }
    let mut active: Vec<PathBuf> = Vec::new();
    for root in assigned.values() {
        if is_jetbrains_daemon_root(root) {
            continue;
        }
        if looks_like_jetbrains_product(root, "")
            && !active
                .iter()
                .any(|existing| normalize_path(existing) == normalize_path(root))
        {
            active.push(root.clone());
        }
    }
    // Also consider JetBrains apps that are running but only matched via .app path.
    for root in jetbrains_active_install_roots(processes) {
        if is_jetbrains_daemon_root(&root) {
            continue;
        }
        if !active
            .iter()
            .any(|existing| normalize_path(existing) == normalize_path(&root))
        {
            active.push(root);
        }
    }
    if active.len() == 1 {
        assigned.insert(daemon.pid, active[0].clone());
    }
}

fn is_shared_install_root(path: &Path) -> bool {
    const ABSOLUTE: &[&str] = &[
        "/",
        "/usr",
        "/usr/local",
        "/usr/share",
        "/usr/lib",
        "/usr/lib64",
        "/opt",
        "/home",
        "/Users",
        "/var",
        "/var/lib",
    ];
    if ABSOLUTE.iter().any(|candidate| path == Path::new(candidate)) {
        return true;
    }
    // User profile hubs — a root here would absorb JetBrains Daemon, DBeaverData, etc.
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        let broad = [
            home.clone(),
            home.join(".local"),
            home.join(".local/share"),
            home.join(".local/bin"),
            home.join(".config"),
            home.join(".cache"),
            home.join("AppData"),
            home.join("AppData").join("Local"),
            home.join("AppData").join("Roaming"),
            home.join("Library"),
            home.join("Library").join("Application Support"),
        ];
        if broad.iter().any(|candidate| candidate == path) {
            return true;
        }
    }
    false
}

fn fallback_root(path: &Path) -> Option<PathBuf> {
    if let Some(app_bundle) = path.ancestors().find(|candidate| {
        candidate
            .extension()
            .is_some_and(|extension| extension == "app")
    }) {
        return Some(app_bundle.to_path_buf());
    }
    // Linux tarball JetBrains installs live under /opt/PhpStorm-…/bin/… —
    // group by the product directory, not the `bin/` folder.
    if let Some(jetbrains_root) = jetbrains_product_root_from_path(path) {
        return Some(jetbrains_root);
    }
    let parent = path.parent()?;
    let shared_binary_directory = [
        Path::new("/bin"),
        Path::new("/sbin"),
        Path::new("/usr/bin"),
        Path::new("/usr/sbin"),
        Path::new("/usr/local/bin"),
        Path::new("/snap/bin"),
    ]
    .contains(&parent);
    Some(if shared_binary_directory {
        path.to_path_buf()
    } else {
        parent.to_path_buf()
    })
}

struct GroupBuilder {
    root: Option<PathBuf>,
    uninstall: Option<UninstallRecord>,
    install: Option<InstallRecord>,
    fallback_name: String,
    processes: Vec<ProcessInfo>,
    autostart: Vec<AutoStartEntry>,
}

impl GroupBuilder {
    fn new(
        root: Option<PathBuf>,
        uninstall: Option<UninstallRecord>,
        install: Option<InstallRecord>,
        fallback_name: String,
    ) -> Self {
        Self {
            root,
            uninstall,
            install,
            fallback_name,
            processes: Vec::new(),
            autostart: Vec::new(),
        }
    }

    fn finish(self, id: String) -> ProcessGroup {
        let name = self
            .install
            .as_ref()
            .and_then(|record| record.display_name.clone())
            .or_else(|| {
                self.uninstall
                    .as_ref()
                    .and_then(|record| record.display_name.clone())
            })
            .or_else(|| {
                // Prefer a short product label over `PhpStorm-262.10315.130`.
                self.root
                    .as_ref()
                    .and_then(|root| humanize_install_root_name(root))
            })
            .unwrap_or(self.fallback_name);
        let load_score = LoadScorer::calculate(&self.processes, self.autostart.len());
        ProcessGroup {
            id,
            name,
            install_root: self.root,
            processes: self.processes,
            autostart_entries: self.autostart,
            uninstall_match: self.uninstall,
            install_match: self.install,
            load_score,
            status: GroupStatus::Active,
            policy: GroupPolicy::Manual,
        }
    }
}

pub struct LoadScorer;

impl LoadScorer {
    pub fn calculate(processes: &[ProcessInfo], autostart_count: usize) -> f64 {
        if processes.is_empty() && autostart_count == 0 {
            return 0.0;
        }
        let cpu_average = if processes.is_empty() {
            0.0
        } else {
            processes
                .iter()
                .map(|process| f64::from(process.cpu_usage))
                .sum::<f64>()
                / processes.len() as f64
        };
        let memory_mb = processes
            .iter()
            .map(|process| process.memory_bytes)
            .sum::<u64>() as f64
            / 1_048_576.0;
        let process_count = processes.len() as f64;
        let autostart_count = autostart_count as f64;
        (cpu_average * 0.4
            + memory_mb / 100.0 * 0.3
            + process_count * 5.0 * 0.2
            + autostart_count * 10.0 * 0.1)
            .clamp(0.0, 100.0)
    }
}

pub fn stable_id(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, path: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: "app.exe".to_owned(),
            path: Some(PathBuf::from(path)),
            parent_pid: None,
            start_time: 1,
            cpu_usage: 10.0,
            memory_bytes: 100 * 1_048_576,
        }
    }

    #[test]
    fn exact_uninstall_location_wins_over_parent_folder() {
        let record = UninstallRecord {
            hive: crate::model::RegistryHive::LocalMachine,
            key_name: "app".to_owned(),
            display_name: Some("App".to_owned()),
            install_location: Some(PathBuf::from("C:/Program Files/App")),
            display_icon: None,
            publisher: None,
        };
        let resolver = GroupResolver::new(vec![record], Vec::new());
        let groups = resolver.resolve(
            &[process(1, "C:/Program Files/App/bin/app.exe")],
            &InventorySnapshot::default(),
        );
        assert_eq!(
            groups[0].install_root.as_deref(),
            Some(Path::new("C:/Program Files/App"))
        );
        assert_eq!(groups[0].name, "App");
    }

    #[test]
    fn stable_id_is_repeatable() {
        assert_eq!(stable_id("abc"), stable_id("abc"));
        assert_ne!(stable_id("abc"), stable_id("abd"));
    }

    #[test]
    fn score_counts_autostart_entries() {
        assert!(LoadScorer::calculate(&[], 2) > LoadScorer::calculate(&[], 1));
        assert!(LoadScorer::calculate(&[process(1, "/tmp/app")], 1) <= 100.0);
    }

    #[test]
    fn mac_app_bundle_becomes_install_root() {
        let resolver = GroupResolver::new(Vec::new(), Vec::new());
        let groups = resolver.resolve(
            &[process(
                1,
                "/Applications/Visual Studio Code.app/Contents/MacOS/Electron",
            )],
            &InventorySnapshot::default(),
        );
        assert_eq!(
            groups[0].install_root.as_deref(),
            Some(Path::new("/Applications/Visual Studio Code.app"))
        );
    }

    #[test]
    fn seeds_installed_app_without_running_processes() {
        let install = InstallRecord {
            source: crate::model::InstallSource::MacOsBundle,
            id: "com.microsoft.VSCode".into(),
            display_name: Some("Visual Studio Code".into()),
            install_root: PathBuf::from("/Applications/Visual Studio Code.app"),
            version: Some("1.0".into()),
            publisher: None,
        };
        let resolver = GroupResolver::new(Vec::new(), vec![install]);
        let groups = resolver.resolve(&[], &InventorySnapshot::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Visual Studio Code");
        assert!(groups[0].processes.is_empty());
        assert_eq!(
            groups[0].install_root.as_deref(),
            Some(Path::new("/Applications/Visual Studio Code.app"))
        );
    }

    #[test]
    fn attaches_running_process_to_seeded_install() {
        let install = InstallRecord {
            source: crate::model::InstallSource::MacOsBundle,
            id: "com.microsoft.VSCode".into(),
            display_name: Some("Visual Studio Code".into()),
            install_root: PathBuf::from("/Applications/Visual Studio Code.app"),
            version: None,
            publisher: None,
        };
        let resolver = GroupResolver::new(Vec::new(), vec![install]);
        let mut code = process(
            1,
            "/Applications/Visual Studio Code.app/Contents/MacOS/Electron",
        );
        code.name = "Code".into();
        let groups = resolver.resolve(&[code], &InventorySnapshot::default());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Visual Studio Code");
        assert_eq!(groups[0].processes.len(), 1);
        assert_eq!(groups[0].processes[0].name, "Code");
    }

    #[test]
    fn unrelated_process_stays_separate_from_seeded_install() {
        let install = InstallRecord {
            source: crate::model::InstallSource::MacOsBundle,
            id: "com.microsoft.VSCode".into(),
            display_name: Some("Visual Studio Code".into()),
            install_root: PathBuf::from("/Applications/Visual Studio Code.app"),
            version: None,
            publisher: None,
        };
        let resolver = GroupResolver::new(Vec::new(), vec![install]);
        let groups = resolver.resolve(
            &[process(2, "/Applications/Other.app/Contents/MacOS/Other")],
            &InventorySnapshot::default(),
        );
        assert_eq!(groups.len(), 2);
        let vscode = groups
            .iter()
            .find(|group| group.name == "Visual Studio Code")
            .unwrap();
        assert!(vscode.processes.is_empty());
    }

    #[test]
    fn shared_binary_directory_does_not_merge_unrelated_apps() {
        let resolver = GroupResolver::new(Vec::new(), Vec::new());
        let groups = resolver.resolve(
            &[
                process(1, "/usr/bin/example-a"),
                process(2, "/usr/bin/example-b"),
            ],
            &InventorySnapshot::default(),
        );
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn expand_sleep_includes_descendants_and_jetbrains_daemon() {
        let mut parent = process(10, "/Applications/RustRover.app/Contents/MacOS/rustrover");
        parent.name = "rustrover".into();
        let mut child = process(11, "/Applications/RustRover.app/Contents/helpers/fsnotifier");
        child.name = "fsnotifier".into();
        child.parent_pid = Some(10);
        let mut daemon = process(
            12,
            "/Users/me/Library/Application Support/JetBrains/Daemon/bundles/current/jetbrainsd.app/Contents/MacOS/jetbrainsd",
        );
        daemon.name = "jetbrainsd".into();
        daemon.parent_pid = Some(1);

        let group = ProcessGroup {
            id: "rr".into(),
            name: "RustRover".into(),
            install_root: Some(PathBuf::from("/Applications/RustRover.app")),
            processes: vec![parent.clone()],
            autostart_entries: Vec::new(),
            uninstall_match: None,
            install_match: None,
            load_score: 1.0,
            status: GroupStatus::Active,
            policy: GroupPolicy::Manual,
        };
        let expanded = expand_sleep_processes(&group, &[parent, child, daemon]);
        let pids = expanded.iter().map(|process| process.pid).collect::<Vec<_>>();
        assert!(pids.contains(&10));
        assert!(pids.contains(&11));
        assert!(pids.contains(&12));
    }

    #[test]
    fn attach_jetbrains_daemon_when_single_ide_running() {
        let install = InstallRecord {
            source: crate::model::InstallSource::MacOsBundle,
            id: "com.jetbrains.rustrover".into(),
            display_name: Some("RustRover".into()),
            install_root: PathBuf::from("/Applications/RustRover.app"),
            version: None,
            publisher: None,
        };
        let resolver = GroupResolver::new(Vec::new(), vec![install]);
        let mut ide = process(10, "/Applications/RustRover.app/Contents/MacOS/rustrover");
        ide.name = "rustrover".into();
        let mut daemon = process(
            12,
            "/Users/me/Library/Application Support/JetBrains/Daemon/bundles/current/jetbrainsd.app/Contents/MacOS/jetbrainsd",
        );
        daemon.name = "jetbrainsd".into();
        let groups = resolver.resolve(&[ide, daemon], &InventorySnapshot::default());
        let rustrover = groups
            .iter()
            .find(|group| group.name == "RustRover")
            .expect("RustRover group");
        assert!(
            rustrover
                .processes
                .iter()
                .any(|process| process.name == "jetbrainsd"),
            "shared JetBrains daemon must join the only active IDE group"
        );
    }

    #[test]
    fn expand_sleep_includes_windows_style_jetbrains_paths() {
        let mut parent = process(
            20,
            r"C:\Program Files\JetBrains\RustRover 2026.2\bin\rustrover64.exe",
        );
        parent.name = "rustrover64.exe".into();
        let mut daemon = process(
            21,
            r"C:\Users\me\AppData\Local\JetBrains\Daemon\bundles\current\jetbrainsd.exe",
        );
        daemon.name = "jetbrainsd.exe".into();
        daemon.parent_pid = Some(1);
        let group = ProcessGroup {
            id: "rr".into(),
            name: "RustRover".into(),
            install_root: Some(PathBuf::from(
                r"C:\Program Files\JetBrains\RustRover 2026.2",
            )),
            processes: vec![parent.clone()],
            autostart_entries: Vec::new(),
            uninstall_match: None,
            install_match: None,
            load_score: 1.0,
            status: GroupStatus::Active,
            policy: GroupPolicy::Manual,
        };
        let expanded = expand_sleep_processes(&group, &[parent, daemon]);
        assert!(expanded.iter().any(|process| process.pid == 21));
    }

    #[test]
    fn linux_phpstorm_tarball_groups_under_product_root() {
        let resolver = GroupResolver::new(Vec::new(), Vec::new());
        let mut ide = process(10, "/opt/PhpStorm-262.10315.130/bin/phpstorm");
        ide.name = "DefaultDispatch".into();
        let mut helper = process(11, "/opt/PhpStorm-262.10315.130/bin/fsnotifier");
        helper.name = "fsnotifier".into();
        let groups = resolver.resolve(&[ide, helper], &InventorySnapshot::default());
        let group = groups
            .iter()
            .find(|group| {
                group.install_root.as_deref() == Some(Path::new("/opt/PhpStorm-262.10315.130"))
            })
            .expect("PhpStorm product group");
        assert_eq!(group.name, "PhpStorm");
        assert_eq!(group.processes.len(), 2);
    }

    #[test]
    fn linux_phpstorm_desktop_install_uses_display_name() {
        let install = InstallRecord {
            source: crate::model::InstallSource::XdgDesktop,
            id: "jetbrains-phpstorm".into(),
            display_name: Some("PhpStorm".into()),
            install_root: PathBuf::from("/opt/PhpStorm-262.10315.130"),
            version: None,
            publisher: None,
        };
        let resolver = GroupResolver::new(Vec::new(), vec![install]);
        let mut ide = process(10, "/opt/PhpStorm-262.10315.130/bin/phpstorm");
        ide.name = "phpstorm".into();
        let groups = resolver.resolve(&[ide], &InventorySnapshot::default());
        let group = groups
            .iter()
            .find(|group| group.name == "PhpStorm")
            .expect("PhpStorm display name");
        assert_eq!(
            group.install_root.as_deref(),
            Some(Path::new("/opt/PhpStorm-262.10315.130"))
        );
        assert!(!group.processes.is_empty());
    }
}