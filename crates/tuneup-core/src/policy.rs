use std::collections::{HashMap, HashSet};

use crate::model::{GroupPolicy, GroupStatus, ManagedGroupState, ProcessIdentity, ProcessInfo};

#[derive(Debug, Clone)]
pub enum ProcessEvent {
    Created(ProcessInfo),
    Terminated(ProcessIdentity),
}

pub trait ProcessLifecycleMonitor: Send {
    fn poll(&mut self, current: &[ProcessInfo]) -> Vec<ProcessEvent>;
}

#[derive(Default)]
pub struct SysinfoPollerMonitor {
    previous: HashMap<ProcessIdentity, ProcessInfo>,
}

impl ProcessLifecycleMonitor for SysinfoPollerMonitor {
    fn poll(&mut self, current: &[ProcessInfo]) -> Vec<ProcessEvent> {
        let next = current
            .iter()
            .map(|process| {
                (
                    ProcessIdentity {
                        pid: process.pid,
                        start_time: process.start_time,
                    },
                    process.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        let mut events = next
            .iter()
            .filter(|(identity, _)| !self.previous.contains_key(*identity))
            .map(|(_, process)| ProcessEvent::Created(process.clone()))
            .collect::<Vec<_>>();
        events.extend(
            self.previous
                .keys()
                .filter(|identity| !next.contains_key(*identity))
                .cloned()
                .map(ProcessEvent::Terminated),
        );
        self.previous = next;
        events
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    WakeGroup { group_id: String },
    SleepGroup { group_id: String },
}

pub struct SleepPolicyEngine {
    managed: HashMap<String, ManagedGroupState>,
    previous: HashSet<ProcessIdentity>,
}

impl SleepPolicyEngine {
    pub fn new(managed: HashMap<String, ManagedGroupState>) -> Self {
        Self {
            managed,
            previous: HashSet::new(),
        }
    }

    pub fn replace_managed(&mut self, managed: HashMap<String, ManagedGroupState>) {
        self.managed = managed;
    }

    pub fn prime(&mut self, processes: &[ProcessInfo]) {
        self.previous = processes
            .iter()
            .map(|process| ProcessIdentity {
                pid: process.pid,
                start_time: process.start_time,
            })
            .collect();
    }

    pub fn on_snapshot(&mut self, processes: &[ProcessInfo]) -> Vec<PolicyAction> {
        let current = processes
            .iter()
            .map(|process| ProcessIdentity {
                pid: process.pid,
                start_time: process.start_time,
            })
            .collect::<HashSet<_>>();
        let created = processes
            .iter()
            .filter(|process| {
                !self.previous.contains(&ProcessIdentity {
                    pid: process.pid,
                    start_time: process.start_time,
                })
            })
            .collect::<Vec<_>>();
        let mut actions = Vec::new();

        for state in self.managed.values() {
            if state.lifecycle == GroupStatus::Ignored {
                continue;
            }
            let matching = processes
                .iter()
                .any(|process| state.match_rules.matches(process));
            let created_matching = created
                .iter()
                .any(|process| state.match_rules.matches(process));
            match state.lifecycle {
                GroupStatus::Sleeping
                    if created_matching && state.policy == GroupPolicy::AutoSleepWake =>
                {
                    actions.push(PolicyAction::WakeGroup {
                        group_id: state.group_id.clone(),
                    });
                }
                GroupStatus::Active if state.policy == GroupPolicy::AutoSleepWake && !matching => {
                    actions.push(PolicyAction::SleepGroup {
                        group_id: state.group_id.clone(),
                    });
                }
                GroupStatus::Active | GroupStatus::Sleeping | GroupStatus::Ignored => {}
            }
        }
        self.previous = current;
        actions
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{GroupMatchRules, PlatformBackup};

    fn process(pid: u32) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: "app.exe".to_owned(),
            path: Some(PathBuf::from("C:/Apps/App/app.exe")),
            parent_pid: None,
            start_time: pid as u64,
            cpu_usage: 0.0,
            memory_bytes: 0,
        }
    }

    fn managed(status: GroupStatus) -> ManagedGroupState {
        ManagedGroupState {
            group_id: "app".to_owned(),
            name: "App".to_owned(),
            policy: GroupPolicy::AutoSleepWake,
            lifecycle: status,
            processes: Vec::new(),
            platform: PlatformBackup::default(),
            match_rules: GroupMatchRules {
                install_root: Some(PathBuf::from("C:/Apps/App")),
                executable_names: vec!["app.exe".to_owned()],
                uninstall_key: None,
            },
        }
    }

    #[test]
    fn sleeping_group_wakes_for_new_matching_process() {
        let mut engine = SleepPolicyEngine::new(HashMap::from([(
            "app".to_owned(),
            managed(GroupStatus::Sleeping),
        )]));
        assert_eq!(
            engine.on_snapshot(&[process(1)]),
            vec![PolicyAction::WakeGroup {
                group_id: "app".to_owned()
            }]
        );
    }

    #[test]
    fn sleeping_manual_group_does_not_resleep_on_launch() {
        let mut state = managed(GroupStatus::Sleeping);
        state.policy = GroupPolicy::Manual;
        let mut engine = SleepPolicyEngine::new(HashMap::from([("app".to_owned(), state)]));
        assert!(engine.on_snapshot(&[process(1)]).is_empty());
    }

    #[test]
    fn active_group_sleeps_after_last_process_exits() {
        let mut engine = SleepPolicyEngine::new(HashMap::from([(
            "app".to_owned(),
            managed(GroupStatus::Active),
        )]));
        engine.on_snapshot(&[process(1)]);
        assert_eq!(
            engine.on_snapshot(&[]),
            vec![PolicyAction::SleepGroup {
                group_id: "app".to_owned()
            }]
        );
    }

    #[test]
    fn active_group_sleeps_when_only_helper_outside_install_root_remains() {
        let mut engine = SleepPolicyEngine::new(HashMap::from([(
            "app".to_owned(),
            managed(GroupStatus::Active),
        )]));
        engine.on_snapshot(&[process(1)]);
        let helper = ProcessInfo {
            pid: 99,
            name: "jetbrainsd".to_owned(),
            path: Some(PathBuf::from(
                "C:/Users/me/AppData/Local/JetBrains/Daemon/jetbrainsd.exe",
            )),
            parent_pid: Some(1),
            start_time: 99,
            cpu_usage: 0.0,
            memory_bytes: 0,
        };
        assert_eq!(
            engine.on_snapshot(&[helper]),
            vec![PolicyAction::SleepGroup {
                group_id: "app".to_owned()
            }]
        );
    }

    #[test]
    fn sleeping_group_does_not_wake_for_helper_outside_install_root() {
        let mut engine = SleepPolicyEngine::new(HashMap::from([(
            "app".to_owned(),
            managed(GroupStatus::Sleeping),
        )]));
        let helper = ProcessInfo {
            pid: 99,
            name: "jetbrainsd".to_owned(),
            path: Some(PathBuf::from(
                "C:/Users/me/AppData/Local/JetBrains/Daemon/jetbrainsd.exe",
            )),
            parent_pid: Some(1),
            start_time: 99,
            cpu_usage: 0.0,
            memory_bytes: 0,
        };
        assert!(engine.on_snapshot(&[helper]).is_empty());
    }
}
