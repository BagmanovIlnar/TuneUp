//! Linux platform services for TuneUp.
//!
//! The crate deliberately executes programs with fixed argument vectors and
//! never invokes a shell. System-wide mutations require an explicit
//! [`SystemMutationHelper`] implementation; the default facade only mutates
//! files and units owned by the current user.

pub mod cleanup;
pub mod inventory;
pub mod launcher;
pub mod platform;
pub mod process;
pub mod self_autostart;
pub mod uninstall;

pub use cleanup::LinuxCleanupProvider;
pub use inventory::LinuxInventoryProvider;
pub use launcher::LinuxAppLauncher;
pub use platform::{
    LinuxPlatformMutator, NoSystemMutationHelper, PkexecSystemMutationHelper, SystemMutationHelper,
    SystemMutationRequest,
};
pub use process::LinuxProcessControl;
pub use self_autostart::LinuxSelfAutostart;
pub use uninstall::LinuxUninstallProvider;

use std::sync::Arc;

use tuneup_platform::{PlatformCapabilities, PlatformServices};

/// Builds Linux services without a privileged system helper.
///
/// User XDG entries, user systemd units, and process signals are available.
/// Attempts to mutate system XDG entries or system units return typed
/// `HelperUnavailable` errors.
pub fn services() -> PlatformServices {
    let helper = Arc::new(PkexecSystemMutationHelper);
    PlatformServices {
        inventory: Arc::new(LinuxInventoryProvider),
        process_control: Arc::new(LinuxProcessControl),
        app_launcher: Arc::new(LinuxAppLauncher),
        mutator: Box::new(LinuxPlatformMutator::new(helper)),
        cleanup: Arc::new(LinuxCleanupProvider::new()),
        uninstaller: Arc::new(LinuxUninstallProvider::new()),
        self_autostart: Arc::new(LinuxSelfAutostart::new()),
        capabilities: PlatformCapabilities {
            process_control: cfg!(target_os = "linux"),
            user_autostart: cfg!(target_os = "linux"),
            system_autostart: cfg!(target_os = "linux"),
            privileged_helper: cfg!(target_os = "linux"),
            cleanup: cfg!(target_os = "linux"),
            uninstall: cfg!(target_os = "linux"),
        },
        helper_status: "Linux pkexec/Polkit готов по запросу".to_owned(),
    }
}

/// Builds Linux services with a caller-supplied privileged helper.
///
/// The helper is expected to authenticate and authorize requests (for example
/// through `pkexec` and the bundled polkit policy) before changing system
/// files or systemd system units.
pub fn services_with_helper(helper: Arc<dyn SystemMutationHelper>) -> PlatformServices {
    PlatformServices {
        inventory: Arc::new(LinuxInventoryProvider),
        process_control: Arc::new(LinuxProcessControl),
        app_launcher: Arc::new(LinuxAppLauncher),
        mutator: Box::new(LinuxPlatformMutator::new(helper)),
        cleanup: Arc::new(LinuxCleanupProvider::new()),
        uninstaller: Arc::new(LinuxUninstallProvider::new()),
        self_autostart: Arc::new(LinuxSelfAutostart::new()),
        capabilities: PlatformCapabilities {
            process_control: cfg!(target_os = "linux"),
            user_autostart: cfg!(target_os = "linux"),
            system_autostart: cfg!(target_os = "linux"),
            privileged_helper: true,
            cleanup: cfg!(target_os = "linux"),
            uninstall: cfg!(target_os = "linux"),
        },
        helper_status: "системный helper настроен".to_owned(),
    }
}
