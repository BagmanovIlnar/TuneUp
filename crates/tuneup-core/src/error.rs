use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TuneupError {
    #[error(transparent)]
    Process(#[from] ProcessError),
    #[error(transparent)]
    Platform(#[from] PlatformError),
    #[error(transparent)]
    Ipc(#[from] IpcError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("инвентаризация: {0}")]
    Inventory(String),
    #[error("политика: {0}")]
    Policy(String),
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("процесс PID {pid} защищён: {reason}")]
    Protected { pid: u32, reason: String },
    #[error("PID {pid} уже принадлежит другому процессу")]
    IdentityMismatch { pid: u32 },
    #[error("Windows пометила PID {pid} как критический")]
    Critical { pid: u32 },
    #[error("не удалось открыть PID {pid}: {detail}")]
    OpenFailed { pid: u32, detail: String },
    #[error("PID {pid}: NTSTATUS 0x{status:08X}")]
    NtStatus { pid: u32, status: u32 },
    #[error("не удалось отправить сигнал процессу PID {pid}: {detail}")]
    SignalFailed { pid: u32, detail: String },
}

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("elevated-helper недоступен")]
    HelperUnavailable,
    #[error("операция отклонена helper: {0}")]
    HelperDenied(String),
    #[error("доступ запрещён: {0}")]
    AccessDenied(String),
    #[error("небезопасный системный объект: {0}")]
    ProtectedObject(String),
    #[error("не удалось изменить {path}: {detail}")]
    Mutation { path: PathBuf, detail: String },
    #[error("частичная операция: {0}")]
    Partial(String),
    #[error("операция не поддерживается текущей платформой: {0}")]
    Unsupported(String),
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("тайм-аут IPC")]
    Timeout,
    #[error("IPC-канал недоступен: {0}")]
    BrokenPipe(String),
    #[error("клиент IPC не прошёл проверку")]
    AuthFailed,
    #[error("несовместимая версия IPC: {0}")]
    VersionMismatch(u32),
    #[error("ошибка сериализации IPC: {0}")]
    Serialize(String),
    #[error("helper вернул ошибку: {0}")]
    Remote(String),
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("не удалось определить каталог состояния")]
    DirectoryUnavailable,
    #[error("ошибка файла состояния: {0}")]
    Io(String),
    #[error("повреждён файл состояния: {0}")]
    Corrupt(String),
    #[error("версия состояния {0} не поддерживается")]
    VersionUnsupported(u32),
}
