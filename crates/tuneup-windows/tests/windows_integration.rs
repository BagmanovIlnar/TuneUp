#![cfg(windows)]

use std::{collections::HashMap, fs, path::PathBuf};

use tuneup_core::{
    ipc::{HelperCommand, HelperResponse},
    model::{
        AutoStartEntry, ExecutableRef, GroupMatchRules, GroupPolicy, GroupStatus,
        ManagedGroupState, PlatformBackup, ProcessGroup, ProcessInfo, RegistryHive, ServiceBackup,
        StartupFolderKind, TaskBackup, Wow64View,
    },
    policy::{PolicyAction, SleepPolicyEngine},
    state::{PersistedState, StateStore},
};
use tuneup_windows::{
    ipc::{ElevatedClient, load_session_key},
    platform::{ElevatedPlatformExecutor, WindowsPlatformFacade},
};
use winreg::HKCU;

fn fixture_root(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("tuneup-{name}-{}", std::process::id()))
}

fn group(root: PathBuf, entry: AutoStartEntry) -> ProcessGroup {
    ProcessGroup {
        id: "integration-fixture".to_owned(),
        name: "TuneUp integration fixture".to_owned(),
        install_root: Some(root),
        processes: Vec::new(),
        autostart_entries: vec![entry],
        uninstall_match: None,
        install_match: None,
        load_score: 0.0,
        status: GroupStatus::Active,
        policy: GroupPolicy::Manual,
    }
}

