use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui::{self, Color32, RichText, ViewportCommand};
use tuneup_core::{
    grouping::{expand_sleep_processes, stable_id},
    model::{
        GroupPolicy, GroupStatus, ManagedGroupState, ProcessGroup, ProcessIdentity, ProcessInfo,
        normalize_path,
    },
    policy::{PolicyAction, SleepPolicyEngine},
    state::{PersistedState, StateStore},
};
use tuneup_platform::{PlatformCapabilities, PlatformServices, SelfAutostartProvider};

use crate::deactivator::ManagedActionReport;

use crate::{
    maintenance_worker::{MaintenanceEvent, MaintenanceWorker},
    platform_runtime,
    scanner::{ProcessScanner, ScanSnapshot},
    theme::PlatformTheme,
    transition_worker::{TransitionEvent, TransitionWorker},
    tray::{TrayAction, TrayController},
    views::{
        cleanup::{CleanupAction, CleanupViewState},
        expand_toggle,
        settings::{SettingsAction, SettingsViewState},
        uninstall::{UninstallAction, UninstallViewState},
    },
    window::WindowController,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum MainSection {
    #[default]
    Sleep,
    Cleanup,
    Uninstall,
    Settings,
}

struct PendingSleep {
    group: ProcessGroup,
    automatic: bool,
}

/// Kind of an in-flight sleep/wake transition, keyed by group id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionKind {
    Sleep,
    Wake,
    Terminate,
}

/// Tracks an in-progress «Сбросить всё» waiting for wake transitions to finish.
struct ResetProgress {
    remaining: usize,
    errors: Vec<String>,
}

