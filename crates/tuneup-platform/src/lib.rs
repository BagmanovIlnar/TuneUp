use std::sync::Arc;

use tuneup_core::{
    TuneupError,
    cleanup::{CleanupReport, CleanupScan, CleanupSelection},
    model::{InventorySnapshot, PlatformBackup, ProcessGroup, ProcessIdentity, ProcessInfo},
    uninstall::{
        InstalledApplication, LeftoverItem, LeftoverSelection, UninstallReport, UninstallRequest,
    },
};

pub mod cleanup_util;
pub mod uninstall_util;

#[derive(Debug, Clone, Copy, Default)]
pub struct PlatformCapabilities {
    pub process_control: bool,
    pub user_autostart: bool,
    pub system_autostart: bool,
    pub privileged_helper: bool,
    pub cleanup: bool,
    pub uninstall: bool,
}

pub struct PlatformChangeReport {
    pub backup: PlatformBackup,
    pub errors: Vec<TuneupError>,
}

pub trait InventoryProvider: Send + Sync {
    fn snapshot(&self) -> Result<InventorySnapshot, TuneupError>;
}

pub trait ProcessControl: Send + Sync {
    /// Force-terminates processes. Already-exited PIDs count as success.
    ///
    /// On partial failure returns still-alive identities together with errors.
    fn terminate(
        &self,
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)>;

    /// Force-terminates previously recorded identities (crash recovery / migration).
    fn terminate_identities(
        &self,
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>);
}

/// Starts an application from its install root (used by Sleep Mode wake).
pub trait AppLauncher: Send + Sync {
    fn launch(&self, install_root: &std::path::Path, display_name: &str) -> Result<(), TuneupError>;
}

pub trait PlatformMutator: Send {
    fn disable_for_group(&mut self, group: &ProcessGroup) -> PlatformChangeReport;
    fn restore(&mut self, backup: PlatformBackup) -> PlatformChangeReport;
}

/// Discovers and removes disposable temporary/cache files.
pub trait CleanupProvider: Send + Sync {
    /// Scans safe cleanup candidates for the current user and OS.
    fn scan(&self) -> Result<CleanupScan, TuneupError>;

    /// Deletes previously scanned items selected by identifier.
    fn clean(&self, selection: &CleanupSelection) -> Result<CleanupReport, TuneupError>;
}

/// Lists installed applications and removes them with leftover cleanup.
pub trait UninstallProvider: Send + Sync {
    /// Returns removable and protected applications known to the current OS.
    fn installed_applications(&self) -> Result<Vec<InstalledApplication>, TuneupError>;

    /// Finds leftover paths associated with an application.
    fn find_leftovers(&self, application_id: &str) -> Result<Vec<LeftoverItem>, TuneupError>;

    /// Runs the native uninstall flow and then removes selected leftovers.
    fn uninstall(&self, request: &UninstallRequest) -> Result<UninstallReport, TuneupError>;

    /// Removes leftovers after the primary uninstall already completed.
    fn remove_leftovers(
        &self,
        selection: &LeftoverSelection,
    ) -> Result<UninstallReport, TuneupError>;
}

/// Enables or disables TuneUp starting automatically at user login.
pub trait SelfAutostartProvider: Send + Sync {
    /// Whether TuneUp is currently registered to start at login.
    fn is_enabled(&self) -> Result<bool, TuneupError>;

    /// Registers or removes TuneUp from the current user's login startup.
    fn set_enabled(&self, enabled: bool) -> Result<(), TuneupError>;
}

pub struct PlatformServices {
    pub inventory: Arc<dyn InventoryProvider>,
    pub process_control: Arc<dyn ProcessControl>,
    pub app_launcher: Arc<dyn AppLauncher>,
    pub mutator: Box<dyn PlatformMutator>,
    pub cleanup: Arc<dyn CleanupProvider>,
    pub uninstaller: Arc<dyn UninstallProvider>,
    pub self_autostart: Arc<dyn SelfAutostartProvider>,
    pub capabilities: PlatformCapabilities,
    pub helper_status: String,
}
