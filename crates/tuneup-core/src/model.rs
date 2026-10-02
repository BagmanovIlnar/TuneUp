use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub path: Option<PathBuf>,
    pub parent_pid: Option<u32>,
    pub start_time: u64,
    pub cpu_usage: f32,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutableRef {
    pub raw: String,
    pub resolved_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryHive {
    CurrentUser,
    LocalMachine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Wow64View {
    Default,
    Wow64_32,
    Wow64_64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartupFolderKind {
    User,
    Common,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallRecord {
    pub hive: RegistryHive,
    pub key_name: String,
    pub display_name: Option<String>,
    pub install_location: Option<PathBuf>,
    pub display_icon: Option<PathBuf>,
    pub publisher: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstallSource {
    WindowsRegistry,
    MacOsBundle,
    Deb,
    Rpm,
    /// Tarball / manual install discovered via XDG `.desktop` (e.g. JetBrains under `/opt`).
    XdgDesktop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub source: InstallSource,
    pub id: String,
    pub display_name: Option<String>,
    pub install_root: PathBuf,
    pub version: Option<String>,
    pub publisher: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaunchdDomain {
    UserAgent,
    SystemAgent,
    SystemDaemon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoginItemKind {
    Legacy,
    ServiceManagement,
    BackgroundTask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AutostartScope {
    User,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum XdgAutostartLocation {
    UserConfig,
    SystemConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AutoStartEntry {
    RegistryRun {
        hive: RegistryHive,
        subkey: String,
        value_name: String,
        view: Wow64View,
        command: String,
        target: ExecutableRef,
    },
    StartupFolder {
        kind: StartupFolderKind,
        entry_path: PathBuf,
        target: ExecutableRef,
    },
    ScheduledTask {
        full_name: String,
        enabled: bool,
        target: ExecutableRef,
    },
    Service {
        service_name: String,
        display_name: Option<String>,
        start_type: u32,
        running: bool,
        protected: bool,
        target: ExecutableRef,
    },
    Launchd {
        domain: LaunchdDomain,
        label: String,
        plist_path: PathBuf,
        loaded: bool,
        protected: bool,
        target: ExecutableRef,
    },
    LoginItem {
        kind: LoginItemKind,
        identifier: String,
        display_name: Option<String>,
        target: ExecutableRef,
        mutable: bool,
    },
    XdgDesktop {
        location: XdgAutostartLocation,
        desktop_path: PathBuf,
        name: String,
        hidden: bool,
        target: ExecutableRef,
    },
    SystemdUnit {
        scope: AutostartScope,
        unit_name: String,
        enabled: bool,
        active: bool,
        protected: bool,
        target: ExecutableRef,
    },
}

impl AutoStartEntry {
    pub fn target_path(&self) -> Option<&Path> {
        match self {
            Self::RegistryRun { target, .. }
            | Self::StartupFolder { target, .. }
            | Self::ScheduledTask { target, .. }
            | Self::Service { target, .. }
            | Self::Launchd { target, .. }
            | Self::LoginItem { target, .. }
            | Self::XdgDesktop { target, .. }
            | Self::SystemdUnit { target, .. } => target.resolved_path.as_deref(),
        }
    }

    pub fn display_name(&self) -> String {
        match self {
            Self::RegistryRun { value_name, .. } => format!("Run: {value_name}"),
            Self::StartupFolder { entry_path, .. } => {
                format!("Startup: {}", entry_path.display())
            }
            Self::ScheduledTask { full_name, .. } => format!("Задача: {full_name}"),
            Self::Service {
                display_name,
                service_name,
                ..
            } => format!(
                "Служба: {}",
                display_name.as_deref().unwrap_or(service_name)
            ),
            Self::Launchd { label, .. } => format!("launchd: {label}"),
            Self::LoginItem {
                identifier,
                display_name,
                ..
            } => format!(
                "Login Item: {}",
                display_name.as_deref().unwrap_or(identifier)
            ),
            Self::XdgDesktop { name, .. } => format!("XDG: {name}"),
            Self::SystemdUnit { unit_name, .. } => format!("systemd: {unit_name}"),
        }
    }

    pub fn is_protected(&self) -> bool {
        matches!(
            self,
            Self::Service {
                protected: true,
                ..
            } | Self::Launchd {
                protected: true,
                ..
            } | Self::SystemdUnit {
                protected: true,
                ..
            } | Self::LoginItem { mutable: false, .. }
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InventorySnapshot {
    pub uninstall: Vec<UninstallRecord>,
    #[serde(default)]
    pub installs: Vec<InstallRecord>,
    pub autostart: Vec<AutoStartEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupStatus {
    #[default]
    Active,
    Sleeping,
    Ignored,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupPolicy {
    #[default]
    Manual,
    AutoSleepWake,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessGroup {
    pub id: String,
    pub name: String,
    pub install_root: Option<PathBuf>,
    pub processes: Vec<ProcessInfo>,
    pub autostart_entries: Vec<AutoStartEntry>,
    pub uninstall_match: Option<UninstallRecord>,
    pub install_match: Option<InstallRecord>,
    pub load_score: f64,
    pub status: GroupStatus,
    pub policy: GroupPolicy,
}

impl ProcessGroup {
    pub fn memory_bytes(&self) -> u64 {
        self.processes
            .iter()
            .map(|process| process.memory_bytes)
            .sum()
    }

    pub fn severity(&self) -> LoadSeverity {
        LoadSeverity::from_score(self.load_score)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSeverity {
    Low,
    Medium,
    High,
}

impl LoadSeverity {
    pub fn from_score(score: f64) -> Self {
        if score > 60.0 {
            Self::High
        } else if score >= 30.0 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GroupMatchRules {
    pub install_root: Option<PathBuf>,
    pub executable_names: Vec<String>,
    pub uninstall_key: Option<String>,
}

impl GroupMatchRules {
    /// Whether a process belongs to the managed application itself.
    ///
    /// When `install_root` is set, only processes under that root count — helpers
    /// like shared JetBrains daemons outside the bundle do not keep the group
    /// "active" for AutoSleepWake.
    ///
    /// # Example
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use tuneup_core::model::{GroupMatchRules, ProcessInfo};
    ///
    /// let rules = GroupMatchRules {
    ///     install_root: Some(PathBuf::from("/Applications/RustRover.app")),
    ///     executable_names: vec!["rustrover".into(), "jetbrainsd".into()],
    ///     uninstall_key: None,
    /// };
    /// let ide = ProcessInfo {
    ///     pid: 1,
    ///     name: "rustrover".into(),
    ///     path: Some(PathBuf::from("/Applications/RustRover.app/Contents/MacOS/rustrover")),
    ///     parent_pid: None,
    ///     start_time: 1,
    ///     cpu_usage: 0.0,
    ///     memory_bytes: 0,
    /// };
    /// let daemon = ProcessInfo {
    ///     pid: 2,
    ///     name: "jetbrainsd".into(),
    ///     path: Some(PathBuf::from("/Users/me/Library/Application Support/JetBrains/Daemon/jetbrainsd")),
    ///     parent_pid: Some(1),
    ///     start_time: 2,
    ///     cpu_usage: 0.0,
    ///     memory_bytes: 0,
    /// };
    /// assert!(rules.matches(&ide));
    /// assert!(!rules.matches(&daemon));
    /// ```
    pub fn matches(&self, process: &ProcessInfo) -> bool {
        if let Some(root) = &self.install_root {
            return process
                .path
                .as_deref()
                .is_some_and(|path| path_is_within(path, root));
        }
        self.executable_names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&process.name))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryBackup {
    pub hive: RegistryHive,
    pub subkey: String,
    pub view: Wow64View,
    pub name: String,
    pub value_bytes: Vec<u8>,
    pub value_type: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupMoveBackup {
    pub kind: StartupFolderKind,
    pub original_path: PathBuf,
    pub disabled_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBackup {
    pub full_name: String,
    pub was_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceBackup {
    pub name: String,
    pub original_start: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchdBackup {
    pub domain: LaunchdDomain,
    pub label: String,
    pub original_path: PathBuf,
    pub disabled_path: PathBuf,
    pub was_loaded: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginItemBackup {
    pub kind: LoginItemKind,
    pub identifier: String,
    pub target: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XdgDesktopBackup {
    pub location: XdgAutostartLocation,
    pub desktop_path: PathBuf,
    pub original_bytes: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemdUnitBackup {
    pub scope: AutostartScope,
    pub unit_name: String,
    pub was_enabled: bool,
    pub was_active: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlatformBackup {
    #[serde(default)]
    pub registry: Vec<RegistryBackup>,
    #[serde(default)]
    pub startup_moves: Vec<StartupMoveBackup>,
    #[serde(default)]
    pub tasks: Vec<TaskBackup>,
    #[serde(default)]
    pub services: Vec<ServiceBackup>,
    #[serde(default)]
    pub launchd: Vec<LaunchdBackup>,
    #[serde(default)]
    pub login_items: Vec<LoginItemBackup>,
    #[serde(default)]
    pub xdg_desktop: Vec<XdgDesktopBackup>,
    #[serde(default)]
    pub systemd_units: Vec<SystemdUnitBackup>,
}

impl PlatformBackup {
    pub fn is_empty(&self) -> bool {
        self.registry.is_empty()
            && self.startup_moves.is_empty()
            && self.tasks.is_empty()
            && self.services.is_empty()
            && self.launchd.is_empty()
            && self.login_items.is_empty()
            && self.xdg_desktop.is_empty()
            && self.systemd_units.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedGroupState {
    pub group_id: String,
    pub name: String,
    pub policy: GroupPolicy,
    pub lifecycle: GroupStatus,
    pub processes: Vec<ProcessIdentity>,
    pub platform: PlatformBackup,
    pub match_rules: GroupMatchRules,
}

impl ManagedGroupState {
    pub fn from_group(group: &ProcessGroup, policy: GroupPolicy) -> Self {
        // Only primary binaries under install_root (not shared helpers/daemons).
        let mut executable_names = group
            .processes
            .iter()
            .filter(|process| match group.install_root.as_deref() {
                Some(root) => process
                    .path
                    .as_deref()
                    .is_some_and(|path| path_is_within(path, root)),
                None => true,
            })
            .map(|process| process.name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        executable_names.sort();
        executable_names.dedup();
        Self {
            group_id: group.id.clone(),
            name: group.name.clone(),
            policy,
            lifecycle: GroupStatus::Active,
            processes: Vec::new(),
            platform: PlatformBackup::default(),
            match_rules: GroupMatchRules {
                install_root: group.install_root.clone(),
                executable_names,
                uninstall_key: group
                    .uninstall_match
                    .as_ref()
                    .map(|record| record.key_name.clone())
                    .or_else(|| group.install_match.as_ref().map(|record| record.id.clone())),
            },
        }
    }
}

pub fn path_is_within(candidate: &Path, root: &Path) -> bool {
    let candidate = normalize_path(candidate);
    let mut root = normalize_path(root);
    if root.is_empty() {
        return false;
    }
    if !root.ends_with('/') {
        root.push('/');
    }
    candidate.starts_with(&root)
}

pub fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}
