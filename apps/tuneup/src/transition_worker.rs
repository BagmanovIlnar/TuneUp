//! Background worker for sleep/wake transitions.
//!
//! Terminating processes (graceful `SIGTERM` + grace period) and flipping
//! platform autostart entries (admin prompts on macOS, pkexec on Linux) can
//! block for seconds. Running them on the egui UI thread would freeze the
//! window, so every `DeactivationManager` call is dispatched here and the
//! report is marshalled back as an event.

use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};

use eframe::egui;
use tuneup_core::{
    TuneupError,
    model::{GroupPolicy, ManagedGroupState, ProcessGroup, ProcessIdentity, ProcessInfo},
};
use tuneup_platform::{AppLauncher, PlatformMutator, ProcessControl};

use crate::deactivator::{DeactivationManager, ManagedActionReport};

enum Command {
    Sleep {
        group: ProcessGroup,
        policy: GroupPolicy,
        automatic: bool,
    },
    Wake {
        state: ManagedGroupState,
        launch: bool,
        automatic: bool,
        /// Orphan cleanup: restore platform entries but do not keep the group
        /// managed afterwards.
        orphan: bool,
    },
    TerminateOnly {
        group_id: String,
        name: String,
        processes: Vec<ProcessInfo>,
    },
    FinishLegacyFreeze {
        group_id: String,
        identities: Vec<ProcessIdentity>,
    },
    Stop,
}

/// Events produced by [`TransitionWorker`].
#[derive(Debug)]
pub enum TransitionEvent {
    Slept {
        group_id: String,
        name: String,
        automatic: bool,
        report: ManagedActionReport,
    },
    Woke {
        group_id: String,
        name: String,
        automatic: bool,
        orphan: bool,
        report: ManagedActionReport,
    },
    Terminated {
        group_id: String,
        name: String,
        result: Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)>,
    },
    LegacyFreezeFinished {
        group_id: String,
        remaining: Vec<ProcessIdentity>,
        errors: Vec<TuneupError>,
    },
}

/// Asynchronous sleep/wake orchestrator used by the GUI.
pub struct TransitionWorker {
    command_tx: Sender<Command>,
    event_rx: Receiver<TransitionEvent>,
    worker: Option<JoinHandle<()>>,
}

impl TransitionWorker {
    /// Starts the transition thread owning the [`DeactivationManager`].
    pub fn start(
        repaint: egui::Context,
        mutator: Box<dyn PlatformMutator>,
        process_control: Arc<dyn ProcessControl>,
        app_launcher: Arc<dyn AppLauncher>,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("tuneup-transitions".into())
            .spawn(move || {
                let mut deactivator =
                    DeactivationManager::new(mutator, process_control, app_launcher);
                while let Ok(command) = command_rx.recv() {
                    let event = match command {
                        Command::Stop => break,
                        Command::Sleep {
                            group,
                            policy,
                            automatic,
                        } => {
                            let group_id = group.id.clone();
                            let name = group.name.clone();
                            let report = deactivator.sleep(&group, policy);
                            TransitionEvent::Slept {
                                group_id,
                                name,
                                automatic,
                                report,
                            }
                        }
                        Command::Wake {
                            state,
                            launch,
                            automatic,
                            orphan,
                        } => {
                            let group_id = state.group_id.clone();
                            let name = state.name.clone();
                            let report = deactivator.wake(state, launch);
                            TransitionEvent::Woke {
                                group_id,
                                name,
                                automatic,
                                orphan,
                                report,
                            }
                        }
                        Command::TerminateOnly {
                            group_id,
                            name,
                            processes,
                        } => {
                            let result = deactivator.terminate_only(&processes);
                            TransitionEvent::Terminated {
                                group_id,
                                name,
                                result,
                            }
                        }
                        Command::FinishLegacyFreeze {
                            group_id,
                            identities,
                        } => {
                            let (remaining, errors) = deactivator.finish_legacy_freeze(identities);
                            TransitionEvent::LegacyFreezeFinished {
                                group_id,
                                remaining,
                                errors,
                            }
                        }
                    };
                    let _ = event_tx.send(event);
                    repaint.request_repaint();
                }
            })
            .expect("spawn transition worker");
        Self {
            command_tx,
            event_rx,
            worker: Some(worker),
        }
    }

    /// Requests a sleep transition (terminate + disable autostart).
    pub fn sleep(&self, group: ProcessGroup, policy: GroupPolicy, automatic: bool) {
        let _ = self.command_tx.send(Command::Sleep {
            group,
            policy,
            automatic,
        });
    }

    /// Requests a wake transition (restore autostart, optional launch).
    pub fn wake(&self, state: ManagedGroupState, launch: bool, automatic: bool, orphan: bool) {
        let _ = self.command_tx.send(Command::Wake {
            state,
            launch,
            automatic,
            orphan,
        });
    }

    /// Requests a pure terminate (re-enforcing persisted sleep).
    pub fn terminate_only(&self, group_id: String, name: String, processes: Vec<ProcessInfo>) {
        let _ = self.command_tx.send(Command::TerminateOnly {
            group_id,
            name,
            processes,
        });
    }

    /// Requests terminating stale freeze-era identities.
    pub fn finish_legacy_freeze(&self, group_id: String, identities: Vec<ProcessIdentity>) {
        let _ = self.command_tx.send(Command::FinishLegacyFreeze {
            group_id,
            identities,
        });
    }

    /// Drains all pending transition events.
    pub fn take_events(&self) -> Vec<TransitionEvent> {
        self.event_rx.try_iter().collect()
    }
}

impl Drop for TransitionWorker {
    fn drop(&mut self) {
        let _ = self.command_tx.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