enum UiAction {
    Sleep(ProcessGroup),
    Wake(String),
    Ignore(ProcessGroup),
    Restore(String),
    Details(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StatusFilter {
    #[default]
    All,
    Sleeping,
    Awake,
}

impl StatusFilter {
    fn matches(self, status: GroupStatus) -> bool {
        match self {
            Self::All => true,
            Self::Sleeping => status == GroupStatus::Sleeping,
            Self::Awake => status != GroupStatus::Sleeping,
        }
    }
}

pub struct TuneupApp {
    groups: Vec<ProcessGroup>,
    processes: Vec<ProcessInfo>,
    warnings: Vec<String>,
    scanner: ProcessScanner,
    tray_event_rx: Receiver<TrayAction>,
    _tray: Option<TrayController>,
    tray_error: Option<String>,
    window: WindowController,
    shutdown_requested: bool,
    store: Option<StateStore>,
    state: PersistedState,
    state_ready: bool,
    policy: SleepPolicyEngine,
    policy_primed: bool,
    transitions: TransitionWorker,
    transitioning: HashMap<String, TransitionKind>,
    reset_pending: Option<ResetProgress>,
    pending_sleep: Option<PendingSleep>,
    details_group: Option<String>,
    action_message: Option<(bool, String)>,
    helper_status: String,
    capabilities: PlatformCapabilities,
    status_filter: StatusFilter,
    group_search: String,
    expanded_groups: HashSet<String>,
    theme: PlatformTheme,
    active_section: MainSection,
    cleanup_view: CleanupViewState,
    uninstall_view: UninstallViewState,
    settings_view: SettingsViewState,
    self_autostart: Arc<dyn SelfAutostartProvider>,
    maintenance: MaintenanceWorker,
}

impl TuneupApp {
    pub fn new(
        creation_context: &eframe::CreationContext<'_>,
        tray_event_tx: Sender<TrayAction>,
        tray_event_rx: Receiver<TrayAction>,
    ) -> Self {
        let context = creation_context.egui_ctx.clone();
        let theme = PlatformTheme::current();
        theme.apply(&context);
        let tray_result = TrayController::new(tray_event_tx, context.clone());
        let (tray, tray_error) = match tray_result {
            Ok(tray) => (Some(tray), None),
            Err(error) => (None, Some(error)),
        };
        let mut window = WindowController::hidden();
        if tray.is_none() {
            // No tray: show the window once the event loop has a Frame.
            window.prepare_open_from_context(&context);
        } else {
            // Tray-first: minimize before the first paint so eframe never
            // runs post_rendering's forced set_visible(true) flash.
            WindowController::park_at_creation(creation_context);
        }

        let (store, mut state, state_error) = match StateStore::for_current_user() {
            Ok(store) => match store.load() {
                Ok(state) => (Some(store), state, None),
                Err(error) => (
                    Some(store),
                    PersistedState::default(),
                    Some(error.to_string()),
                ),
            },
            Err(error) => (None, PersistedState::default(), Some(error.to_string())),
        };

        let (services, helper_error) = platform_runtime::create();
        let PlatformServices {
            inventory,
            process_control,
            app_launcher,
            mutator,
            cleanup,
            uninstaller,
            self_autostart,
            capabilities,
            helper_status,
        } = services;
        let action_message = state_error.or(helper_error).map(|message| (false, message));
        let policy = SleepPolicyEngine::new(state.managed_groups.clone());
        let maintenance = MaintenanceWorker::start(context.clone(), cleanup, uninstaller);
        let transitions = TransitionWorker::start(context, mutator, process_control, app_launcher);
        let mut settings_view = SettingsViewState {
            start_at_login: state.start_at_login,
            ..SettingsViewState::default()
        };
        if let Ok(enabled) = self_autostart.is_enabled() {
            settings_view.apply_os_state(enabled);
            state.start_at_login = enabled;
        }

        Self {
            groups: Vec::new(),
            processes: Vec::new(),
            warnings: Vec::new(),
            scanner: ProcessScanner::start(creation_context.egui_ctx.clone(), inventory),
            tray_event_rx,
            _tray: tray,
            tray_error,
            window,
            shutdown_requested: false,
            store,
            state,
            state_ready: action_message.is_none(),
            policy,
            policy_primed: false,
            transitions,
            transitioning: HashMap::new(),
            reset_pending: None,
            pending_sleep: None,
            details_group: None,
            action_message,
            helper_status,
            capabilities,
            status_filter: StatusFilter::All,
            group_search: String::new(),
            expanded_groups: HashSet::new(),
            theme,
            active_section: MainSection::Sleep,
            cleanup_view: CleanupViewState::default(),
            uninstall_view: UninstallViewState::default(),
            settings_view,
            self_autostart,
            maintenance,
        }
    }

    fn process_tray_events(&mut self, context: &egui::Context, frame: &eframe::Frame) {
        while let Ok(action) = self.tray_event_rx.try_recv() {
            match action {
                TrayAction::OpenMainWindow => self.window.show(context, frame),
                TrayAction::ExitApplication => self.request_exit(context),
            }
        }
    }

    fn process_scanner_events(&mut self) {
        let Some(snapshot) = self.scanner.take_latest() else {
            return;
        };
        self.apply_snapshot(snapshot);
        if !self.policy_primed {
            self.migrate_legacy_frozen_processes();
            let interrupted = self
                .state
                .managed_groups
                .values()
                .filter(|state| {
                    state.lifecycle == GroupStatus::Active
                        && (!state.processes.is_empty() || !state.platform.is_empty())
                })
                .map(|state| state.group_id.clone())
                .collect::<Vec<_>>();
            for group_id in interrupted {
                tracing::warn!(%group_id, "reconciling interrupted sleep transition");
                self.wake_group(&group_id, false, false);
            }
            self.reenforce_persisted_sleep();
            self.heal_orphaned_sleeping_groups();
            self.policy.prime(&self.processes);
            self.policy_primed = true;
            return;
        }
        self.policy
            .replace_managed(self.state.managed_groups.clone());
        let actions = self.policy.on_snapshot(&self.processes);
        for action in actions {
            match action {
                PolicyAction::WakeGroup { group_id } => self.wake_group(&group_id, true, false),
                PolicyAction::SleepGroup { group_id } => {
                    if self.transitioning.contains_key(&group_id) {
                        // A transition is already running for this group; the next
                        // snapshot re-evaluates the policy anyway.
                        continue;
                    }
                    let policy = self
                        .state
                        .managed_groups
                        .get(&group_id)
                        .map(|state| state.policy)
                        .unwrap_or(GroupPolicy::AutoSleepWake);
                    let group = self
                        .groups
                        .iter()
                        .find(|group| group.id == group_id)
                        .cloned()
                        .or_else(|| {
                            self.state
                                .managed_groups
                                .get(&group_id)
                                .map(|state| ProcessGroup {
                                    id: state.group_id.clone(),
                                    name: state.name.clone(),
                                    install_root: state.match_rules.install_root.clone(),
                                    processes: Vec::new(),
                                    autostart_entries: Vec::new(),
                                    uninstall_match: None,
                                    install_match: None,
                                    load_score: 0.0,
                                    status: GroupStatus::Active,
                                    policy: state.policy,
                                })
                        });
                    if let Some(group) = group {
                        self.sleep_group(group, policy, true);
                    }
                }
            }
        }
    }

    fn process_transition_events(&mut self) {
        for event in self.transitions.take_events() {
            match event {
                TransitionEvent::Slept {
                    group_id,
                    name,
                    automatic,
                    report,
                } => {
                    self.transitioning.remove(&group_id);
                    self.state
                        .managed_groups
                        .insert(group_id.clone(), report.state);
                    self.persist_state();
                    self.scanner.request_refresh();
                    self.action_message = Some(if report.errors.is_empty() {
                        tracing::info!(group_id = %group_id, "sleep transition completed");
                        crate::notify::app_deactivated(&name, automatic);
                        (
                            true,
                            format!(
                                "Группа «{name}» {}",
                                if automatic {
                                    "автоматически усыплена"
                                } else {
                                    "усыплена"
                                }
                            ),
                        )
                    } else {
                        tracing::error!(
                            group_id = %group_id,
                            errors = report.errors.len(),
                            "sleep transition completed with errors"
                        );
                        (
                            false,
                            report
                                .errors
                                .into_iter()
                                .map(|error| error.to_string())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    });
                }
                TransitionEvent::Woke {
                    group_id,
                    name,
                    automatic,
                    orphan,
                    report,
                } => {
                    self.transitioning.remove(&group_id);
                    if self.reset_pending.is_some() {
                        self.handle_reset_wake(group_id, name, report);
                        continue;
                    }
                    if orphan {
                        self.state.managed_groups.remove(&group_id);
                        tracing::info!(
                            group_id = %group_id,
                            "orphaned sleeping group healed: autostart restored"
                        );
                    } else if automatic {
                        // Auto wake keeps AutoSleepWake armed: closing the app will sleep again.
                        self.state
                            .managed_groups
                            .insert(group_id.clone(), report.state);
                    } else if report.state.lifecycle == GroupStatus::Sleeping
                        || !report.state.processes.is_empty()
                        || !report.state.platform.is_empty()
                    {
                        // Incomplete manual wake — keep a Manual entry so the user can retry.
                        let mut state = report.state;
                        state.policy = GroupPolicy::Manual;
                        self.state.managed_groups.insert(group_id.clone(), state);
                    }
                    // Successful manual wake: do not re-insert. Leaving managed_groups means
                    // closing the app will not put it back to sleep.
                    self.persist_state();
                    self.scanner.request_refresh();
                    crate::notify::app_activated(&name, automatic);
                    self.action_message = Some(if report.errors.is_empty() {
                        tracing::info!(group_id = %group_id, automatic, "wake transition completed");
                        (
                            true,
                            format!(
                                "Группа «{name}» {}",
                                if automatic {
                                    "автоматически пробуждена"
                                } else {
                                    "пробуждена и снята с режима сна"
                                }
                            ),
                        )
                    } else {
                        tracing::error!(
                            group_id = %group_id,
                            errors = report.errors.len(),
                            "wake transition completed with errors"
                        );
                        (
                            false,
                            report
                                .errors
                                .into_iter()
                                .map(|error| error.to_string())
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    });
                }
                TransitionEvent::Terminated {
                    group_id,
                    name,
                    result,
                } => {
                    self.transitioning.remove(&group_id);
                    match result {
                        Ok(()) => {
                            tracing::info!(group_id = %group_id, "re-enforced persisted sleep");
                            crate::notify::app_deactivated(&name, true);
                        }
                        Err((_remaining, errors)) => {
                            for error in errors {
                                tracing::error!(
                                    group_id = %group_id,
                                    error = %error,
                                    "re-enforce sleep failed"
                                );
                            }
                        }
                    }
                }
                TransitionEvent::LegacyFreezeFinished {
                    group_id,
                    remaining,
                    errors,
                } => {
                    self.transitioning.remove(&group_id);
                    if let Some(state) = self.state.managed_groups.get_mut(&group_id) {
                        state.processes = remaining;
                        self.persist_state();
                    }
                    for error in errors {
                        tracing::error!(
                            group_id = %group_id,
                            error = %error,
                            "legacy freeze terminate failed"
                        );
                    }
                }
            }
        }
    }

    fn process_maintenance_events(&mut self) {
        for event in self.maintenance.take_events() {
            match event {
                MaintenanceEvent::CleanupScan(Ok(scan)) => self.cleanup_view.apply_scan(scan),
                MaintenanceEvent::CleanupScan(Err(error)) => {
                    self.cleanup_view.busy = false;
                    self.cleanup_view.message = Some((false, error));
                }
                MaintenanceEvent::CleanupDone(Ok(report)) => self.cleanup_view.apply_report(report),
                MaintenanceEvent::CleanupDone(Err(error)) => {
                    self.cleanup_view.busy = false;
                    self.cleanup_view.message = Some((false, error));
                }
                MaintenanceEvent::Applications(Ok(apps)) => {
                    self.uninstall_view.apply_applications(apps);
                }
                MaintenanceEvent::Applications(Err(error)) => {
                    self.uninstall_view.busy = false;
                    self.uninstall_view.message = Some((false, error));
                }
                MaintenanceEvent::Leftovers {
                    application_id,
                    result,
                } => match result {
                    Ok(leftovers) => self
                        .uninstall_view
                        .apply_leftovers(application_id, leftovers),
                    Err(error) => {
                        self.uninstall_view.busy = false;
                        self.uninstall_view.message = Some((false, error));
                    }
                },
                MaintenanceEvent::UninstallDone(Ok(report)) => {
                    let need_reload = report.uninstalled;
                    self.uninstall_view.apply_report(report);
                    if need_reload {
                        self.maintenance.load_applications();
                        self.uninstall_view.busy = true;
                    }
                }
                MaintenanceEvent::UninstallDone(Err(error)) => {
                    self.uninstall_view.busy = false;
                    self.uninstall_view.message = Some((false, error));
                }
            }
        }
    }

    fn apply_snapshot(&mut self, snapshot: ScanSnapshot) {
        self.groups = snapshot.groups;
        self.processes = snapshot.processes;
        self.warnings = snapshot.warnings;
        for group in &mut self.groups {
            if let Some(managed) = self.state.managed_groups.get(&group.id) {
                group.status = managed.lifecycle;
                group.policy = managed.policy;
            }
        }
    }

    fn process_close_request(&mut self, context: &egui::Context, frame: &eframe::Frame) {
        let close_requested = context.input(|input| input.viewport().close_requested());
        if close_requested && !self.shutdown_requested {
            context.send_viewport_cmd(ViewportCommand::CancelClose);
            self.window.hide(context, frame);
        }
    }

    fn request_exit(&mut self, context: &egui::Context) {
        // Finish in-flight sleep/wake transitions first: their reports carry the
        // platform autostart backups that exist nowhere else. Exiting with a
        // transition half-done would leave autostart disabled with no backup.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !self.transitioning.is_empty() && std::time::Instant::now() < deadline {
            self.process_transition_events();
            if !self.transitioning.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        if !self.transitioning.is_empty() {
            tracing::warn!(
                remaining = self.transitioning.len(),
                "exit with in-flight transitions; last persisted state is kept"
            );
        }
        // Sleeping groups stay asleep across TuneUp restarts.
        // Do not restore autostart or clear managed state on exit.
        if self.persist_state() {
            tracing::info!(
                sleeping = self
                    .state
                    .managed_groups
                    .values()
                    .filter(|state| state.lifecycle == GroupStatus::Sleeping)
                    .count(),
                "exiting; persisted sleep state kept"
            );
            self.shutdown_requested = true;
            context.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn sleep_group(&mut self, group: ProcessGroup, policy: GroupPolicy, automatic: bool) {
        tracing::info!(
            group_id = %group.id,
            group_name = %group.name,
            ?policy,
            automatic,
            "sleep transition started"
        );
        let mut group = group;
        let expanded = expand_sleep_processes(&group, &self.processes);
        if expanded.len() != group.processes.len() {
            tracing::info!(
                group_id = %group.id,
                before = group.processes.len(),
                after = expanded.len(),
                "expanded related processes for sleep"
            );
        }
        group.processes = expanded;
        let mut intent = ManagedGroupState::from_group(&group, policy);
        intent.processes = group
            .processes
            .iter()
            .map(|process| ProcessIdentity {
                pid: process.pid,
                start_time: process.start_time,
            })
            .collect();
        let name = group.name.clone();
        self.state.managed_groups.insert(group.id.clone(), intent);
        if !self.persist_state() {
            // Keep the intent so the startup reconcile can roll it back.
            return;
        }
        self.transitioning
            .insert(group.id.clone(), TransitionKind::Sleep);
        self.transitions.sleep(group, policy, automatic);
        self.action_message = Some((true, format!("Усыпляем «{name}»…")));
    }

    fn migrate_legacy_frozen_processes(&mut self) {
        let targets = self
            .state
            .managed_groups
            .iter()
            .filter(|(_, state)| {
                state.lifecycle == GroupStatus::Sleeping && !state.processes.is_empty()
            })
            .map(|(id, state)| (id.clone(), state.processes.clone()))
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return;
        }
        for (group_id, identities) in targets {
            tracing::warn!(
                %group_id,
                count = identities.len(),
                "migrating legacy freeze PIDs to terminate"
            );
            self.transitioning
                .insert(group_id.clone(), TransitionKind::Terminate);
            self.transitions.finish_legacy_freeze(group_id, identities);
        }
    }

    /// Re-terminates processes that belong to groups still marked Sleeping in state.
    ///
    /// Platform disable stays as stored; we only kill anything that came back
    /// while TuneUp was not running.
    fn reenforce_persisted_sleep(&mut self) {
        let sleeping = self
            .state
            .managed_groups
            .iter()
            .filter(|(_, state)| state.lifecycle == GroupStatus::Sleeping)
            .map(|(id, state)| {
                (
                    id.clone(),
                    state.name.clone(),
                    state.policy,
                    state.match_rules.install_root.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (group_id, name, policy, install_root) in sleeping {
            if self.transitioning.contains_key(&group_id) {
                continue;
            }
            let group = self
                .groups
                .iter()
                .find(|group| group.id == group_id)
                .cloned()
                .unwrap_or_else(|| ProcessGroup {
                    id: group_id.clone(),
                    name: name.clone(),
                    install_root: install_root.clone(),
                    processes: Vec::new(),
                    autostart_entries: Vec::new(),
                    uninstall_match: None,
                    install_match: None,
                    load_score: 0.0,
                    status: GroupStatus::Sleeping,
                    policy,
                });
            let expanded = expand_sleep_processes(&group, &self.processes);
            if expanded.is_empty() {
                continue;
            }
            tracing::info!(
                %group_id,
                count = expanded.len(),
                "re-enforcing persisted sleep on startup"
            );
            self.transitioning
                .insert(group_id.clone(), TransitionKind::Terminate);
            self.transitions.terminate_only(group_id, name, expanded);
        }
        self.scanner.request_refresh();
    }

    fn wake_group(&mut self, group_id: &str, automatic: bool, launch: bool) {
        self.wake_group_inner(group_id, automatic, launch, false);
    }

    fn wake_group_inner(&mut self, group_id: &str, automatic: bool, launch: bool, orphan: bool) {
        if self.transitioning.contains_key(group_id) {
            return;
        }
        let Some(state) = self.state.managed_groups.remove(group_id) else {
            return;
        };
        let name = state.name.clone();
        tracing::info!(
            group_id,
            group_name = %name,
            automatic,
            launch,
            orphan,
            "wake transition started"
        );
        // Persist the removal immediately: if TuneUp dies mid-wake the group
        // must not stay flagged Sleeping (the next start would re-kill a
        // freshly woken application).
        self.persist_state();
        self.transitioning
            .insert(group_id.to_owned(), TransitionKind::Wake);
        self.transitions.wake(state, launch, automatic, orphan);
        self.action_message = Some((true, format!("Будим «{name}»…")));
    }

    /// Restores autostart for sleeping groups that no longer match any known
    /// install.
    ///
    /// An app update that changes the install path (JetBrains-style versioned
    /// folders) or a manual removal leaves the group id orphaned: the entry
    /// stays in `managed_groups` forever with autostart disabled and no UI to
    /// wake it. Healing restores the disabled entries and drops the group.
    fn heal_orphaned_sleeping_groups(&mut self) {
        let orphaned = self
            .state
            .managed_groups
            .iter()
            .filter(|(id, state)| {
                state.lifecycle == GroupStatus::Sleeping
                    && !self.groups.iter().any(|group| group.id == **id)
            })
            .filter(
                |(id, state)| match state.match_rules.install_root.as_deref() {
                    None => true,
                    // Root disappeared (uninstalled / updated to a new path), or
                    // the id was written by an older path-normalization scheme.
                    Some(root) => !root.exists() || **id != stable_id(&normalize_path(root)),
                },
            )
            .map(|(id, state)| (id.clone(), state.clone()))
            .collect::<Vec<_>>();
        let mut changed = false;
        for (group_id, state) in orphaned {
            changed = true;
            if state.platform.is_empty() {
                tracing::warn!(
                    %group_id,
                    "dropping orphaned sleeping group without platform backup"
                );
                self.state.managed_groups.remove(&group_id);
                continue;
            }
            tracing::warn!(
                %group_id,
                "healing orphaned sleeping group: restoring disabled autostart"
            );
            self.transitioning
                .insert(group_id.clone(), TransitionKind::Wake);
            self.transitions.wake(state, false, false, true);
        }
        if changed {
            self.persist_state();
        }
    }

    fn handle_reset_wake(&mut self, group_id: String, name: String, report: ManagedActionReport) {
        let Some(progress) = &mut self.reset_pending else {
            return;
        };
        tracing::info!(%group_id, "reset-all woke group");
        if report.errors.is_empty() {
            crate::notify::app_activated(&name, false);
        } else {
            progress
                .errors
                .extend(report.errors.iter().map(ToString::to_string));
        }
        progress.remaining -= 1;
        if progress.remaining == 0 {
            let progress = self
                .reset_pending
                .take()
                .expect("reset progress checked above");
            self.finish_reset(progress.errors);
        }
    }

    fn ignore_group(&mut self, group: &ProcessGroup) {
        let mut state = ManagedGroupState::from_group(group, GroupPolicy::Manual);
        state.lifecycle = GroupStatus::Ignored;
        self.state.managed_groups.insert(group.id.clone(), state);
        self.persist_state();
    }

    fn restore_group(&mut self, group_id: &str) {
        if self
            .state
            .managed_groups
            .get(group_id)
            .is_some_and(|state| state.lifecycle == GroupStatus::Sleeping)
        {
            self.wake_group(group_id, false, true);
        }
        self.state.managed_groups.remove(group_id);
        self.persist_state();
    }

    fn persist_state(&mut self) -> bool {
        let Some(store) = &self.store else {
            self.state_ready = false;
            return false;
        };
        match store.save_atomic(&self.state) {
            Ok(()) => {
                self.policy
                    .replace_managed(self.state.managed_groups.clone());
                true
            }
            Err(error) => {
                tracing::error!(%error, "state persistence failed");
                self.state_ready = false;
                self.action_message = Some((false, error.to_string()));
                false
            }
        }
    }

    fn refresh_settings_from_os(&mut self) {
        match self.self_autostart.is_enabled() {
            Ok(enabled) => {
                self.settings_view.apply_os_state(enabled);
                self.state.start_at_login = enabled;
                let _ = self.persist_state();
            }
            Err(error) => {
                self.settings_view.message = Some((false, error.to_string()));
            }
        }
    }

    fn set_start_at_login(&mut self, enabled: bool) {
        match self.self_autostart.set_enabled(enabled) {
            Ok(()) => {
                self.state.start_at_login = enabled;
                self.settings_view.start_at_login = enabled;
                self.settings_view.message = Some((
                    true,
                    if enabled {
                        "Автозапуск включён".into()
                    } else {
                        "Автозапуск выключен".into()
                    },
                ));
                let _ = self.persist_state();
            }
            Err(error) => {
                if let Ok(current) = self.self_autostart.is_enabled() {
                    self.settings_view.start_at_login = current;
                } else {
                    self.settings_view.start_at_login = !enabled;
                }
                self.settings_view.message = Some((false, error.to_string()));
            }
        }
    }

    /// Wakes every sleeping group, clears managed state, and turns off TuneUp
    /// autostart. Wake transitions run on the worker; completion is tracked in
    /// [`ResetProgress`] and finalized by [`Self::finish_reset`].
    fn reset_all(&mut self) {
        if self.reset_pending.is_some() {
            return;
        }
        tracing::info!("settings reset-all requested");
        let previous = std::mem::take(&mut self.state.managed_groups);
        for (group_id, state) in previous {
            if state.lifecycle != GroupStatus::Sleeping {
                continue;
            }
            tracing::info!(%group_id, "reset-all waking group");
            self.transitioning
                .insert(group_id.clone(), TransitionKind::Wake);
            self.transitions.wake(state, false, false, false);
        }
        // Wake transitions already in flight when the reset started are
        // absorbed by the same accounting.
        let total_wakes = self
            .transitioning
            .values()
            .filter(|kind| **kind == TransitionKind::Wake)
            .count();
        if total_wakes == 0 {
            self.finish_reset(Vec::new());
        } else {
            self.reset_pending = Some(ResetProgress {
                remaining: total_wakes,
                errors: Vec::new(),
            });
            self.settings_view.message = Some((true, "Пробуждаем спящие приложения…".into()));
        }
    }

    /// Finalizes a reset after all wake transitions reported back.
    fn finish_reset(&mut self, mut errors: Vec<String>) {
        // Anything that completed a transition during the reset is dropped too.
        self.state.managed_groups.clear();
        if self.state.start_at_login {
            if let Err(error) = self.self_autostart.set_enabled(false) {
                errors.push(error.to_string());
            } else {
                self.state.start_at_login = false;
                self.settings_view.start_at_login = false;
            }
        }

        let ok = self.persist_state();
        self.scanner.request_refresh();
        if !errors.is_empty() || !ok {
            self.settings_view.message = Some((
                false,
                if errors.is_empty() {
                    "Не удалось сохранить state после сброса".into()
                } else {
                    errors.join("\n")
                },
            ));
        } else {
            self.settings_view.message = Some((true, "Все настройки и усыпления сброшены".into()));
            self.action_message = Some((true, "Сброс выполнен".into()));
        }
    }

    fn render_sidebar(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        ui.vertical(|ui| {
            ui.label(
                RichText::new("TuneUp")
                    .size(20.0)
                    .strong()
                    .color(theme.accent),
            );
            ui.label(RichText::new(os_subtitle()).small().color(theme.text_muted));
            ui.add_space(14.0);
            for (section, label) in [
                (MainSection::Sleep, "Сон"),
                (MainSection::Cleanup, "Очистка"),
                (MainSection::Uninstall, "Удаление"),
                (MainSection::Settings, "Настройки"),
            ] {
                let selected = self.active_section == section;
                let fill = if selected {
                    theme.accent
                } else {
                    theme.panel_raised
                };
                let text = if selected {
                    Color32::WHITE
                } else {
                    Color32::from_rgb(220, 220, 220)
                };
                let width = ui.available_width();
                let response = ui.add_sized(
                    [width, 36.0],
                    egui::Button::new(RichText::new(label).size(14.0).strong().color(text))
                        .fill(fill)
                        .corner_radius(theme.corner_radius),
                );
                if response.clicked() {
                    self.active_section = section;
                    if section == MainSection::Settings {
                        self.refresh_settings_from_os();
                    }
                }
                ui.add_space(4.0);
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                ui.label(
                    RichText::new(&self.helper_status)
                        .small()
                        .italics()
                        .color(theme.text_muted),
                );
            });
        });
    }

    fn render_sleep_header(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("Сон").size(30.0).strong().color(theme.accent));
                ui.label(
                    RichText::new("Управление фоновыми приложениями")
                        .size(14.0)
                        .color(theme.text_muted),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(egui::Button::new("Сохранить").min_size(egui::vec2(100.0, 34.0)))
                    .clicked()
                    && self.persist_state()
                {
                    self.action_message = Some((true, "Состояние сохранено".to_owned()));
                }
                if ui
                    .add(
                        egui::Button::new(RichText::new("Обновить").color(Color32::WHITE))
                            .fill(theme.accent)
                            .min_size(egui::vec2(100.0, 34.0)),
                    )
                    .clicked()
                {
                    self.scanner.request_refresh();
                }
            });
        });
        ui.add_space(18.0);

        let active = self
            .groups
            .iter()
            .filter(|group| group.status == GroupStatus::Active)
            .count();
        let sleeping = self
            .groups
            .iter()
            .filter(|group| group.status == GroupStatus::Sleeping)
            .count();
        let memory = self
            .groups
            .iter()
            .map(ProcessGroup::memory_bytes)
            .sum::<u64>();
        ui.columns(4, |columns| {
            stat_card(
                &mut columns[0],
                &theme,
                "АКТИВНЫЕ",
                &active.to_string(),
                theme.accent,
            );
            stat_card(
                &mut columns[1],
                &theme,
                "СПЯЩИЕ",
                &sleeping.to_string(),
                Color32::from_rgb(139, 148, 170),
            );
            stat_card(
                &mut columns[2],
                &theme,
                "ПРОЦЕССЫ",
                &self.processes.len().to_string(),
                Color32::from_rgb(104, 211, 145),
            );
            stat_card(
                &mut columns[3],
                &theme,
                "ПАМЯТЬ",
                &format_bytes(memory),
                Color32::from_rgb(244, 184, 96),
            );
        });
        ui.add_space(12.0);

        if !self.capabilities.system_autostart {
            notice(
                ui,
                &theme,
                "Системная автозагрузка недоступна; пользовательские операции продолжают работать.",
                Color32::from_rgb(75, 121, 209),
            );
        }
        if !self.state_ready {
            notice(
                ui,
                &theme,
                "Изменения заблокированы: журнал состояния недоступен.",
                Color32::from_rgb(196, 72, 72),
            );
        }
        if let Some((success, message)) = &self.action_message {
            notice(
                ui,
                &theme,
                message,
                if *success {
                    Color32::from_rgb(55, 143, 93)
                } else {
                    Color32::from_rgb(196, 72, 72)
                },
            );
        }
        if let Some(error) = &self.tray_error {
            notice(
                ui,
                &theme,
                &format!("Трей: {error}"),
                Color32::from_rgb(196, 72, 72),
            );
        }
        for warning in &self.warnings {
            notice(ui, &theme, warning, Color32::from_rgb(171, 124, 45));
        }
    }

    fn render_groups(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme;
        let mut action = None;
        let search = self.group_search.to_ascii_lowercase();
        egui::Frame::new()
            .fill(theme.panel)
            .stroke(egui::Stroke::new(1.0, theme.border))
            .corner_radius(theme.corner_radius)
            .inner_margin(16.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Приложения").size(18.0).strong());
                    let visible = self
                        .groups
                        .iter()
                        .filter(|group| {
                            self.status_filter.matches(group.status)
                                && group_matches_search(group, &search)
                        })
                        .count();
                    ui.label(
                        RichText::new(if visible == self.groups.len() && search.is_empty() {
                            format!("{} приложений", self.groups.len())
                        } else {
                            format!("{visible} из {} приложений", self.groups.len())
                        })
                        .color(theme.text_muted),
                    );
                    ui.add_space(12.0);
                    for (filter, label) in [
                        (StatusFilter::All, "Все"),
                        (StatusFilter::Sleeping, "Усыпленные"),
                        (StatusFilter::Awake, "Не усыпленные"),
                    ] {
                        let selected = self.status_filter == filter;
                        let button = if selected {
                            egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                .fill(theme.accent)
                        } else {
                            egui::Button::new(RichText::new(label).color(theme.text_muted))
                                .fill(theme.panel_raised)
                        };
                        if ui.add(button).clicked() {
                            self.status_filter = filter;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(
                                "Раскройте строку — процессы · двойной клик — подробности",
                            )
                            .small()
                            .color(theme.text_muted),
                        );
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Поиск").color(theme.text_muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.group_search)
                            .desired_width(280.0)
                            .hint_text("название приложения"),
                    );
                    if !self.group_search.is_empty()
                        && ui.small_button("✕").on_hover_text("Очистить").clicked()
                    {
                        self.group_search.clear();
                    }
                });
                ui.add_space(10.0);
                let search = self.group_search.to_ascii_lowercase();
                let filter = self.status_filter;
                let can_control = self.capabilities.process_control;
                let state_ready = self.state_ready;
                let rows: Vec<ProcessGroup> = self
                    .groups
                    .iter()
                    .filter(|group| {
                        filter.matches(group.status) && group_matches_search(group, &search)
                    })
                    .cloned()
                    .collect();

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for group in &rows {
                            let expanded = self.expanded_groups.contains(&group.id);
                            let busy = self.transitioning.contains_key(&group.id);
                            let sleep_enabled =
                                can_sleep(group) && can_control && state_ready && !busy;

                            egui::Frame::new()
                                .fill(theme.panel_raised)
                                .stroke(egui::Stroke::new(1.0, theme.border))
                                .corner_radius(theme.corner_radius)
                                .inner_margin(egui::Margin::symmetric(12, 10))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if expand_toggle(ui, expanded, !group.processes.is_empty())
                                            .clicked()
                                        {
                                            if expanded {
                                                self.expanded_groups.remove(&group.id);
                                            } else {
                                                self.expanded_groups.insert(group.id.clone());
                                            }
                                        }

                                        let name = ui.add(
                                            egui::Label::new(
                                                RichText::new(&group.name)
                                                    .strong()
                                                    .size(15.0)
                                                    .color(Color32::WHITE),
                                            )
                                            .sense(egui::Sense::click()),
                                        );
                                        if name.double_clicked() {
                                            action = Some(UiAction::Details(group.id.clone()));
                                        }

                                        ui.label(
                                            RichText::new(format!(
                                                "{} пр. · {} авт. · {}",
                                                group.processes.len(),
                                                group.autostart_entries.len(),
                                                format_bytes(group.memory_bytes())
                                            ))
                                            .color(theme.text_muted),
                                        );
                                        ui.label(
                                            RichText::new(status_text(group.status))
                                                .strong()
                                                .color(status_color(group.status)),
                                        );
                                        ui.label(
                                            RichText::new(running_text(group))
                                                .color(running_color(group)),
                                        );

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| match group.status {
                                                GroupStatus::Active => {
                                                    if ui
                                                        .add_enabled(
                                                            sleep_enabled,
                                                            egui::Button::new("Усыпить").fill(
                                                                Color32::from_rgb(53, 67, 96),
                                                            ),
                                                        )
                                                        .on_disabled_hover_text(
                                                            if group.processes.is_empty() {
                                                                "Нет запущенных процессов"
                                                            } else {
                                                                "Усыпление недоступно"
                                                            },
                                                        )
                                                        .clicked()
                                                    {
                                                        action =
                                                            Some(UiAction::Sleep(group.clone()));
                                                    }
                                                    if ui.small_button("Игнорировать").clicked()
                                                    {
                                                        action =
                                                            Some(UiAction::Ignore(group.clone()));
                                                    }
                                                }
                                                GroupStatus::Sleeping => {
                                                    if ui
                                                        .add_enabled(
                                                            can_control && !busy,
                                                            egui::Button::new("Разбудить").fill(
                                                                Color32::from_rgb(48, 105, 76),
                                                            ),
                                                        )
                                                        .on_disabled_hover_text(
                                                            "Переход выполняется…",
                                                        )
                                                        .clicked()
                                                    {
                                                        action =
                                                            Some(UiAction::Wake(group.id.clone()));
                                                    }
                                                }
                                                GroupStatus::Ignored => {
                                                    if ui.button("Вернуть").clicked() {
                                                        action = Some(UiAction::Restore(
                                                            group.id.clone(),
                                                        ));
                                                    }
                                                }
                                            },
                                        );
                                    });

                                    if let Some(root) = &group.install_root {
                                        ui.label(
                                            RichText::new(root.display().to_string())
                                                .small()
                                                .color(theme.text_muted),
                                        );
                                    }

                                    if expanded && !group.processes.is_empty() {
                                        ui.add_space(6.0);
                                        ui.label(
                                            RichText::new("Связанные процессы")
                                                .small()
                                                .strong()
                                                .color(theme.text_muted),
                                        );
                                        for process in &group.processes {
                                            ui.label(
                                                RichText::new(format!(
                                                    "· {} · PID {} · {:.1}% CPU · {}",
                                                    process.name,
                                                    process.pid,
                                                    process.cpu_usage,
                                                    format_bytes(process.memory_bytes)
                                                ))
                                                .small()
                                                .color(Color32::from_rgb(210, 214, 224)),
                                            );
                                        }
                                    }
                                });
                            ui.add_space(6.0);
                        }
                    });
            });
        match action {
            Some(UiAction::Sleep(group)) => {
                self.pending_sleep = Some(PendingSleep {
                    group,
                    automatic: true,
                });
            }
            Some(UiAction::Wake(id)) => self.wake_group(&id, false, false),
            Some(UiAction::Ignore(group)) => self.ignore_group(&group),
            Some(UiAction::Restore(id)) => self.restore_group(&id),
            Some(UiAction::Details(id)) => self.details_group = Some(id),
            None => {}
        }
    }

    fn render_sleep_confirmation(&mut self, context: &egui::Context) {
        let Some(pending) = &mut self.pending_sleep else {
            return;
        };
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Подтверждение")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(context, |ui| {
                if pending.group.processes.is_empty() {
                    ui.label(format!(
                        "Усыпить «{}»? Сейчас не запущено. Автозапуск будет отключён.",
                        pending.group.name,
                    ));
                } else {
                    ui.label(format!(
                        "Усыпить «{}»: {} процессов будут завершены, {} точек автозапуска отключены.",
                        pending.group.name,
                        pending.group.processes.len(),
                        pending.group.autostart_entries.len()
                    ));
                }
                ui.checkbox(
                    &mut pending.automatic,
                    "Автоматически пробуждать и повторно усыплять",
                );
                for entry in &pending.group.autostart_entries {
                    ui.small(entry.display_name());
                }
                if pending.automatic {
                    ui.colored_label(
                        Color32::YELLOW,
                        "При запуске приложения TuneUp разблокирует автозапуск. После выхода снова завершит процессы и отключит автозапуск.",
                    );
                } else {
                    ui.colored_label(
                        Color32::YELLOW,
                        "Без авто: «Разбудить» только снимет режим сна (без запуска). Пока статус «усыплено», новые запуски сами не останавливаются.",
                    );
                }
                ui.horizontal(|ui| {
                    cancel = ui.button("Отмена").clicked();
                    confirm = ui
                        .button(RichText::new("Усыпить").color(Color32::LIGHT_RED))
                        .clicked();
                });
            });
        if cancel {
            self.pending_sleep = None;
        } else if confirm {
            let pending = self.pending_sleep.take().expect("pending exists");
            self.sleep_group(
                pending.group,
                if pending.automatic {
                    GroupPolicy::AutoSleepWake
                } else {
                    GroupPolicy::Manual
                },
                false,
            );
        }
    }

    fn render_details(&mut self, context: &egui::Context) {
        let Some(group_id) = self.details_group.clone() else {
            return;
        };
        let Some(group) = self
            .groups
            .iter()
            .find(|group| group.id == group_id)
            .cloned()
        else {
            self.details_group = None;
            return;
        };
        let mut open = true;
        let original_policy = self
            .state
            .managed_groups
            .get(&group.id)
            .map(|state| state.policy);
        let mut automatic = original_policy == Some(GroupPolicy::AutoSleepWake);
        egui::Window::new(format!("Детали — {}", group.name))
            .open(&mut open)
            .resizable(true)
            .show(context, |ui| {
                ui.label(format!(
                    "Каталог: {}",
                    group
                        .install_root
                        .as_deref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "не определён".to_owned())
                ));
                if original_policy.is_some() {
                    ui.checkbox(
                        &mut automatic,
                        "Автоматически пробуждать и повторно усыплять",
                    );
                }
                if let Some(uninstall) = &group.uninstall_match {
                    ui.label(format!(
                        "Uninstall: {} · {}",
                        uninstall.display_name.as_deref().unwrap_or("без имени"),
                        uninstall
                            .publisher
                            .as_deref()
                            .unwrap_or("издатель неизвестен")
                    ));
                }
                if let Some(install) = &group.install_match {
                    ui.label(format!(
                        "Установка: {} · {}",
                        install.display_name.as_deref().unwrap_or(&install.id),
                        install.version.as_deref().unwrap_or("версия неизвестна")
                    ));
                }
                ui.heading("Процессы");
                for process in &group.processes {
                    ui.label(format!(
                        "{} · PID {} · {:.1}% CPU · {}",
                        process.name,
                        process.pid,
                        process.cpu_usage,
                        format_bytes(process.memory_bytes)
                    ));
                }
                ui.heading("Автозапуск и службы");
                for entry in &group.autostart_entries {
                    ui.colored_label(
                        if entry.is_protected() {
                            Color32::YELLOW
                        } else {
                            Color32::WHITE
                        },
                        entry.display_name(),
                    );
                }
            });
        let selected_policy = if automatic {
            GroupPolicy::AutoSleepWake
        } else {
            GroupPolicy::Manual
        };
        if original_policy.is_some() && original_policy != Some(selected_policy) {
            if let Some(state) = self.state.managed_groups.get_mut(&group.id) {
                state.policy = selected_policy;
            }
            self.persist_state();
        }
        if !open {
            self.details_group = None;
        }
    }

    fn render_sleep_section(&mut self, ui: &mut egui::Ui) {
        self.render_sleep_header(ui);
        ui.add_space(14.0);
        if self.groups.is_empty() {
            let theme = self.theme;
            egui::Frame::new()
                .fill(theme.panel)
                .stroke(egui::Stroke::new(1.0, theme.border))
                .corner_radius(theme.corner_radius)
                .inner_margin(32.0)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.spinner();
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new("Сканируем приложения…")
                                .size(16.0)
                                .color(theme.text_muted),
                        );
                    });
                });
        } else {
            self.render_groups(ui);
        }
    }
}

