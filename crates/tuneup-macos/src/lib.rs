//! macOS inventory and safe platform controls for TuneUp.
//!
//! The public types implement the platform-neutral traits from `tuneup-platform`.
//! On targets other than macOS they remain constructible and return supported
//! empty inventory or typed `Unsupported` errors.

pub mod cleanup;
pub mod inventory;
pub mod launcher;
pub mod path_util;
pub mod platform;
pub mod process;
pub mod self_autostart;
pub mod uninstall;

pub use cleanup::MacOsCleanupProvider;
pub use inventory::MacOsInventoryProvider;
pub use launcher::MacOsAppLauncher;
pub use platform::MacOsPlatformMutator;
pub use process::MacOsProcessControl;
pub use self_autostart::MacOsSelfAutostart;
pub use uninstall::MacOsUninstallProvider;

use std::sync::Arc;

use tuneup_platform::{PlatformCapabilities, PlatformServices};

pub fn services() -> PlatformServices {
    PlatformServices {
        inventory: Arc::new(MacOsInventoryProvider),
        process_control: Arc::new(MacOsProcessControl),
        app_launcher: Arc::new(MacOsAppLauncher),
        mutator: Box::new(MacOsPlatformMutator::new()),
        cleanup: Arc::new(MacOsCleanupProvider::new()),
        uninstaller: Arc::new(MacOsUninstallProvider::new()),
        self_autostart: Arc::new(MacOsSelfAutostart::new()),
        capabilities: PlatformCapabilities {
            process_control: cfg!(target_os = "macos"),
            user_autostart: cfg!(target_os = "macos"),
            system_autostart: cfg!(target_os = "macos"),
            privileged_helper: cfg!(target_os = "macos"),
            cleanup: cfg!(target_os = "macos"),
            uninstall: cfg!(target_os = "macos"),
        },
        helper_status: "macOS: права администратора запрашиваются по необходимости".to_owned(),
    }
}
