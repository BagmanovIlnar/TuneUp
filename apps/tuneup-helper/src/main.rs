#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    let _guard = init_logging().ok();
    if let Err(error) = run() {
        tracing::error!(%error, "helper failed");
        eprintln!("TuneUp helper: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "windows")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let key = tuneup_windows::ipc::load_session_key()?;
    let mut server = tuneup_windows::ipc::HelperServer::new(key)?;
    server.run_foreground()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;

    if std::env::args().nth(1).as_deref() != Some("--linux-system-mutation") {
        return Err("неизвестный режим Linux helper".into());
    }
    let mut payload = Vec::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_end(&mut payload)?;
    let request: tuneup_linux::SystemMutationRequest = serde_json::from_slice(&payload)?;
    tuneup_linux::platform::execute_system_mutation(&request)?;
    Ok(())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("отдельный helper на этой платформе не используется".into())
}

fn init_logging()
-> Result<tracing_appender::non_blocking::WorkerGuard, Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("TuneUp/logs");
    #[cfg(target_os = "linux")]
    let base = std::path::PathBuf::from("/var/log/TuneUp");
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    let base = std::env::temp_dir().join("TuneUp/logs");
    std::fs::create_dir_all(&base)?;
    let appender = tracing_appender::rolling::daily(base, "helper.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt().with_writer(writer).try_init()?;
    Ok(guard)
}