impl eframe::App for TuneupApp {
    fn logic(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        self.process_tray_events(context, frame);
        self.window.enforce_hidden_if_needed(context, frame);
        self.process_scanner_events();
        self.process_maintenance_events();
        self.process_transition_events();
        self.process_close_request(context, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let theme = self.theme;

        egui::Panel::left("tuneup_nav")
            .exact_size(theme.sidebar_width)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(theme.panel)
                    .stroke(egui::Stroke::new(1.0, theme.border))
                    .inner_margin(egui::Margin::symmetric(10, 12)),
            )
            .show(ui, |ui| {
                self.render_sidebar(ui);
            });

        egui::Frame::new()
            .fill(theme.background)
            .inner_margin(egui::Margin::symmetric(16, 14))
            .show(ui, |ui| match self.active_section {
                MainSection::Sleep => self.render_sleep_section(ui),
                MainSection::Cleanup => {
                    let enabled = self.capabilities.cleanup && self.state_ready;
                    match self.cleanup_view.ui(ui, &theme, enabled) {
                        CleanupAction::None => {}
                        CleanupAction::Scan => self.maintenance.scan_cleanup(),
                        CleanupAction::Clean(selection) => {
                            self.maintenance.clean(selection);
                        }
                    }
                }
                MainSection::Uninstall => {
                    let enabled = self.capabilities.uninstall && self.state_ready;
                    match self.uninstall_view.ui(ui, &theme, enabled) {
                        UninstallAction::None => {}
                        UninstallAction::Load => self.maintenance.load_applications(),
                        UninstallAction::ScanLeftovers(id) => {
                            self.maintenance.scan_leftovers(id);
                        }
                        UninstallAction::Uninstall(request) => {
                            self.maintenance.uninstall(request);
                        }
                    }
                }
                MainSection::Settings => match self.settings_view.ui(ui, &theme) {
                    SettingsAction::None => {}
                    SettingsAction::SetStartAtLogin(enabled) => {
                        self.set_start_at_login(enabled);
                    }
                    SettingsAction::ResetAll => self.reset_all(),
                },
            });

        let context = ui.ctx().clone();
        if self.active_section == MainSection::Sleep {
            self.render_sleep_confirmation(&context);
            self.render_details(&context);
        }
        // Keep tray-only startup: undo eframe revealing the window after paint.
        self.window.enforce_hidden_if_needed(&context, frame);
    }
}

