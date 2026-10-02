use std::{
    path::Path,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;
use sysinfo::{Process, ProcessRefreshKind, ProcessesToUpdate, System};
use tuneup_core::{
    grouping::GroupResolver,
    model::{InventorySnapshot, ProcessGroup, ProcessInfo},
};
use tuneup_platform::InventoryProvider;

pub struct ScanSnapshot {
    pub groups: Vec<ProcessGroup>,
    pub processes: Vec<ProcessInfo>,
    pub warnings: Vec<String>,
}

enum ScannerCommand {
    Refresh,
    Stop,
}

pub struct ProcessScanner {
    command_tx: Sender<ScannerCommand>,
    snapshot_rx: Receiver<ScanSnapshot>,
    worker: Option<JoinHandle<()>>,
}

impl ProcessScanner {
    pub fn start(repaint: egui::Context, inventory_scanner: Arc<dyn InventoryProvider>) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut system = System::new();
            let mut inventory = inventory_scanner.snapshot().unwrap_or_default();
            let mut inventory_updated = Instant::now();
            let mut last_structure = String::new();
            let mut last_metrics_repaint = Instant::now();
            let mut force_repaint = true;
            loop {
                refresh_process_list(&mut system);
                let processes = collect_processes(&system);
                let groups =
                    GroupResolver::new(inventory.uninstall.clone(), inventory.installs.clone())
                        .resolve(&processes, &inventory);
                let structure = structure_key(&groups);
                let structure_changed = structure != last_structure;
                if structure_changed {
                    last_structure = structure;
                }
                // Always publish for sleep policy; only wake the UI when the
                // app set changes, or rarely for live CPU/RAM labels.
                let want_repaint = force_repaint
                    || structure_changed
                    || last_metrics_repaint.elapsed() >= Duration::from_secs(10);
                force_repaint = false;
                if snapshot_tx
                    .send(ScanSnapshot {
                        groups,
                        processes,
                        warnings: inventory.warnings.clone(),
                    })
                    .is_err()
                {
                    break;
                }
                if want_repaint {
                    last_metrics_repaint = Instant::now();
                    repaint.request_repaint();
                }

                let force_inventory = match command_rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(ScannerCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Ok(ScannerCommand::Refresh) => {
                        force_repaint = true;
                        true
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => false,
                };
                if force_inventory || inventory_updated.elapsed() >= Duration::from_secs(60) {
                    inventory =
                        inventory_scanner
                            .snapshot()
                            .unwrap_or_else(|error| InventorySnapshot {
                                warnings: vec![error.to_string()],
                                ..InventorySnapshot::default()
                            });
                    inventory_updated = Instant::now();
                }
            }
        });
        Self {
            command_tx,
            snapshot_rx,
            worker: Some(worker),
        }
    }

    pub fn request_refresh(&self) {
        let _ = self.command_tx.send(ScannerCommand::Refresh);
    }

    pub fn take_latest(&self) -> Option<ScanSnapshot> {
        self.snapshot_rx.try_iter().last()
    }
}

impl Drop for ProcessScanner {
    fn drop(&mut self) {
        let _ = self.command_tx.send(ScannerCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Stable key for “which apps / which PIDs” — ignores CPU/RAM noise.
fn structure_key(groups: &[ProcessGroup]) -> String {
    let mut parts = groups
        .iter()
        .map(|group| {
            let mut pids = group
                .processes
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>();
            pids.sort_unstable();
            format!("{}:{pids:?}", group.id)
        })
        .collect::<Vec<_>>();
    parts.sort();
    parts.join("|")
}

/// Refresh real processes only.
///
/// On Linux, `ProcessRefreshKind::everything()` also walks `/proc/<pid>/task`
/// and exposes every thread as a separate process. Thread `comm` names are
/// truncated to 15 chars (`DefaultDispatcher-worker` → `DefaultDispatch`), and
/// summing RSS across threads multiplies the same process memory many times.
fn refresh_process_list(system: &mut System) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::everything().without_tasks(),
    );
}

fn collect_processes(system: &System) -> Vec<ProcessInfo> {
    system
        .processes()
        .iter()
        .filter(|(_, process)| process.thread_kind().is_none())
        .map(|(pid, process)| ProcessInfo {
            pid: pid.as_u32(),
            name: process_display_name(process),
            path: process.exe().map(ToOwned::to_owned),
            parent_pid: process.parent().map(|parent| parent.as_u32()),
            start_time: process.start_time(),
            cpu_usage: process.cpu_usage(),
            memory_bytes: process.memory(),
        })
        .collect()
}

fn process_display_name(process: &Process) -> String {
    resolve_process_name(&process.name().to_string_lossy(), process.exe())
}

/// Prefer the executable basename over Linux `/proc/.../comm`.
///
/// `comm` is capped at 15 bytes and for JVM/Kotlin apps often shows a thread
/// pool name (`DefaultDispatch`) instead of the product binary (`phpstorm`).
fn resolve_process_name(comm: &str, exe: Option<&Path>) -> String {
    if let Some(file_name) = exe.and_then(Path::file_name) {
        let name = file_name.to_string_lossy();
        if !name.is_empty() {
            return name.into_owned();
        }
    }
    comm.to_owned()
}

#[cfg(test)]
mod tests {
    use super::resolve_process_name;
    use std::path::Path;

    #[test]
    fn prefers_executable_basename_over_comm() {
        assert_eq!(
            resolve_process_name(
                "DefaultDispatch",
                Some(Path::new("/opt/PhpStorm-262.10315.130/bin/phpstorm"))
            ),
            "phpstorm"
        );
    }

    #[test]
    fn falls_back_to_comm_without_exe() {
        assert_eq!(resolve_process_name("bash", None), "bash");
    }
}