#[test]
#[ignore = "requires Windows VM"]
fn hkcu_run_entry_round_trip() {
    let root = fixture_root("registry");
    fs::create_dir_all(&root).unwrap();
    let executable = root.join("fixture.exe");
    fs::write(&executable, b"fixture").unwrap();
    let subkey = format!(r"Software\TuneUp\Tests\{}", std::process::id());
    let key = HKCU.create_subkey(&subkey).unwrap().0;
    let command = format!("\"{}\" --background", executable.display());
    key.set_value("Fixture", &command).unwrap();
    drop(key);

    let entry = AutoStartEntry::RegistryRun {
        hive: RegistryHive::CurrentUser,
        subkey: subkey.clone(),
        value_name: "Fixture".to_owned(),
        view: Wow64View::Default,
        command,
        target: ExecutableRef {
            raw: executable.display().to_string(),
            resolved_path: Some(executable),
        },
    };
    let mut facade = WindowsPlatformFacade::new(None);
    let disabled = facade.disable_for_group(&group(root.clone(), entry));
    assert!(disabled.errors.is_empty(), "{:?}", disabled.errors);
    assert!(
        HKCU.open_subkey(&subkey)
            .unwrap()
            .get_raw_value("Fixture")
            .is_err()
    );
    let restored = facade.restore(disabled.backup);
    assert!(restored.errors.is_empty(), "{:?}", restored.errors);
    let restored_command: String = HKCU
        .open_subkey(&subkey)
        .unwrap()
        .get_value("Fixture")
        .unwrap();
    assert!(restored_command.contains("--background"));

    HKCU.delete_subkey_all(&subkey).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Windows VM"]
fn startup_entry_round_trip() {
    let root = fixture_root("startup");
    let startup = root.join("Startup");
    fs::create_dir_all(&startup).unwrap();
    let executable = root.join("fixture.exe");
    let entry_path = startup.join("fixture.cmd");
    fs::write(&executable, b"fixture").unwrap();
    fs::write(&entry_path, b"@exit /b 0").unwrap();
    let entry = AutoStartEntry::StartupFolder {
        kind: StartupFolderKind::User,
        entry_path: entry_path.clone(),
        target: ExecutableRef {
            raw: executable.display().to_string(),
            resolved_path: Some(executable),
        },
    };
    let mut facade = WindowsPlatformFacade::new(None);
    let disabled = facade.disable_for_group(&group(root.clone(), entry));
    assert!(disabled.errors.is_empty(), "{:?}", disabled.errors);
    assert!(!entry_path.exists());
    let restored = facade.restore(disabled.backup);
    assert!(restored.errors.is_empty(), "{:?}", restored.errors);
    assert!(entry_path.exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires an administrator Windows VM and pre-created TUNEUP_TEST_TASK"]
fn task_fixture_disable_restore() {
    let full_name = std::env::var("TUNEUP_TEST_TASK").expect("set TUNEUP_TEST_TASK");
    let executor = ElevatedPlatformExecutor;
    let disabled = executor
        .restore(PlatformBackup {
            tasks: vec![TaskBackup {
                full_name: full_name.clone(),
                was_enabled: false,
            }],
            ..PlatformBackup::default()
        })
        .unwrap();
    assert!(disabled.is_empty());
    let restored = executor
        .restore(PlatformBackup {
            tasks: vec![TaskBackup {
                full_name,
                was_enabled: true,
            }],
            ..PlatformBackup::default()
        })
        .unwrap();
    assert!(restored.is_empty());
}

#[test]
#[ignore = "requires an administrator Windows VM and pre-created TUNEUP_TEST_SERVICE"]
fn service_fixture_disable_restore() {
    let name = std::env::var("TUNEUP_TEST_SERVICE").expect("set TUNEUP_TEST_SERVICE");
    let original_start = std::env::var("TUNEUP_TEST_SERVICE_START")
        .expect("set TUNEUP_TEST_SERVICE_START")
        .parse()
        .unwrap();
    let executor = ElevatedPlatformExecutor;
    let disabled = executor
        .restore(PlatformBackup {
            services: vec![ServiceBackup {
                name: name.clone(),
                original_start: 4,
            }],
            ..PlatformBackup::default()
        })
        .unwrap();
    assert!(disabled.is_empty());
    let restored = executor
        .restore(PlatformBackup {
            services: vec![ServiceBackup {
                name,
                original_start,
            }],
            ..PlatformBackup::default()
        })
        .unwrap();
    assert!(restored.is_empty());
}

#[test]
#[ignore = "requires Windows VM with elevated helper and test executable beside helper"]
fn helper_authenticated_ping() {
    let key = load_session_key().expect("run tuneup-helper.exe first");
    let mut client = ElevatedClient::new(key).unwrap();
    assert!(matches!(
        client.request(HelperCommand::Ping).unwrap(),
        HelperResponse::Pong { elevated: true, .. }
    ));
}

#[test]
#[ignore = "requires Windows VM"]
fn crash_recovery_preserves_sleeping_rollback_state() {
    let root = fixture_root("crash-recovery");
    fs::create_dir_all(&root).unwrap();
    let state_path = root.join("state.json");
    let mut state = PersistedState::default();
    state.managed_groups.insert(
        "fixture".to_owned(),
        ManagedGroupState {
            group_id: "fixture".to_owned(),
            name: "Fixture".to_owned(),
            policy: GroupPolicy::AutoSleepWake,
            lifecycle: GroupStatus::Sleeping,
            processes: Vec::new(),
            platform: PlatformBackup::default(),
            match_rules: GroupMatchRules {
                install_root: Some(root.clone()),
                executable_names: vec!["fixture.exe".to_owned()],
                uninstall_key: None,
            },
        },
    );
    let store = StateStore::new(state_path);
    store.save_atomic(&state).unwrap();
    let recovered = store.load().unwrap();
    assert_eq!(
        recovered.managed_groups["fixture"].lifecycle,
        GroupStatus::Sleeping
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "requires Windows VM"]
fn auto_wake_and_resleep_policy_cycle() {
    let root = fixture_root("auto-cycle");
    fs::create_dir_all(&root).unwrap();
    let process = ProcessInfo {
        pid: 42,
        name: "fixture.exe".to_owned(),
        path: Some(root.join("fixture.exe")),
        parent_pid: None,
        start_time: 1,
        cpu_usage: 0.0,
        memory_bytes: 0,
    };
    let sleeping = ManagedGroupState {
        group_id: "fixture".to_owned(),
        name: "Fixture".to_owned(),
        policy: GroupPolicy::AutoSleepWake,
        lifecycle: GroupStatus::Sleeping,
        processes: Vec::new(),
        platform: PlatformBackup::default(),
        match_rules: GroupMatchRules {
            install_root: Some(root.clone()),
            executable_names: vec!["fixture.exe".to_owned()],
            uninstall_key: None,
        },
    };
    let mut engine =
        SleepPolicyEngine::new(HashMap::from([("fixture".to_owned(), sleeping.clone())]));
    assert_eq!(
        engine.on_snapshot(std::slice::from_ref(&process)),
        vec![PolicyAction::WakeGroup {
            group_id: "fixture".to_owned()
        }]
    );
    let mut active = sleeping;
    active.lifecycle = GroupStatus::Active;
    engine.replace_managed(HashMap::from([("fixture".to_owned(), active)]));
    assert_eq!(
        engine.on_snapshot(&[]),
        vec![PolicyAction::SleepGroup {
            group_id: "fixture".to_owned()
        }]
    );
    fs::remove_dir_all(root).unwrap();
}