fn os_subtitle() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "Desktop"
    }
}

fn stat_card(ui: &mut egui::Ui, theme: &PlatformTheme, title: &str, value: &str, color: Color32) {
    egui::Frame::new()
        .fill(theme.panel_raised)
        .stroke(egui::Stroke::new(1.0, theme.border))
        .corner_radius(theme.corner_radius)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_min_height(54.0);
            ui.label(
                RichText::new(title)
                    .size(11.0)
                    .strong()
                    .color(theme.text_muted),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(value).size(22.0).strong().color(color));
        });
}

fn notice(ui: &mut egui::Ui, theme: &PlatformTheme, message: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.65)))
        .corner_radius(theme.corner_radius)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(RichText::new(message).color(color));
        });
    ui.add_space(5.0);
}

fn status_color(status: GroupStatus) -> Color32 {
    match status {
        GroupStatus::Active => Color32::LIGHT_GREEN,
        GroupStatus::Sleeping => Color32::GRAY,
        GroupStatus::Ignored => Color32::LIGHT_BLUE,
    }
}

fn status_text(status: GroupStatus) -> &'static str {
    match status {
        GroupStatus::Active => "Active",
        GroupStatus::Sleeping => "Sleeping",
        GroupStatus::Ignored => "Ignored",
    }
}

