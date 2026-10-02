#![cfg(target_os = "macos")]

use tuneup_macos::{MacOsInventoryProvider, MacOsPlatformMutator, MacOsProcessControl};

#[test]
#[ignore = "reads the host application and launchd inventory"]
fn inventory_reads_real_macos_sources() {
    let snapshot = MacOsInventoryProvider
        .snapshot()
        .expect("collect macOS inventory");
    assert!(
        !snapshot.installs.is_empty(),
        "expected at least one .app bundle"
    );
}

#[test]
#[ignore = "sends SIGKILL to a spawned host process"]
fn process_control_terminates_fixture() {
    use std::{fs, process::Command, thread, time::Duration};

    use sysinfo::{Pid, System};
    use tuneup_core::model::ProcessInfo;

    let fixture = std::env::temp_dir().join(format!("tuneup-sleep-{}", std::process::id()));
    fs::copy("/bin/sleep", &fixture).expect("copy sleep fixture");
    let mut child = Command::new(&fixture)
        .arg("30")
        .spawn()
        .expect("spawn sleep fixture");
    let system = System::new_all();
    let live = system
        .process(Pid::from_u32(child.id()))
        .expect("find sleep fixture");
    let process = ProcessInfo {
        pid: child.id(),
        name: live.name().to_string_lossy().into_owned(),
        path: live.exe().map(ToOwned::to_owned),
        parent_pid: live.parent().map(Pid::as_u32),
        start_time: live.start_time(),
        cpu_usage: live.cpu_usage(),
        memory_bytes: live.memory(),
    };
    let controller = MacOsProcessControl;
    controller.terminate(&[process]).expect("terminate fixture");
    thread::sleep(Duration::from_millis(50));
    let status = child.try_wait().expect("wait after kill");
    assert!(status.is_some(), "fixture process must exit after SIGKILL");
    let _ = fs::remove_file(fixture);
}

#[test]
#[ignore = "creates a temporary user LaunchAgent"]
fn user_launch_agent_round_trip() {
    use std::{fs, path::PathBuf};

    use tuneup_core::model::{
        AutoStartEntry, ExecutableRef, GroupPolicy, GroupStatus, LaunchdDomain, ProcessGroup,
    };

    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let root = home
        .join("Library/Application Support/TuneUp/TestFixtures")
        .join(format!("launchd-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let executable = root.join("fixture");
    fs::write(&executable, b"fixture").unwrap();
    let agents = home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents).unwrap();
    let plist = agents.join(format!(
        "com.example.tuneup-fixture-{}.plist",
        std::process::id()
    ));
    fs::write(
        &plist,
        b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>",
    )
    .unwrap();
    let group = ProcessGroup {
        id: "macos-fixture".to_owned(),
        name: "macOS fixture".to_owned(),
        install_root: Some(root.clone()),
        processes: Vec::new(),
        autostart_entries: vec![AutoStartEntry::Launchd {
            domain: LaunchdDomain::UserAgent,
            label: "com.example.tuneup-fixture".to_owned(),
            plist_path: plist.clone(),
            loaded: false,
            protected: false,
            target: ExecutableRef {
                raw: executable.display().to_string(),
                resolved_path: Some(executable),
            },
        }],
        uninstall_match: None,
        install_match: None,
        load_score: 0.0,
        status: GroupStatus::Active,
        policy: GroupPolicy::Manual,
    };
    let mut mutator = MacOsPlatformMutator::new();
    let disabled = mutator.disable_for_group(&group);
    assert!(disabled.errors.is_empty(), "{:?}", disabled.errors);
    assert!(!plist.exists());
    let restored = mutator.restore(disabled.backup);
    assert!(restored.errors.is_empty(), "{:?}", restored.errors);
    assert!(plist.exists());
    fs::remove_file(plist).unwrap();
    fs::remove_dir_all(root).unwrap();
}
