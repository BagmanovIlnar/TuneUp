use tuneup_core::{error::TuneupError, model::InventorySnapshot};

pub struct InventoryAggregator;

impl InventoryAggregator {
    pub fn snapshot(&self) -> Result<InventorySnapshot, TuneupError> {
        platform::snapshot()
    }
}

impl tuneup_platform::InventoryProvider for InventoryAggregator {
    fn snapshot(&self) -> Result<InventorySnapshot, TuneupError> {
        InventoryAggregator::snapshot(self)
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn snapshot() -> Result<InventorySnapshot, TuneupError> {
        Ok(InventorySnapshot::default())
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        ffi::OsString,
        os::windows::ffi::OsStringExt,
        path::{Path, PathBuf},
    };

    use tuneup_core::{
        error::TuneupError,
        model::{
            AutoStartEntry, ExecutableRef, InventorySnapshot, RegistryHive, StartupFolderKind,
            UninstallRecord, Wow64View,
        },
    };
    use windows::{
        Win32::{
            Foundation::{ERROR_MORE_DATA, VARIANT_BOOL},
            Storage::FileSystem::WIN32_FIND_DATAW,
            System::{
                Com::{
                    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                    CoUninitialize, IPersistFile, STGM_READ,
                },
                Services::{
                    CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW,
                    OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW, QueryServiceConfigW,
                    SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_ENUMERATE_SERVICE,
                    SERVICE_QUERY_CONFIG, SERVICE_RUNNING, SERVICE_STATE_ALL, SERVICE_WIN32,
                },
                TaskScheduler::{
                    IActionCollection, IExecAction, IRegisteredTask, ITaskFolder, ITaskService,
                    TASK_ENUM_HIDDEN, TaskScheduler,
                },
                Variant::VARIANT,
            },
            UI::Shell::{IShellLinkW, SLGP_RAWPATH, ShellLink},
        },
        core::{BSTR, Interface, PCWSTR},
    };
    use winreg::{
        HKCU, HKLM,
        enums::{KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY},
        types::FromRegValue,
    };

    use crate::path_util::{
        command_executable, expand_environment, protected_system_path, to_wide,
        wide_ptr_to_os_string,
    };

    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_ONCE_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\RunOnce";

    pub fn snapshot() -> Result<InventorySnapshot, TuneupError> {
        let mut warnings = Vec::new();
        let uninstall = collect_or_warn(UninstallRegistryScanner.scan(), &mut warnings);
        let mut autostart = collect_or_warn(RegistryRunScanner.scan(), &mut warnings);
        autostart.extend(collect_or_warn(StartupFolderScanner.scan(), &mut warnings));
        autostart.extend(collect_or_warn(TaskSchedulerScanner.scan(), &mut warnings));
        autostart.extend(collect_or_warn(ServiceScanner.scan(), &mut warnings));
        Ok(InventorySnapshot {
            uninstall,
            installs: Vec::new(),
            autostart,
            warnings,
        })
    }

    fn collect_or_warn<T>(result: Result<Vec<T>, String>, warnings: &mut Vec<String>) -> Vec<T> {
        match result {
            Ok(values) => values,
            Err(error) => {
                warnings.push(error);
                Vec::new()
            }
        }
    }

    pub struct UninstallRegistryScanner;

    impl UninstallRegistryScanner {
        pub fn scan(&self) -> Result<Vec<UninstallRecord>, String> {
            let mut records = Vec::new();
            for (root, hive) in [
                (HKCU, RegistryHive::CurrentUser),
                (HKLM, RegistryHive::LocalMachine),
            ] {
                for (flags, _view) in views() {
                    let Ok(uninstall) =
                        root.open_subkey_with_flags(UNINSTALL_KEY, KEY_READ | flags)
                    else {
                        continue;
                    };
                    for key_name in uninstall.enum_keys().filter_map(Result::ok) {
                        let Ok(key) = uninstall.open_subkey_with_flags(&key_name, KEY_READ) else {
                            continue;
                        };
                        if key.get_value::<u32, _>("SystemComponent").unwrap_or(0) == 1 {
                            continue;
                        }
                        let display_name = key.get_value::<String, _>("DisplayName").ok();
                        let install_location = key
                            .get_value::<OsString, _>("InstallLocation")
                            .ok()
                            .filter(|value| !value.is_empty())
                            .map(PathBuf::from);
                        let display_icon = key
                            .get_value::<OsString, _>("DisplayIcon")
                            .ok()
                            .and_then(|value| command_executable(&value));
                        let publisher = key.get_value::<String, _>("Publisher").ok();
                        if display_name.is_none() && install_location.is_none() {
                            continue;
                        }
                        records.push(UninstallRecord {
                            hive,
                            key_name,
                            display_name,
                            install_location,
                            display_icon,
                            publisher,
                        });
                    }
                }
            }
            deduplicate_uninstall(&mut records);
            Ok(records)
        }
    }

    pub struct RegistryRunScanner;

    impl RegistryRunScanner {
        pub fn scan(&self) -> Result<Vec<AutoStartEntry>, String> {
            let mut entries = Vec::new();
            for (root, hive) in [
                (HKCU, RegistryHive::CurrentUser),
                (HKLM, RegistryHive::LocalMachine),
            ] {
                for subkey in [RUN_KEY, RUN_ONCE_KEY] {
                    for (flags, view) in views() {
                        let Ok(key) = root.open_subkey_with_flags(subkey, KEY_READ | flags) else {
                            continue;
                        };
                        for (value_name, value) in key.enum_values().filter_map(Result::ok) {
                            let Ok(command) = OsString::from_reg_value(&value) else {
                                continue;
                            };
                            let command_text = command.to_string_lossy().into_owned();
                            entries.push(AutoStartEntry::RegistryRun {
                                hive,
                                subkey: subkey.to_owned(),
                                value_name,
                                view,
                                command: command_text.clone(),
                                target: ExecutableRef {
                                    raw: command_text,
                                    resolved_path: command_executable(&command),
                                },
                            });
                        }
                    }
                }
            }
            deduplicate_autostart(&mut entries);
            Ok(entries)
        }
    }

    pub struct StartupFolderScanner;

    impl StartupFolderScanner {
        pub fn scan(&self) -> Result<Vec<AutoStartEntry>, String> {
            let folders = [
                (
                    StartupFolderKind::User,
                    std::env::var_os("APPDATA")
                        .map(PathBuf::from)
                        .map(|base| base.join(r"Microsoft\Windows\Start Menu\Programs\Startup")),
                ),
                (
                    StartupFolderKind::Common,
                    std::env::var_os("PROGRAMDATA")
                        .map(PathBuf::from)
                        .map(|base| base.join(r"Microsoft\Windows\Start Menu\Programs\StartUp")),
                ),
            ];
            let mut entries = Vec::new();
            for (kind, folder) in folders {
                let Some(folder) = folder else { continue };
                let Ok(children) = std::fs::read_dir(&folder) else {
                    continue;
                };
                for child in children.filter_map(Result::ok) {
                    let path = child.path();
                    if !path.is_file() {
                        continue;
                    }
                    let target = match path
                        .extension()
                        .and_then(|value| value.to_str())
                        .map(str::to_ascii_lowercase)
                        .as_deref()
                    {
                        Some("lnk") => resolve_shell_link(&path),
                        Some("exe") => Some(path.clone()),
                        _ => None,
                    };
                    let Some(target) = target else { continue };
                    entries.push(AutoStartEntry::StartupFolder {
                        kind,
                        entry_path: path,
                        target: ExecutableRef {
                            raw: target.display().to_string(),
                            resolved_path: Some(target),
                        },
                    });
                }
            }
            Ok(entries)
        }
    }

    pub struct TaskSchedulerScanner;

    impl TaskSchedulerScanner {
        pub fn scan(&self) -> Result<Vec<AutoStartEntry>, String> {
            let _apartment = ComApartment::init()?;
            // SAFETY: COM is initialized for this thread and the CLSID/interface pair is valid.
            let service: ITaskService = unsafe {
                CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
                    .map_err(|error| error.to_string())?
            };
            let empty = VARIANT::default();
            // SAFETY: Empty variants request the local Task Scheduler connection.
            unsafe {
                service
                    .Connect(&empty, &empty, &empty, &empty)
                    .map_err(|error| error.to_string())?;
            }
            // SAFETY: The service is connected and the root path is valid.
            let root = unsafe {
                service
                    .GetFolder(&BSTR::from("\\"))
                    .map_err(|error| error.to_string())?
            };
            let mut entries = Vec::new();
            scan_task_folder(&root, &mut entries)?;
            Ok(entries)
        }
    }

    pub struct ServiceScanner;

    impl ServiceScanner {
        pub fn scan(&self) -> Result<Vec<AutoStartEntry>, String> {
            // SAFETY: Null names select the local machine and active database.
            let manager = unsafe {
                OpenSCManagerW(None, None, SC_MANAGER_ENUMERATE_SERVICE)
                    .map_err(|error| error.to_string())?
            };
            let manager = ServiceHandle(manager);
            let mut needed = 0;
            let mut returned = 0;
            // SAFETY: First call intentionally requests the required buffer size.
            let first = unsafe {
                EnumServicesStatusExW(
                    manager.0,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    SERVICE_STATE_ALL,
                    None,
                    &mut needed,
                    &mut returned,
                    None,
                    None,
                )
            };
            if let Err(error) = first
                && error.code() != ERROR_MORE_DATA.to_hresult()
            {
                return Err(error.to_string());
            }
            let mut buffer = vec![0u8; needed as usize];
            // SAFETY: The buffer size was supplied by the SCM.
            unsafe {
                EnumServicesStatusExW(
                    manager.0,
                    SC_ENUM_PROCESS_INFO,
                    SERVICE_WIN32,
                    SERVICE_STATE_ALL,
                    Some(&mut buffer),
                    &mut needed,
                    &mut returned,
                    None,
                    None,
                )
                .map_err(|error| error.to_string())?;
            }
            // SAFETY: SCM filled `returned` contiguous records in the supplied buffer.
            let records = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>(),
                    returned as usize,
                )
            };
            let mut entries = Vec::new();
            for record in records {
                let service_name = wide_ptr_to_os_string(record.lpServiceName.0)
                    .to_string_lossy()
                    .into_owned();
                let display_name = wide_ptr_to_os_string(record.lpDisplayName.0)
                    .to_string_lossy()
                    .into_owned();
                // SAFETY: The manager handle is valid and service name came from SCM.
                let Ok(service) = (unsafe {
                    OpenServiceW(
                        manager.0,
                        PCWSTR(record.lpServiceName.0),
                        SERVICE_QUERY_CONFIG,
                    )
                }) else {
                    continue;
                };
                let service = ServiceHandle(service);
                let Some((start_type, raw)) = query_service_config(service.0) else {
                    continue;
                };
                let expanded = expand_environment(&raw);
                let resolved_path = command_executable(&expanded);
                let protected = resolved_path.as_deref().is_some_and(protected_system_path);
                entries.push(AutoStartEntry::Service {
                    service_name,
                    display_name: Some(display_name),
                    start_type,
                    running: record.ServiceStatusProcess.dwCurrentState == SERVICE_RUNNING,
                    protected,
                    target: ExecutableRef {
                        raw: raw.to_string_lossy().into_owned(),
                        resolved_path,
                    },
                });
            }
            Ok(entries)
        }
    }

    struct ComApartment;

    impl ComApartment {
        fn init() -> Result<Self, String> {
            // SAFETY: Balancing CoUninitialize is handled by Drop on this thread.
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(|error| error.to_string())?;
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: This balances the successful initialization above.
            unsafe { CoUninitialize() };
        }
    }

    struct ServiceHandle(SC_HANDLE);

    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            // SAFETY: The handle was returned by SCM and is closed once.
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    fn resolve_shell_link(path: &Path) -> Option<PathBuf> {
        let _apartment = ComApartment::init().ok()?;
        // SAFETY: COM is initialized and ShellLink implements IShellLinkW.
        let shell_link: IShellLinkW =
            unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()? };
        let persist: IPersistFile = shell_link.cast().ok()?;
        let wide = to_wide(path.as_os_str());
        // SAFETY: The path is NUL-terminated and points to a shortcut file.
        unsafe { persist.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()? };
        let mut target = vec![0u16; 32_768];
        let mut find_data = WIN32_FIND_DATAW::default();
        // SAFETY: Buffers are valid and sized for the API.
        unsafe {
            shell_link
                .GetPath(&mut target, &mut find_data, SLGP_RAWPATH.0 as u32)
                .ok()?;
        }
        let length = target.iter().position(|value| *value == 0)?;
        Some(PathBuf::from(OsString::from_wide(&target[..length])))
    }

    fn scan_task_folder(
        folder: &ITaskFolder,
        entries: &mut Vec<AutoStartEntry>,
    ) -> Result<(), String> {
        // SAFETY: Folder is a valid Task Scheduler COM object.
        let tasks = unsafe {
            folder
                .GetTasks(TASK_ENUM_HIDDEN.0)
                .map_err(|error| error.to_string())?
        };
        // SAFETY: Collection remains alive for iteration.
        let count = unsafe { tasks.Count().map_err(|error| error.to_string())? };
        for index in 1..=count {
            let variant = VARIANT::from(index);
            // SAFETY: Task Scheduler collections use one-based indices.
            let task = unsafe {
                tasks
                    .get_Item(&variant)
                    .map_err(|error| error.to_string())?
            };
            append_task_actions(&task, entries)?;
        }
        // SAFETY: Folder collection remains alive for recursive traversal.
        let folders = unsafe { folder.GetFolders(0).map_err(|error| error.to_string())? };
        // SAFETY: Collection remains alive for iteration.
        let count = unsafe { folders.Count().map_err(|error| error.to_string())? };
        for index in 1..=count {
            let variant = VARIANT::from(index);
            // SAFETY: Task Scheduler collections use one-based indices.
            let child = unsafe {
                folders
                    .get_Item(&variant)
                    .map_err(|error| error.to_string())?
            };
            scan_task_folder(&child, entries)?;
        }
        Ok(())
    }

    fn append_task_actions(
        task: &IRegisteredTask,
        entries: &mut Vec<AutoStartEntry>,
    ) -> Result<(), String> {
        // SAFETY: Registered task remains alive while its definition is inspected.
        let definition = unsafe { task.Definition().map_err(|error| error.to_string())? };
        // SAFETY: Definition owns a valid action collection.
        let actions: IActionCollection =
            unsafe { definition.Actions().map_err(|error| error.to_string())? };
        let mut count = 0;
        // SAFETY: Output pointer is valid.
        unsafe {
            actions
                .Count(&mut count)
                .map_err(|error| error.to_string())?;
        }
        for index in 1..=count {
            // SAFETY: Action collections use one-based indices.
            let action = unsafe { actions.get_Item(index).map_err(|error| error.to_string())? };
            let Ok(exec) = action.cast::<IExecAction>() else {
                continue;
            };
            let mut path = BSTR::new();
            // SAFETY: Output BSTR pointer is valid.
            unsafe { exec.Path(&mut path).map_err(|error| error.to_string())? };
            let raw = path.to_string();
            let target = Some(PathBuf::from(expand_environment(
                OsString::from(&raw).as_os_str(),
            )));
            // SAFETY: These property getters return owned values.
            let full_name = unsafe { task.Path().map_err(|error| error.to_string())? }.to_string();
            let enabled =
                unsafe { task.Enabled().map_err(|error| error.to_string())? } != VARIANT_BOOL(0);
            entries.push(AutoStartEntry::ScheduledTask {
                full_name,
                enabled,
                target: ExecutableRef {
                    raw,
                    resolved_path: target,
                },
            });
        }
        Ok(())
    }

    fn query_service_config(handle: SC_HANDLE) -> Option<(u32, OsString)> {
        let mut needed = 0;
        // SAFETY: First call requests the required size.
        let _ = unsafe { QueryServiceConfigW(handle, None, 0, &mut needed) };
        if needed == 0 {
            return None;
        }
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: Buffer has the size requested by SCM. Pointer-backed values are
        // converted to owned values before the buffer is dropped.
        unsafe {
            QueryServiceConfigW(
                handle,
                Some(buffer.as_mut_ptr().cast()),
                needed,
                &mut needed,
            )
            .ok()?;
            let config = &*buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>();
            Some((
                config.dwStartType.0,
                wide_ptr_to_os_string(config.lpBinaryPathName.0),
            ))
        }
    }

    fn views() -> [(u32, Wow64View); 2] {
        [
            (KEY_WOW64_64KEY, Wow64View::Wow64_64),
            (KEY_WOW64_32KEY, Wow64View::Wow64_32),
        ]
    }

    fn deduplicate_uninstall(records: &mut Vec<UninstallRecord>) {
        records.sort_by(|left, right| {
            (
                left.key_name.as_str(),
                left.install_location.as_ref(),
                left.display_name.as_ref(),
            )
                .cmp(&(
                    right.key_name.as_str(),
                    right.install_location.as_ref(),
                    right.display_name.as_ref(),
                ))
        });
        records.dedup_by(|left, right| {
            left.key_name == right.key_name
                && left.install_location == right.install_location
                && left.display_name == right.display_name
        });
    }

    fn deduplicate_autostart(entries: &mut Vec<AutoStartEntry>) {
        let mut seen = std::collections::HashSet::new();
        entries.retain(|entry| seen.insert(entry.display_name()));
    }
}