fn running_text(group: &ProcessGroup) -> &'static str {
    if group.processes.is_empty() {
        "Не запущено"
    } else {
        "Запущено"
    }
}

fn running_color(group: &ProcessGroup) -> Color32 {
    if group.processes.is_empty() {
        Color32::from_rgb(150, 156, 170)
    } else {
        Color32::from_rgb(90, 175, 120)
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1_048_576.0;
    const GIB: f64 = 1_073_741_824.0;
    if bytes as f64 >= GIB {
        format!("{:.1} ГБ", bytes as f64 / GIB)
    } else if bytes as f64 >= MIB {
        format!("{:.0} МБ", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.0} КБ", bytes as f64 / KIB)
    } else {
        format!("{bytes} Б")
    }
}

fn group_matches_search(group: &ProcessGroup, search: &str) -> bool {
    if search.is_empty() {
        return true;
    }
    if group.name.to_ascii_lowercase().contains(search) {
        return true;
    }
    if group
        .processes
        .iter()
        .any(|process| process.name.to_ascii_lowercase().contains(search))
    {
        return true;
    }
    if let Some(root) = &group.install_root
        && root.to_string_lossy().to_ascii_lowercase().contains(search)
    {
        return true;
    }
    if let Some(install) = &group.install_match {
        if install.id.to_ascii_lowercase().contains(search) {
            return true;
        }
        if install
            .display_name
            .as_ref()
            .is_some_and(|name| name.to_ascii_lowercase().contains(search))
        {
            return true;
        }
    }
    false
}

fn can_sleep(group: &ProcessGroup) -> bool {
    if group.status != GroupStatus::Active {
        return false;
    }
    // Running processes, or a durable install root to watch after launch.
    !group.processes.is_empty() || group.install_root.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_memory() {
        assert_eq!(format_bytes(512 * 1_048_576), "512 МБ");
        assert_eq!(format_bytes(2 * 1_073_741_824), "2.0 ГБ");
        assert_eq!(format_bytes(300 * 1024), "300 КБ");
        assert_eq!(format_bytes(42), "42 Б");
    }

    #[test]
    fn status_filter_separates_sleeping_groups() {
        assert!(StatusFilter::All.matches(GroupStatus::Ignored));
        assert!(StatusFilter::Sleeping.matches(GroupStatus::Sleeping));
        assert!(!StatusFilter::Sleeping.matches(GroupStatus::Active));
        assert!(StatusFilter::Awake.matches(GroupStatus::Active));
        assert!(StatusFilter::Awake.matches(GroupStatus::Ignored));
        assert!(!StatusFilter::Awake.matches(GroupStatus::Sleeping));
    }

    #[test]
    fn group_search_matches_name_and_process() {
        let group = ProcessGroup {
            id: "1".into(),
            name: "Code".into(),
            install_root: Some(std::path::PathBuf::from(
                "/Applications/Visual Studio Code.app",
            )),
            processes: vec![ProcessInfo {
                pid: 1,
                name: "Code Helper".into(),
                path: None,
                parent_pid: None,
                start_time: 0,
                cpu_usage: 0.0,
                memory_bytes: 0,
            }],
            autostart_entries: Vec::new(),
            uninstall_match: None,
            install_match: None,
            load_score: 0.0,
            status: GroupStatus::Active,
            policy: GroupPolicy::Manual,
        };
        assert!(group_matches_search(&group, ""));
        assert!(group_matches_search(&group, "code"));
        assert!(group_matches_search(&group, "helper"));
        assert!(group_matches_search(&group, "visual studio"));
        assert!(!group_matches_search(&group, "chrome"));
    }

    #[test]
    fn can_sleep_allows_installed_app_without_processes() {
        let mut group = ProcessGroup {
            id: "1".into(),
            name: "App".into(),
            install_root: None,
            processes: Vec::new(),
            autostart_entries: Vec::new(),
            uninstall_match: None,
            install_match: None,
            load_score: 0.0,
            status: GroupStatus::Active,
            policy: GroupPolicy::Manual,
        };
        assert!(!can_sleep(&group));
        group.install_root = Some(std::path::PathBuf::from("/Applications/App.app"));
        assert!(can_sleep(&group));
        group.processes.push(ProcessInfo {
            pid: 1,
            name: "app".into(),
            path: None,
            parent_pid: None,
            start_time: 0,
            cpu_usage: 0.0,
            memory_bytes: 0,
        });
        assert!(can_sleep(&group));
        group.status = GroupStatus::Sleeping;
        assert!(!can_sleep(&group));
    }
}
