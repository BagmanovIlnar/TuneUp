use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::StateError,
    model::{
        GroupMatchRules, GroupPolicy, GroupStatus, ManagedGroupState, PlatformBackup,
        ProcessIdentity, RegistryBackup, RegistryHive, ServiceBackup, Wow64View,
    },
};

pub const STATE_VERSION: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub version: u32,
    pub managed_groups: HashMap<String, ManagedGroupState>,
    pub ipc_session_key: Option<[u8; 32]>,
    /// Preferred UI value for launching TuneUp at login (OS registration is authoritative).
    #[serde(default)]
    pub start_at_login: bool,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            managed_groups: HashMap::new(),
            ipc_session_key: None,
            start_at_login: false,
        }
    }
}

pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn for_current_user() -> Result<Self, StateError> {
        Ok(Self::new(default_state_path()?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<PersistedState, StateError> {
        let backup = self.path.with_extension("json.bak");
        if !self.path.exists() && !backup.exists() {
            return Ok(PersistedState::default());
        }
        let mut first_error = None;
        for source in [&self.path, &backup] {
            if !source.exists() {
                continue;
            }
            match load_file(source) {
                Ok(state) => return Ok(state),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or(StateError::DirectoryUnavailable))
    }

    pub fn save_atomic(&self, state: &PersistedState) -> Result<(), StateError> {
        let parent = self.path.parent().ok_or(StateError::DirectoryUnavailable)?;
        fs::create_dir_all(parent).map_err(|error| StateError::Io(error.to_string()))?;
        let temporary = self.path.with_extension("json.tmp");
        let backup = self.path.with_extension("json.bak");
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|error| StateError::Corrupt(error.to_string()))?;
        fs::write(&temporary, bytes).map_err(|error| StateError::Io(error.to_string()))?;
        if backup.exists() {
            fs::remove_file(&backup).map_err(|error| StateError::Io(error.to_string()))?;
        }
        if self.path.exists() {
            fs::rename(&self.path, &backup).map_err(|error| StateError::Io(error.to_string()))?;
        }
        if let Err(error) = fs::rename(&temporary, &self.path) {
            if backup.exists() {
                let _ = fs::rename(&backup, &self.path);
            }
            return Err(StateError::Io(error.to_string()));
        }
        if backup.exists() {
            fs::remove_file(backup).map_err(|error| StateError::Io(error.to_string()))?;
        }
        Ok(())
    }
}

fn load_file(path: &Path) -> Result<PersistedState, StateError> {
    let bytes = fs::read(path).map_err(|error| StateError::Io(error.to_string()))?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| StateError::Corrupt(error.to_string()))?;
    if let Some(version) = value.get("version").and_then(serde_json::Value::as_u64) {
        if !(2..=u64::from(STATE_VERSION)).contains(&version) {
            return Err(StateError::VersionUnsupported(version as u32));
        }
        let mut state: PersistedState = serde_json::from_value(value)
            .map_err(|error| StateError::Corrupt(error.to_string()))?;
        state.version = STATE_VERSION;
        return Ok(state);
    }
    migrate_v1(value)
}

fn default_state_path() -> Result<PathBuf, StateError> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".local/state"))
        });
    base.map(|base| base.join("TuneUp/state.json"))
        .ok_or(StateError::DirectoryUnavailable)
}

#[derive(Deserialize)]
struct LegacyGroup {
    name: String,
    processes: Vec<ProcessIdentity>,
    platform: LegacyPlatform,
}

#[derive(Default, Deserialize)]
struct LegacyPlatform {
    #[serde(default)]
    autostart: Vec<LegacyRegistryBackup>,
    #[serde(default)]
    services: Vec<ServiceBackup>,
}

#[derive(Deserialize)]
struct LegacyRegistryBackup {
    hive: RegistryHive,
    subkey: String,
    view: u32,
    name: String,
    value_bytes: Vec<u8>,
    value_type: u32,
}

fn migrate_v1(value: serde_json::Value) -> Result<PersistedState, StateError> {
    let legacy: HashMap<String, LegacyGroup> = serde_json::from_value(value)
        .map_err(|error| StateError::Corrupt(format!("v1: {error}")))?;
    let managed_groups = legacy
        .into_iter()
        .map(|(group_id, group)| {
            let platform = PlatformBackup {
                registry: group
                    .platform
                    .autostart
                    .into_iter()
                    .map(|backup| RegistryBackup {
                        hive: backup.hive,
                        subkey: backup.subkey,
                        view: if backup.view == 0x0200 {
                            Wow64View::Wow64_32
                        } else if backup.view == 0x0100 {
                            Wow64View::Wow64_64
                        } else {
                            Wow64View::Default
                        },
                        name: backup.name,
                        value_bytes: backup.value_bytes,
                        value_type: backup.value_type,
                    })
                    .collect(),
                services: group.platform.services,
                ..PlatformBackup::default()
            };
            (
                group_id.clone(),
                ManagedGroupState {
                    group_id,
                    name: group.name.clone(),
                    policy: GroupPolicy::Manual,
                    lifecycle: GroupStatus::Sleeping,
                    processes: group.processes,
                    platform,
                    match_rules: GroupMatchRules {
                        install_root: None,
                        executable_names: vec![group.name.to_ascii_lowercase()],
                        uninstall_key: None,
                    },
                },
            )
        })
        .collect();
    Ok(PersistedState {
        version: STATE_VERSION,
        managed_groups,
        ipc_session_key: None,
        start_at_login: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "tuneup-state-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let store = StateStore::new(path.clone());
        let state = PersistedState::default();
        store.save_atomic(&state).unwrap();
        assert_eq!(store.load().unwrap().version, STATE_VERSION);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn migrates_legacy_sleeping_group() {
        let legacy = serde_json::json!({
            "legacy-id": {
                "name": "Legacy App",
                "processes": [{ "pid": 7, "start_time": 11 }],
                "platform": {
                    "autostart": [{
                        "hive": "CurrentUser",
                        "subkey": "Software\\Legacy",
                        "view": 0,
                        "name": "Legacy",
                        "value_bytes": [1, 2, 3],
                        "value_type": 1
                    }],
                    "services": []
                }
            }
        });
        let migrated = migrate_v1(legacy).unwrap();
        let group = &migrated.managed_groups["legacy-id"];
        assert_eq!(migrated.version, STATE_VERSION);
        assert_eq!(group.lifecycle, GroupStatus::Sleeping);
        assert_eq!(group.policy, GroupPolicy::Manual);
        assert_eq!(group.platform.registry.len(), 1);
    }

    #[test]
    fn loads_backup_when_primary_is_corrupt() {
        let path =
            std::env::temp_dir().join(format!("tuneup-state-recovery-{}.json", std::process::id()));
        let backup = path.with_extension("json.bak");
        fs::write(&path, b"{broken").unwrap();
        fs::write(
            &backup,
            serde_json::to_vec(&PersistedState::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            StateStore::new(path.clone()).load().unwrap().version,
            STATE_VERSION
        );
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(backup);
    }

    #[test]
    fn migrates_version_two_state_to_current_version() {
        let value = serde_json::json!({
            "version": 2,
            "managed_groups": {},
            "ipc_session_key": null
        });
        let path =
            std::env::temp_dir().join(format!("tuneup-state-v2-{}.json", std::process::id()));
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let migrated = StateStore::new(path.clone()).load().unwrap();
        assert_eq!(migrated.version, STATE_VERSION);
        let _ = fs::remove_file(path);
    }
}
