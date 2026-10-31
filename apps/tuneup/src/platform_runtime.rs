use tuneup_platform::PlatformServices;

#[cfg(target_os = "windows")]
pub fn create() -> (PlatformServices, Option<String>) {
    tuneup_windows::services()
}

#[cfg(target_os = "macos")]
pub fn create() -> (PlatformServices, Option<String>) {
    (tuneup_macos::services(), None)
}

#[cfg(target_os = "linux")]
pub fn create() -> (PlatformServices, Option<String>) {
    (tuneup_linux::services(), None)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
compile_error!("TuneUp поддерживает только Windows, macOS и Linux");
