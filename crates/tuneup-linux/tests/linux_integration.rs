#![cfg(target_os = "linux")]

use std::fs;

use tuneup_core::model::{
    AutoStartEntry, ExecutableRef, GroupPolicy, GroupStatus, ProcessGroup, XdgAutostartLocation,
};
use tuneup_linux::{LinuxInventoryProvider, LinuxPlatformMutator};

#[test]
#[ignore = "reads package manager, XDG, and live systemd state"]
fn live_inventory_completes_without_shell() {
    let snapshot = LinuxInventoryProvider.snapshot().unwrap();
    assert!(
        !snapshot.installs.is_empty()
            || !snapshot.autostart.is_empty()
            || !snapshot.warnings.is_empty()
    );
}

#[test]
#[ignore = "mutates a temporary XDG desktop file"]
fn xdg_disable_and_restore_round_trip() {
    let base = std::env::temp_dir().join(format!("tuneup-linux-it-{}", std::process::id()));
    let root = base.join("opt/acme");
    let executable = root.join("agent");
    let desktop = base.join("home/autostart/acme.desktop");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(desktop.parent().unwrap()).unwrap();
    fs::write(&executable, b"test").unwrap();
    let original = format!(
        "[Desktop Entry]\nName=Acme\nExec={}\n",
        executable.display()
    );
    fs::write(&desktop, original.as_bytes()).unwrap();

    let group = ProcessGroup {
        id: "acme".to_owned(),
        name: "Acme".to_owned(),
        install_root: Some(root),
        processes: Vec::new(),
        autostart_entries: vec![AutoStartEntry::XdgDesktop {
            location: XdgAutostartLocation::UserConfig,
            desktop_path: desktop.clone(),
            name: "Acme".to_owned(),
            hidden: false,
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

    let mut mutator = LinuxPlatformMutator::default();
    let disabled = mutator.disable_for_group(&group);
    assert!(disabled.errors.is_empty());
    assert!(
        fs::read_to_string(&desktop)
            .unwrap()
            .contains("Hidden=true")
    );
    let restored = mutator.restore(disabled.backup);
    assert!(restored.errors.is_empty());
    assert_eq!(fs::read(&desktop).unwrap(), original.as_bytes());
    let _ = fs::remove_dir_all(base);
}
