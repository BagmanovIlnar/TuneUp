use tuneup_core::state::PersistedState;
use tuneup_platform::PlatformServices;

#[cfg(target_os = "windows")]
pub fn create(state: &mut PersistedState) -> (PlatformServices, Option<String>) {
    tuneup_windows::services(state)
}

#[cfg(target_os = "macos")]
pub fn create(_state: &mut PersistedState) -> (PlatformServices, Option<String>) {
    (tuneup_macos::services(), None)
}

#[cfg(target_os = "linux")]
pub fn create(_state: &mut PersistedState) -> (PlatformServices, Option<String>) {
    (tuneup_linux::services(), None)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
compile_error!("TuneUp supports Windows, macOS and Linux");
