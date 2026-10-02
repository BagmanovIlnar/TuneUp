pub mod cleanup;
pub mod inventory;
pub mod ipc;
pub mod launcher;
pub mod platform;
pub mod process;
pub mod self_autostart;
pub mod uninstall;

#[cfg(windows)]
mod path_util;

use std::sync::Arc;

use tuneup_core::state::PersistedState;
use tuneup_platform::{PlatformCapabilities, PlatformServices};

pub fn services(state: &mut PersistedState) -> (PlatformServices, Option<String>) {
    let (helper, helper_status, error) =
        match ipc::load_or_create_session_key(state.ipc_session_key) {
            Ok(key) => {
                state.ipc_session_key = Some(key);
                match ipc::ElevatedClient::new(key) {
                    Ok(client) => (
                        Some(client),
                        "Windows helper готов, UAC по запросу".to_owned(),
                        None,
                    ),
                    Err(error) => (
                        None,
                        "Windows helper недоступен".to_owned(),
                        Some(error.to_string()),
                    ),
                }
            }
            Err(error) => (
                None,
                "Windows helper недоступен".to_owned(),
                Some(error.to_string()),
            ),
        };
    (
        PlatformServices {
            inventory: Arc::new(inventory::InventoryAggregator),
            process_control: Arc::new(process::ProcessController),
            app_launcher: Arc::new(launcher::WindowsAppLauncher),
            mutator: Box::new(platform::WindowsPlatformFacade::new(helper)),
            cleanup: Arc::new(cleanup::WindowsCleanupProvider::new()),
            uninstaller: Arc::new(uninstall::WindowsUninstallProvider::new()),
            self_autostart: Arc::new(self_autostart::WindowsSelfAutostart::new()),
            capabilities: PlatformCapabilities {
                process_control: cfg!(windows),
                user_autostart: cfg!(windows),
                system_autostart: cfg!(windows),
                privileged_helper: cfg!(windows),
                cleanup: cfg!(windows),
                uninstall: cfg!(windows),
            },
            helper_status,
        },
        error,
    )
}
