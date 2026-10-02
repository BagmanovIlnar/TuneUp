use tuneup_core::{
    error::IpcError,
    ipc::{HelperCommand, HelperResponse},
};

pub struct ElevatedClient {
    inner: platform::Client,
}

impl ElevatedClient {
    pub fn new(session_key: [u8; 32]) -> Result<Self, IpcError> {
        Ok(Self {
            inner: platform::Client::new(session_key)?,
        })
    }

    pub fn request(&mut self, command: HelperCommand) -> Result<HelperResponse, IpcError> {
        self.inner.request(command)
    }
}

pub struct HelperServer {
    inner: platform::Server,
}

impl HelperServer {
    pub fn new(session_key: [u8; 32]) -> Result<Self, IpcError> {
        Ok(Self {
            inner: platform::Server::new(session_key)?,
        })
    }

    pub fn run_foreground(&mut self) -> Result<(), IpcError> {
        self.inner.run()
    }
}

pub fn load_or_create_session_key(existing: Option<[u8; 32]>) -> Result<[u8; 32], IpcError> {
    platform::load_or_create_session_key(existing)
}

pub fn load_session_key() -> Result<[u8; 32], IpcError> {
    platform::load_session_key()
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub struct Client;
    pub struct Server;

    impl Client {
        pub fn new(_key: [u8; 32]) -> Result<Self, IpcError> {
            Ok(Self)
        }

        pub fn request(&mut self, _command: HelperCommand) -> Result<HelperResponse, IpcError> {
            Err(IpcError::BrokenPipe(
                "helper поддерживается только в Windows".to_owned(),
            ))
        }
    }

    impl Server {
        pub fn new(_key: [u8; 32]) -> Result<Self, IpcError> {
            Ok(Self)
        }

        pub fn run(&mut self) -> Result<(), IpcError> {
            Err(IpcError::BrokenPipe(
                "helper поддерживается только в Windows".to_owned(),
            ))
        }
    }

    pub fn load_or_create_session_key(existing: Option<[u8; 32]>) -> Result<[u8; 32], IpcError> {
        Ok(existing.unwrap_or([0; 32]))
    }

    pub fn load_session_key() -> Result<[u8; 32], IpcError> {
        Ok([0; 32])
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        ffi::OsString,
        fs, io,
        os::windows::ffi::OsStringExt,
        path::{Path, PathBuf},
        ptr,
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use tuneup_core::{
        error::IpcError,
        ipc::{
            HelperCommand, HelperError, HelperErrorCode, HelperResponse, IPC_PROTOCOL_VERSION,
            IpcRequest, IpcResponse, PIPE_NAME,
        },
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, HANDLE, INVALID_HANDLE_VALUE,
            LocalFree,
        },
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
        },
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
                GetNamedPipeClientProcessId, PIPE_NOWAIT, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE,
                PIPE_WAIT, SetNamedPipeHandleState, WaitNamedPipeW,
            },
            Threading::{
                GetCurrentProcess, OpenProcess, OpenProcessToken,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_HIDE},
    };

    use crate::{
        path_util::{to_wide, wide_ptr_to_os_string},
        platform::ElevatedPlatformExecutor,
    };

    const MAX_FRAME: usize = 4 * 1024 * 1024;
    const HELPER_IDLE: Duration = Duration::from_secs(300);

    pub struct Client {
        session_key: [u8; 32],
        helper_path: PathBuf,
        nonce: AtomicU64,
    }

    impl Client {
        pub fn new(session_key: [u8; 32]) -> Result<Self, IpcError> {
            write_session_key(&session_key)?;
            let current =
                std::env::current_exe().map_err(|error| IpcError::BrokenPipe(error.to_string()))?;
            let helper_path = current
                .parent()
                .ok_or_else(|| IpcError::BrokenPipe("нет каталога exe".to_owned()))?
                .join("tuneup-helper.exe");
            Ok(Self {
                session_key,
                helper_path,
                nonce: AtomicU64::new(initial_nonce()),
            })
        }

        pub fn request(&mut self, command: HelperCommand) -> Result<HelperResponse, IpcError> {
            let handle = match connect_pipe() {
                Ok(handle) => handle,
                Err(_) => {
                    self.spawn_helper()?;
                    wait_for_pipe()?;
                    connect_pipe()?
                }
            };
            let handle = OwnedHandle(handle);
            let nonce = self.nonce.fetch_add(1, Ordering::Relaxed);
            let request = IpcRequest::signed(nonce, command, &self.session_key)?;
            write_json(handle.0, &request)?;
            let response: IpcResponse = read_json(handle.0)?;
            if response.version != IPC_PROTOCOL_VERSION || response.nonce != nonce {
                return Err(IpcError::AuthFailed);
            }
            response
                .result
                .map_err(|error| IpcError::Remote(error.message))
        }

        fn spawn_helper(&self) -> Result<(), IpcError> {
            if !self.helper_path.exists() {
                return Err(IpcError::BrokenPipe(format!(
                    "{} не найден",
                    self.helper_path.display()
                )));
            }
            let operation = to_wide("runas".as_ref());
            let file = to_wide(self.helper_path.as_os_str());
            let parameters = to_wide("--server".as_ref());
            // SAFETY: All strings are NUL-terminated and helper path is verified above.
            let result = unsafe {
                ShellExecuteW(
                    ptr::null_mut(),
                    operation.as_ptr(),
                    file.as_ptr(),
                    parameters.as_ptr(),
                    ptr::null(),
                    SW_HIDE,
                )
            };
            if result as isize <= 32 {
                return Err(IpcError::BrokenPipe(format!(
                    "UAC/helper: код {}",
                    result as isize
                )));
            }
            Ok(())
        }
    }

    pub struct Server {
        session_key: [u8; 32],
        last_nonce: u64,
        expected_directory: PathBuf,
    }

    impl Server {
        pub fn new(session_key: [u8; 32]) -> Result<Self, IpcError> {
            let current =
                std::env::current_exe().map_err(|error| IpcError::BrokenPipe(error.to_string()))?;
            let expected_directory = current
                .parent()
                .ok_or_else(|| IpcError::BrokenPipe("нет каталога helper".to_owned()))?
                .to_path_buf();
            Ok(Self {
                session_key,
                last_nonce: 0,
                expected_directory,
            })
        }

        pub fn run(&mut self) -> Result<(), IpcError> {
            let mut idle_started = Instant::now();
            loop {
                let security = PipeSecurity::new()?;
                let name = to_wide(PIPE_NAME.as_ref());
                // SAFETY: Security attributes and pipe name live through this call.
                let pipe = unsafe {
                    CreateNamedPipeW(
                        name.as_ptr(),
                        PIPE_ACCESS_DUPLEX,
                        PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_NOWAIT,
                        1,
                        65_536,
                        65_536,
                        0,
                        &security.attributes,
                    )
                };
                if pipe == INVALID_HANDLE_VALUE {
                    return Err(last_pipe_error());
                }
                let pipe = OwnedHandle(pipe);
                loop {
                    // SAFETY: Pipe handle is valid and non-overlapped.
                    let connected = unsafe { ConnectNamedPipe(pipe.0, ptr::null_mut()) };
                    if connected != 0 {
                        break;
                    }
                    let code = io::Error::last_os_error()
                        .raw_os_error()
                        .unwrap_or_default() as u32;
                    if code == ERROR_PIPE_CONNECTED {
                        break;
                    }
                    if code != ERROR_PIPE_LISTENING {
                        return Err(last_pipe_error());
                    }
                    if idle_started.elapsed() >= HELPER_IDLE {
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                let wait_mode = PIPE_READMODE_MESSAGE | PIPE_WAIT;
                // SAFETY: Mode pointer is valid and optional settings are null.
                unsafe {
                    SetNamedPipeHandleState(pipe.0, &wait_mode, ptr::null(), ptr::null());
                }
                let response = self.handle_connection(pipe.0);
                if let Ok(response) = response {
                    let _ = write_json(pipe.0, &response);
                }
                idle_started = Instant::now();
                // SAFETY: Connected server pipe may be disconnected before close.
                unsafe {
                    DisconnectNamedPipe(pipe.0);
                }
            }
        }

        fn handle_connection(&mut self, pipe: HANDLE) -> Result<IpcResponse, IpcError> {
            verify_client_process(pipe, &self.expected_directory)?;
            let request: IpcRequest = read_json(pipe)?;
            request.verify(&self.session_key)?;
            if request.nonce <= self.last_nonce {
                return Err(IpcError::AuthFailed);
            }
            self.last_nonce = request.nonce;
            let result = dispatch(request.command).map_err(|error| HelperError {
                code: match error {
                    tuneup_core::error::PlatformError::AccessDenied(_) => {
                        HelperErrorCode::AccessDenied
                    }
                    tuneup_core::error::PlatformError::ProtectedObject(_) => {
                        HelperErrorCode::InvalidRequest
                    }
                    _ => HelperErrorCode::Internal,
                },
                message: error.to_string(),
            });
            Ok(IpcResponse {
                version: IPC_PROTOCOL_VERSION,
                nonce: request.nonce,
                result,
            })
        }
    }

    fn dispatch(
        command: HelperCommand,
    ) -> Result<HelperResponse, tuneup_core::error::PlatformError> {
        tracing::info!(
            command = match &command {
                HelperCommand::Ping => "ping",
                HelperCommand::GetCapabilities => "get_capabilities",
                HelperCommand::ApplyPlatform(_) => "apply_platform",
                HelperCommand::RestorePlatform(_) => "restore_platform",
                HelperCommand::CleanPaths(_) => "clean_paths",
                HelperCommand::UninstallPackage(_) => "uninstall_package",
            },
            "dispatching elevated helper command"
        );
        let executor = ElevatedPlatformExecutor;
        match command {
            HelperCommand::Ping => Ok(HelperResponse::Pong {
                elevated: true,
                build: env!("CARGO_PKG_VERSION").to_owned(),
            }),
            HelperCommand::GetCapabilities => Ok(HelperResponse::Capabilities(
                tuneup_core::ipc::CapabilityFlags {
                    hklm_run: true,
                    services: true,
                    tasks: true,
                    common_startup: true,
                    cleanup: true,
                    uninstall: true,
                    ..tuneup_core::ipc::CapabilityFlags::default()
                },
            )),
            HelperCommand::ApplyPlatform(request) => {
                let report = executor.apply(&request);
                Ok(HelperResponse::PlatformApplied {
                    backup: report.backup,
                    errors: report
                        .errors
                        .into_iter()
                        .map(|error| error.to_string())
                        .collect(),
                })
            }
            HelperCommand::RestorePlatform(backup) => executor
                .restore(backup)
                .map(|remaining| HelperResponse::PlatformRestored { remaining }),
            HelperCommand::CleanPaths(request) => {
                Ok(HelperResponse::Cleanup(elevated_clean_paths(request)?))
            }
            HelperCommand::UninstallPackage(request) => Ok(HelperResponse::Uninstall(
                elevated_uninstall_package(request)?,
            )),
        }
    }

    fn elevated_clean_paths(
        request: tuneup_core::ipc::CleanPathsRequest,
    ) -> Result<tuneup_core::cleanup::CleanupReport, tuneup_core::error::PlatformError> {
        use std::fs;
        use tuneup_core::fs_util::{directory_size, is_protected_root};

        let allowlist = [
            std::path::Path::new(r"C:\Windows\Temp"),
            std::path::Path::new(r"C:\Windows\SoftwareDistribution\Download"),
        ];
        let mut report = tuneup_core::cleanup::CleanupReport::default();
        if request.paths.len() > 64 {
            report
                .failed
                .push("слишком много путей в одном запросе".into());
            return Ok(report);
        }
        for path in request.paths {
            let Ok(canonical) = fs::canonicalize(&path) else {
                report.failed.push(path.display().to_string());
                continue;
            };
            if is_protected_root(&canonical)
                || !allowlist
                    .iter()
                    .any(|root| canonical == *root || canonical.starts_with(root))
            {
                report.failed.push(canonical.display().to_string());
                continue;
            }
            let size = directory_size(&canonical);
            let clear = if canonical.is_dir() {
                clear_dir_contents(&canonical)
            } else {
                fs::remove_file(&canonical).map_err(|error| error.to_string())
            };
            match clear {
                Ok(()) => report.freed_bytes = report.freed_bytes.saturating_add(size),
                Err(error) => report
                    .failed
                    .push(format!("{}: {error}", canonical.display())),
            }
        }
        Ok(report)
    }

    fn clear_dir_contents(path: &std::path::Path) -> Result<(), String> {
        use std::fs;
        for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let child = entry.path();
            let metadata = fs::symlink_metadata(&child).map_err(|error| error.to_string())?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                fs::remove_dir_all(&child).map_err(|error| error.to_string())?;
            } else {
                fs::remove_file(&child).map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn elevated_uninstall_package(
        request: tuneup_core::ipc::UninstallPackageRequest,
    ) -> Result<tuneup_core::uninstall::UninstallReport, tuneup_core::error::PlatformError> {
        use std::process::Command;
        use tuneup_core::error::PlatformError;

        let identifier = request.identifier;
        if identifier.len() > 256
            || !identifier.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '-' | '_' | '{' | '}' | '.')
            })
        {
            return Err(PlatformError::ProtectedObject(identifier));
        }
        let status = match request.kind.as_str() {
            "msi" => Command::new("msiexec")
                .args(["/x", &identifier, "/qn"])
                .status()
                .map_err(|error| PlatformError::HelperDenied(error.to_string()))?,
            other => {
                return Err(PlatformError::Unsupported(format!(
                    "неизвестный elevated uninstall kind: {other}"
                )));
            }
        };
        Ok(tuneup_core::uninstall::UninstallReport {
            uninstalled: status.success(),
            errors: if status.success() {
                Vec::new()
            } else {
                vec![format!("msiexec завершился с кодом {status}")]
            },
            ..tuneup_core::uninstall::UninstallReport::default()
        })
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: Handle is owned and closed once.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    struct PipeSecurity {
        descriptor: PSECURITY_DESCRIPTOR,
        attributes: SECURITY_ATTRIBUTES,
    }

    impl PipeSecurity {
        fn new() -> Result<Self, IpcError> {
            let sid = current_user_sid()?;
            let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{sid})");
            let wide = to_wide(sddl.as_ref());
            let mut descriptor = ptr::null_mut();
            // SAFETY: SDDL is NUL-terminated and output pointer is valid.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_pipe_error());
            }
            Ok(Self {
                descriptor,
                attributes: SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor,
                    bInheritHandle: 0,
                },
            })
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            // SAFETY: Descriptor was allocated by the conversion API.
            unsafe {
                LocalFree(self.descriptor);
            }
        }
    }

    fn current_user_sid() -> Result<String, IpcError> {
        let mut token = ptr::null_mut();
        // SAFETY: Current process pseudo-handle is valid and output pointer is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(last_pipe_error());
        }
        let token = OwnedHandle(token);
        let mut needed = 0;
        // SAFETY: First call requests required size.
        unsafe {
            GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed);
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: Buffer has requested size.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(last_pipe_error());
        }
        // SAFETY: Buffer contains TOKEN_USER from successful API call.
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_string = ptr::null_mut();
        // SAFETY: SID came from TOKEN_USER and output pointer is valid.
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
            return Err(last_pipe_error());
        }
        let text = wide_ptr_to_os_string(sid_string)
            .to_string_lossy()
            .into_owned();
        // SAFETY: String SID was allocated by LocalAlloc.
        unsafe {
            LocalFree(sid_string.cast());
        }
        Ok(text)
    }

    fn verify_client_process(pipe: HANDLE, expected_directory: &Path) -> Result<(), IpcError> {
        let mut pid = 0;
        // SAFETY: Pipe is connected and output pointer is valid.
        if unsafe { GetNamedPipeClientProcessId(pipe, &mut pid) } == 0 {
            return Err(IpcError::AuthFailed);
        }
        // SAFETY: PID is supplied by the kernel for this pipe connection.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(IpcError::AuthFailed);
        }
        let process = OwnedHandle(process);
        let mut buffer = vec![0u16; 32_768];
        let mut size = buffer.len() as u32;
        // SAFETY: Buffer and size pointer are valid.
        if unsafe { QueryFullProcessImageNameW(process.0, 0, buffer.as_mut_ptr(), &mut size) } == 0
        {
            return Err(IpcError::AuthFailed);
        }
        let path = PathBuf::from(OsString::from_wide(&buffer[..size as usize]));
        let same_directory = path
            .parent()
            .and_then(|parent| fs::canonicalize(parent).ok())
            == fs::canonicalize(expected_directory).ok();
        let valid_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("tuneup.exe"));
        if same_directory && valid_name {
            Ok(())
        } else {
            Err(IpcError::AuthFailed)
        }
    }

    fn connect_pipe() -> Result<HANDLE, IpcError> {
        let name = to_wide(PIPE_NAME.as_ref());
        // SAFETY: Pipe name is NUL-terminated and remaining optional arguments are valid.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                0,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(last_pipe_error())
        } else {
            Ok(handle)
        }
    }

    fn wait_for_pipe() -> Result<(), IpcError> {
        let name = to_wide(PIPE_NAME.as_ref());
        // SAFETY: Pipe name is NUL-terminated.
        if unsafe { WaitNamedPipeW(name.as_ptr(), 30_000) } == 0 {
            Err(IpcError::Timeout)
        } else {
            Ok(())
        }
    }

    fn write_json<T: serde::Serialize>(handle: HANDLE, value: &T) -> Result<(), IpcError> {
        let bytes =
            serde_json::to_vec(value).map_err(|error| IpcError::Serialize(error.to_string()))?;
        if bytes.len() > MAX_FRAME {
            return Err(IpcError::Serialize("слишком большой IPC frame".to_owned()));
        }
        write_all(handle, &(bytes.len() as u32).to_le_bytes())?;
        write_all(handle, &bytes)
    }

    fn read_json<T: serde::de::DeserializeOwned>(handle: HANDLE) -> Result<T, IpcError> {
        let mut length = [0u8; 4];
        read_exact(handle, &mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_FRAME {
            return Err(IpcError::Serialize("слишком большой IPC frame".to_owned()));
        }
        let mut bytes = vec![0u8; length];
        read_exact(handle, &mut bytes)?;
        serde_json::from_slice(&bytes).map_err(|error| IpcError::Serialize(error.to_string()))
    }

    fn write_all(handle: HANDLE, mut bytes: &[u8]) -> Result<(), IpcError> {
        while !bytes.is_empty() {
            let mut written = 0;
            // SAFETY: Slice pointer/length and output pointer are valid.
            if unsafe {
                WriteFile(
                    handle,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_pipe_error());
            }
            bytes = &bytes[written as usize..];
        }
        Ok(())
    }

    fn read_exact(handle: HANDLE, mut bytes: &mut [u8]) -> Result<(), IpcError> {
        while !bytes.is_empty() {
            let mut read = 0;
            // SAFETY: Slice pointer/length and output pointer are valid.
            if unsafe {
                ReadFile(
                    handle,
                    bytes.as_mut_ptr(),
                    bytes.len() as u32,
                    &mut read,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(last_pipe_error());
            }
            if read == 0 {
                return Err(IpcError::BrokenPipe("EOF".to_owned()));
            }
            let (_, rest) = bytes.split_at_mut(read as usize);
            bytes = rest;
        }
        Ok(())
    }

    pub fn load_or_create_session_key(existing: Option<[u8; 32]>) -> Result<[u8; 32], IpcError> {
        let key = match existing {
            Some(key) => key,
            None => {
                let mut key = [0u8; 32];
                // SAFETY: Buffer is valid; system-preferred RNG requires no algorithm handle.
                let status = unsafe {
                    windows_sys::Win32::Security::Cryptography::BCryptGenRandom(
                        ptr::null_mut(),
                        key.as_mut_ptr(),
                        key.len() as u32,
                        windows_sys::Win32::Security::Cryptography::BCRYPT_USE_SYSTEM_PREFERRED_RNG,
                    )
                };
                if status < 0 {
                    return Err(IpcError::BrokenPipe(format!(
                        "BCryptGenRandom: 0x{:08X}",
                        status as u32
                    )));
                }
                key
            }
        };
        write_session_key(&key)?;
        Ok(key)
    }

    pub fn load_session_key() -> Result<[u8; 32], IpcError> {
        let bytes =
            fs::read(session_path()?).map_err(|error| IpcError::BrokenPipe(error.to_string()))?;
        bytes.try_into().map_err(|_| IpcError::AuthFailed)
    }

    fn write_session_key(key: &[u8; 32]) -> Result<(), IpcError> {
        let path = session_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| IpcError::BrokenPipe("нет каталога session".to_owned()))?;
        fs::create_dir_all(parent).map_err(|error| IpcError::BrokenPipe(error.to_string()))?;
        fs::write(path, key).map_err(|error| IpcError::BrokenPipe(error.to_string()))
    }

    fn session_path() -> Result<PathBuf, IpcError> {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("TuneUp/helper.session"))
            .ok_or_else(|| IpcError::BrokenPipe("LOCALAPPDATA не определён".to_owned()))
    }

    fn initial_nonce() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64
    }

    fn last_pipe_error() -> IpcError {
        IpcError::BrokenPipe(io::Error::last_os_error().to_string())
    }
}
