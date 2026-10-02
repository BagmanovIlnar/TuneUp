use std::sync::Arc;

use tuneup_core::{
    TuneupError,
    model::{GroupPolicy, GroupStatus, ManagedGroupState, ProcessGroup},
};
use tuneup_platform::{AppLauncher, PlatformMutator, ProcessControl};

pub struct ManagedActionReport {
    pub state: ManagedGroupState,
    pub errors: Vec<TuneupError>,
}

pub struct DeactivationManager {
    platform: Box<dyn PlatformMutator>,
    process_control: Arc<dyn ProcessControl>,
    app_launcher: Arc<dyn AppLauncher>,
}

impl DeactivationManager {
    pub fn new(
        platform: Box<dyn PlatformMutator>,
        process_control: Arc<dyn ProcessControl>,
        app_launcher: Arc<dyn AppLauncher>,
    ) -> Self {
        Self {
            platform,
            process_control,
            app_launcher,
        }
    }

    /// Terminates group processes and disables matched autostart entries.
    pub fn sleep(&mut self, group: &ProcessGroup, policy: GroupPolicy) -> ManagedActionReport {
        let mut state = ManagedGroupState::from_group(group, policy);
        let mut errors = Vec::new();

        if !group.processes.is_empty() {
            match self.process_control.terminate(&group.processes) {
                Ok(()) => {
                    state.processes = Vec::new();
                }
                Err((remaining, process_errors)) => {
                    state.processes = remaining;
                    state.lifecycle = if state.processes.is_empty() {
                        GroupStatus::Active
                    } else {
                        GroupStatus::Sleeping
                    };
                    return ManagedActionReport {
                        state,
                        errors: process_errors,
                    };
                }
            }
        }

        let platform = self.platform.disable_for_group(group);
        state.platform = platform.backup;
        state.lifecycle = GroupStatus::Sleeping;
        state.processes = Vec::new();
        errors.extend(platform.errors);
        ManagedActionReport { state, errors }
    }

    /// Restores autostart. When `launch` is true, also starts the application.
    pub fn wake(&mut self, mut state: ManagedGroupState, launch: bool) -> ManagedActionReport {
        let platform = self.platform.restore(std::mem::take(&mut state.platform));
        state.platform = platform.backup;
        let mut errors = platform.errors;

        if !state.processes.is_empty() {
            let (remaining, kill_errors) = self
                .process_control
                .terminate_identities(std::mem::take(&mut state.processes));
            state.processes = remaining;
            errors.extend(kill_errors);
        }

        if launch {
            if let Some(root) = state.match_rules.install_root.clone() {
                if let Err(error) = self.app_launcher.launch(&root, &state.name) {
                    errors.push(error);
                }
            }
        }

        state.lifecycle = if state.processes.is_empty() && state.platform.is_empty() {
            GroupStatus::Active
        } else {
            GroupStatus::Sleeping
        };
        ManagedActionReport { state, errors }
    }

    /// Force-terminates processes without changing platform autostart backups.
    pub fn terminate_only(
        &self,
        processes: &[tuneup_core::model::ProcessInfo],
    ) -> Result<(), (Vec<tuneup_core::model::ProcessIdentity>, Vec<TuneupError>)> {
        if processes.is_empty() {
            return Ok(());
        }
        self.process_control.terminate(processes)
    }

    /// Force-kills leftover identities from a previous freeze-based sleep.
    pub fn finish_legacy_freeze(
        &self,
        identities: Vec<tuneup_core::model::ProcessIdentity>,
    ) -> (Vec<tuneup_core::model::ProcessIdentity>, Vec<TuneupError>) {
        self.process_control.terminate_identities(identities)
    }
}
