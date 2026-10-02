use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

pub struct LoggingGuard {
    _guard: WorkerGuard,
}

pub fn init() -> Result<LoggingGuard, Box<dyn std::error::Error + Send + Sync>> {
    let directory = log_directory()?;
    std::fs::create_dir_all(&directory)?;
    let appender = tracing_appender::rolling::daily(directory, "tuneup.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_env("TUNEUP_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_writer(writer))
        .try_init()?;
    Ok(LoggingGuard { _guard: guard })
}

fn log_directory() -> Result<PathBuf, &'static str> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Logs"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    base.map(|base| base.join("TuneUp/logs"))
        .ok_or("не найден каталог логов")
}
