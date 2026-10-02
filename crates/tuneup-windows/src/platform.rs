use tuneup_core::{
    error::PlatformError,
    ipc::DisablePlatformRequest,
    model::{PlatformBackup, ProcessGroup},
};
pub use tuneup_platform::PlatformChangeReport;

use crate::ipc::ElevatedClient;

pub struct WindowsPlatformFacade {
    helper: Option<ElevatedClient>,
}

impl tuneup_platform::PlatformMutator for WindowsPlatformFacade {
    fn disable_for_group(&mut self, group: &ProcessGroup) -> PlatformChangeReport {
        WindowsPlatformFacade::disable_for_group(self, group)
    }

    fn restore(&mut self, backup: PlatformBackup) -> PlatformChangeReport {
        WindowsPlatformFacade::restore(self, backup)
    }
}

impl WindowsPlatformFacade {
    pub fn new(helper: Option<ElevatedClient>) -> Self {
        Self { helper }
    }

    pub fn disable_for_group(&mut self, group: &ProcessGroup) -> PlatformChangeReport {
        let mut report = imp::disable_for_group(group, self.helper.as_mut());
        if !report.errors.is_empty() && !report.backup.is_empty() {
            tracing::warn!(
                group_id = %group.id,
                errors = report.errors.len(),
                "platform deactivation failed; rolling back completed changes"
            );
            let rollback = imp::restore(report.backup, self.helper.as_mut());
            report.backup = rollback.backup;
            report.errors.extend(rollback.errors);
        }
        report
    }

    pub fn restore(&mut self, backup: PlatformBackup) -> PlatformChangeReport {
        imp::restore(backup, self.helper.as_mut())
    }
}

pub struct ElevatedPlatformExecutor;

impl ElevatedPlatformExecutor {
    pub fn apply(&self, request: &DisablePlatformRequest) -> PlatformChangeReport {
        imp::apply_elevated(request)
    }

    pub fn restore(&self, backup: PlatformBackup) -> Result<PlatformBackup, PlatformError> {
        imp::restore_elevated(backup)
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    pub fn disable_for_group(
        _group: &ProcessGroup,
        _helper: Option<&mut ElevatedClient>,
    ) -> PlatformChangeReport {
        PlatformChangeReport {
            backup: PlatformBackup::default(),
            errors: vec![PlatformError::HelperUnavailable.into()],
        }
    }

    pub fn restore(
        backup: PlatformBackup,
        _helper: Option<&mut ElevatedClient>,
    ) -> PlatformChangeReport {
        PlatformChangeReport {
            backup,
            errors: Vec::new(),
        }
    }

    pub fn apply_elevated(_request: &DisablePlatformRequest) -> PlatformChangeReport {
        PlatformChangeReport {
            backup: PlatformBackup::default(),
            errors: vec![PlatformError::HelperUnavailable.into()],
        }
    }

    pub fn restore_elevated(backup: PlatformBackup) -> Result<PlatformBackup, PlatformError> {
        Ok(backup)
    }
}

#[cfg(windows)]
mod imp {
    use std::{
        borrow::Cow,
        ffi::OsStr,
        fs,
        path::{Path, PathBuf},
    };

    use tuneup_core::{
        error::{IpcError, PlatformError, TuneupError},
        ipc::{DisablePlatformRequest, HelperCommand, HelperResponse},
        model::{
            AutoStartEntry, PlatformBackup, ProcessGroup, RegistryBackup, RegistryHive,
            ServiceBackup, StartupFolderKind, StartupMoveBackup, TaskBackup, Wow64View,
        },
    };
    use windows::{
        Win32::System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            Services::{
                ChangeServiceConfigW, CloseServiceHandle, ENUM_SERVICE_TYPE, OpenSCManagerW,
                OpenServiceW, SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_CHANGE_CONFIG,
                SERVICE_DISABLED, SERVICE_ERROR, SERVICE_NO_CHANGE, SERVICE_START_TYPE,
            },
            TaskScheduler::{ITaskService, TaskScheduler},
            Variant::VARIANT,
        },
        core::{BSTR, PCWSTR},
    };
    use winreg::{
        HKCU, HKLM, RegKey, RegValue,
        enums::{
            KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, KEY_WRITE, REG_EXPAND_SZ, REG_SZ, RegType,
        },
    };

    use super::*;
    use crate::{
        inventory::InventoryAggregator,
        ipc::ElevatedClient,
        path_util::{exact_group_match, protected_system_path},
    };

    pub fn disable_for_group(
        group: &ProcessGroup,
        helper: Option<&mut ElevatedClient>,
    ) -> PlatformChangeReport {
        let Some(root) = group.install_root.as_deref() else {
            return PlatformChangeReport {
                backup: PlatformBackup::default(),
                errors: vec![TuneupError::Platform(PlatformError::ProtectedObject(
                    "у группы нет точного install root".to_owned(),
                ))],
            };
        };
        let mut backup = PlatformBackup::default();
        let mut errors = Vec::new();
        for entry in &group.autostart_entries {
            match entry {
                AutoStartEntry::RegistryRun {
                    hive: RegistryHive::CurrentUser,
                    ..
                } => match disable_registry_entry(entry, root) {
                    Ok(value) => backup.registry.push(value),
                    Err(error) => errors.push(error.into()),
                },
                AutoStartEntry::StartupFolder {
                    kind: StartupFolderKind::User,
                    ..
                } => match disable_startup_entry(entry, root) {
                    Ok(value) => backup.startup_moves.push(value),
                    Err(error) => errors.push(error.into()),
                },
                _ => {}
            }
        }
        if needs_elevation(group) {
            match helper {
                Some(client) => {
                    let request = DisablePlatformRequest {
                        install_root: root.to_path_buf(),
                        include_hklm_run: true,
                        include_services: true,
                        include_tasks: true,
                        include_common_startup: true,
                        ..DisablePlatformRequest::default()
                    };
                    match client.request(HelperCommand::ApplyPlatform(request)) {
                        Ok(HelperResponse::PlatformApplied {
                            backup: elevated,
                            errors: elevated_errors,
                        }) => {
                            merge_backup(&mut backup, elevated);
                            errors.extend(
                                elevated_errors.into_iter().map(|error| {
                                    TuneupError::Platform(PlatformError::Partial(error))
                                }),
                            );
                        }
                        Ok(_) => errors
                            .push(IpcError::Remote("неожиданный ответ helper".to_owned()).into()),
                        Err(error) => errors.push(error.into()),
                    }
                }
                None => errors.push(PlatformError::HelperUnavailable.into()),
            }
        }
        PlatformChangeReport { backup, errors }
    }

    pub fn restore(
        backup: PlatformBackup,
        helper: Option<&mut ElevatedClient>,
    ) -> PlatformChangeReport {
        let (local, elevated) = split_backup(backup);
        let mut remaining = PlatformBackup::default();
        let mut errors = Vec::new();
        for value in local.registry {
            if let Err(error) = restore_registry_entry(&value) {
                errors.push(error.into());
                remaining.registry.push(value);
            }
        }
        for value in local.startup_moves {
            if let Err(error) = restore_startup_entry(&value) {
                errors.push(error.into());
                remaining.startup_moves.push(value);
            }
        }
        if !elevated.is_empty() {
            match helper {
                Some(client) => match client
                    .request(HelperCommand::RestorePlatform(elevated.clone()))
                {
                    Ok(HelperResponse::PlatformRestored {
                        remaining: elevated_remaining,
                    }) => {
                        merge_backup(&mut remaining, elevated_remaining);
                    }
                    Ok(_) => {
                        errors.push(IpcError::Remote("неожиданный ответ helper".to_owned()).into());
                        merge_backup(&mut remaining, elevated);
                    }
                    Err(error) => {
                        errors.push(error.into());
                        merge_backup(&mut remaining, elevated);
                    }
                },
                None => {
                    errors.push(PlatformError::HelperUnavailable.into());
                    merge_backup(&mut remaining, elevated);
                }
            }
        }
        PlatformChangeReport {
            backup: remaining,
            errors,
        }
    }

    pub fn apply_elevated(request: &DisablePlatformRequest) -> PlatformChangeReport {
        let mut backup = PlatformBackup::default();
        let mut errors = Vec::new();
        if let Err(error) = validate_install_root(&request.install_root) {
            return PlatformChangeReport {
                backup,
                errors: vec![error.into()],
            };
        }
        let inventory = match InventoryAggregator.snapshot() {
            Ok(inventory) => inventory,
            Err(error) => {
                return PlatformChangeReport {
                    backup,
                    errors: vec![PlatformError::Partial(error.to_string()).into()],
                };
            }
        };
        for entry in &inventory.autostart {
            let matches = entry
                .target_path()
                .is_some_and(|path| exact_group_match(path, &request.install_root));
            if !matches || entry.is_protected() {
                continue;
            }
            match entry {
                AutoStartEntry::RegistryRun {
                    hive: RegistryHive::LocalMachine,
                    ..
                } if request.include_hklm_run => {
                    match disable_registry_entry(entry, &request.install_root) {
                        Ok(value) => backup.registry.push(value),
                        Err(error) => errors.push(error.into()),
                    }
                }
                AutoStartEntry::StartupFolder {
                    kind: StartupFolderKind::Common,
                    ..
                } if request.include_common_startup => {
                    match disable_startup_entry(entry, &request.install_root) {
                        Ok(value) => backup.startup_moves.push(value),
                        Err(error) => errors.push(error.into()),
                    }
                }
                AutoStartEntry::ScheduledTask {
                    full_name,
                    enabled: true,
                    ..
                } if request.include_tasks => match set_task_enabled(full_name, false) {
                    Ok(()) => backup.tasks.push(TaskBackup {
                        full_name: full_name.clone(),
                        was_enabled: true,
                    }),
                    Err(error) => errors.push(error.into()),
                },
                AutoStartEntry::Service {
                    service_name,
                    start_type,
                    protected: false,
                    ..
                } if request.include_services && *start_type != SERVICE_DISABLED.0 => {
                    match set_service_start(service_name, SERVICE_DISABLED.0) {
                        Ok(()) => backup.services.push(ServiceBackup {
                            name: service_name.clone(),
                            original_start: *start_type,
                        }),
                        Err(error) => errors.push(error.into()),
                    }
                }
                _ => {}
            }
        }
        PlatformChangeReport { backup, errors }
    }

    pub fn restore_elevated(backup: PlatformBackup) -> Result<PlatformBackup, PlatformError> {
        let mut remaining = PlatformBackup::default();
        for value in backup.registry {
            if restore_registry_entry(&value).is_err() {
                remaining.registry.push(value);
            }
        }
        for value in backup.startup_moves {
            if restore_startup_entry(&value).is_err() {
                remaining.startup_moves.push(value);
            }
        }
        for value in backup.tasks {
            if set_task_enabled(&value.full_name, value.was_enabled).is_err() {
                remaining.tasks.push(value);
            }
        }
        for value in backup.services {
            if set_service_start(&value.name, value.original_start).is_err() {
                remaining.services.push(value);
            }
        }
        Ok(remaining)
    }

    fn disable_registry_entry(
        entry: &AutoStartEntry,
        install_root: &Path,
    ) -> Result<RegistryBackup, PlatformError> {
        let AutoStartEntry::RegistryRun {
            hive,
            subkey,
            value_name,
            view,
            target,
            ..
        } = entry
        else {
            return Err(PlatformError::Partial("неверный тип Run".to_owned()));
        };
        let path = target
            .resolved_path
            .as_deref()
            .ok_or_else(|| PlatformError::ProtectedObject(value_name.clone()))?;
        if !exact_group_match(path, install_root) {
            return Err(PlatformError::ProtectedObject(value_name.clone()));
        }
        let key = registry_root(*hive)
            .open_subkey_with_flags(subkey, KEY_READ | KEY_WRITE | view_flags(*view))
            .map_err(|error| PlatformError::AccessDenied(error.to_string()))?;
        let value = key
            .get_raw_value(value_name)
            .map_err(|error| PlatformError::Mutation {
                path: PathBuf::from(subkey),
                detail: error.to_string(),
            })?;
        tracing::info!(subkey, value_name, "disabling registry autostart entry");
        key.delete_value(value_name)
            .map_err(|error| PlatformError::Mutation {
                path: PathBuf::from(subkey),
                detail: error.to_string(),
            })?;
        Ok(RegistryBackup {
            hive: *hive,
            subkey: subkey.clone(),
            view: *view,
            name: value_name.clone(),
            value_type: value.vtype as u32,
            value_bytes: value.bytes.into_owned(),
        })
    }

    fn restore_registry_entry(backup: &RegistryBackup) -> Result<(), PlatformError> {
        let key = registry_root(backup.hive)
            .open_subkey_with_flags(&backup.subkey, KEY_WRITE | view_flags(backup.view))
            .map_err(|error| PlatformError::AccessDenied(error.to_string()))?;
        let value_type = saved_reg_type(backup.value_type)
            .ok_or_else(|| PlatformError::Partial("неизвестный тип реестра".to_owned()))?;
        key.set_raw_value(
            &backup.name,
            &RegValue {
                bytes: Cow::Borrowed(&backup.value_bytes),
                vtype: value_type,
            },
        )
        .map_err(|error| PlatformError::Mutation {
            path: PathBuf::from(&backup.subkey),
            detail: error.to_string(),
        })
    }

    fn disable_startup_entry(
        entry: &AutoStartEntry,
        install_root: &Path,
    ) -> Result<StartupMoveBackup, PlatformError> {
        let AutoStartEntry::StartupFolder {
            kind,
            entry_path,
            target,
        } = entry
        else {
            return Err(PlatformError::Partial("неверный Startup entry".to_owned()));
        };
        let target_path = target
            .resolved_path
            .as_deref()
            .ok_or_else(|| PlatformError::ProtectedObject(entry_path.display().to_string()))?;
        if !exact_group_match(target_path, install_root) {
            return Err(PlatformError::ProtectedObject(
                entry_path.display().to_string(),
            ));
        }
        let parent = entry_path.parent().ok_or_else(|| PlatformError::Mutation {
            path: entry_path.clone(),
            detail: "нет родительского каталога".to_owned(),
        })?;
        let disabled_dir = parent.join("TuneUpDisabled");
        fs::create_dir_all(&disabled_dir).map_err(|error| PlatformError::Mutation {
            path: disabled_dir.clone(),
            detail: error.to_string(),
        })?;
        let file_name = entry_path
            .file_name()
            .ok_or_else(|| PlatformError::Mutation {
                path: entry_path.clone(),
                detail: "нет имени файла".to_owned(),
            })?;
        let disabled_path = unique_destination(&disabled_dir, file_name);
        tracing::info!(
            source = %entry_path.display(),
            destination = %disabled_path.display(),
            "moving Startup entry"
        );
        fs::rename(entry_path, &disabled_path).map_err(|error| PlatformError::Mutation {
            path: entry_path.clone(),
            detail: error.to_string(),
        })?;
        Ok(StartupMoveBackup {
            kind: *kind,
            original_path: entry_path.clone(),
            disabled_path,
        })
    }

    fn restore_startup_entry(backup: &StartupMoveBackup) -> Result<(), PlatformError> {
        if !backup.disabled_path.exists() && backup.original_path.exists() {
            return Ok(());
        }
        if backup.original_path.exists() {
            return Err(PlatformError::Mutation {
                path: backup.original_path.clone(),
                detail: "целевой файл уже существует".to_owned(),
            });
        }
        fs::rename(&backup.disabled_path, &backup.original_path).map_err(|error| {
            PlatformError::Mutation {
                path: backup.disabled_path.clone(),
                detail: error.to_string(),
            }
        })
    }

    fn set_task_enabled(full_name: &str, enabled: bool) -> Result<(), PlatformError> {
        tracing::info!(full_name, enabled, "changing scheduled task state");
        let _apartment = ComApartment::init()?;
        // SAFETY: COM is initialized and CLSID/interface pair is valid.
        let service: ITaskService = unsafe {
            CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| PlatformError::Partial(error.to_string()))?
        };
        let empty = VARIANT::default();
        // SAFETY: Empty variants connect to local Task Scheduler.
        unsafe {
            service
                .Connect(&empty, &empty, &empty, &empty)
                .map_err(|error| PlatformError::Partial(error.to_string()))?;
        }
        let split = full_name.rfind('\\').unwrap_or(0);
        let (folder_path, task_name) = if split == 0 {
            ("\\", full_name.trim_start_matches('\\'))
        } else {
            (&full_name[..split], &full_name[split + 1..])
        };
        // SAFETY: Connected service and scheduler paths are valid BSTR values.
        let folder = unsafe {
            service
                .GetFolder(&BSTR::from(folder_path))
                .map_err(|error| PlatformError::Partial(error.to_string()))?
        };
        // SAFETY: Folder is valid and task name is relative to it.
        let task = unsafe {
            folder
                .GetTask(&BSTR::from(task_name))
                .map_err(|error| PlatformError::Partial(error.to_string()))?
        };
        // SAFETY: VARIANT_BOOL uses -1 for true and 0 for false.
        unsafe {
            task.SetEnabled(windows::Win32::Foundation::VARIANT_BOOL(if enabled {
                -1
            } else {
                0
            }))
            .map_err(|error| PlatformError::Partial(error.to_string()))
        }
    }

    fn set_service_start(name: &str, start_type: u32) -> Result<(), PlatformError> {
        tracing::info!(name, start_type, "changing service start type");
        // SAFETY: Null names select local SCM.
        let manager = unsafe {
            OpenSCManagerW(None, None, SC_MANAGER_CONNECT)
                .map_err(|error| PlatformError::AccessDenied(error.to_string()))?
        };
        let manager = ServiceHandle(manager);
        // SAFETY: Manager is valid and name comes from SCM inventory/backup.
        let service = unsafe {
            OpenServiceW(manager.0, &BSTR::from(name), SERVICE_CHANGE_CONFIG)
                .map_err(|error| PlatformError::AccessDenied(error.to_string()))?
        };
        let service = ServiceHandle(service);
        // SAFETY: SERVICE_NO_CHANGE preserves unrelated fields; null optional strings are valid.
        unsafe {
            ChangeServiceConfigW(
                service.0,
                ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
                SERVICE_START_TYPE(start_type),
                SERVICE_ERROR(SERVICE_NO_CHANGE),
                PCWSTR::null(),
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
                PCWSTR::null(),
            )
            .map_err(|error| PlatformError::Mutation {
                path: PathBuf::from(name),
                detail: error.to_string(),
            })
        }
    }

    struct ServiceHandle(SC_HANDLE);

    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            // SAFETY: SCM handle is owned and closed once.
            unsafe {
                let _ = CloseServiceHandle(self.0);
            }
        }
    }

    struct ComApartment;

    impl ComApartment {
        fn init() -> Result<Self, PlatformError> {
            // SAFETY: Balanced by Drop on this thread.
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
                .ok()
                .map_err(|error| PlatformError::Partial(error.to_string()))?;
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            // SAFETY: Balances successful initialization.
            unsafe { CoUninitialize() };
        }
    }

    fn needs_elevation(group: &ProcessGroup) -> bool {
        group.autostart_entries.iter().any(|entry| {
            matches!(
                entry,
                AutoStartEntry::RegistryRun {
                    hive: RegistryHive::LocalMachine,
                    ..
                } | AutoStartEntry::StartupFolder {
                    kind: StartupFolderKind::Common,
                    ..
                } | AutoStartEntry::ScheduledTask { .. }
                    | AutoStartEntry::Service { .. }
            )
        })
    }

    fn split_backup(backup: PlatformBackup) -> (PlatformBackup, PlatformBackup) {
        let mut local = PlatformBackup::default();
        let mut elevated = PlatformBackup::default();
        for value in backup.registry {
            match value.hive {
                RegistryHive::CurrentUser => local.registry.push(value),
                RegistryHive::LocalMachine => elevated.registry.push(value),
            }
        }
        for value in backup.startup_moves {
            match value.kind {
                StartupFolderKind::User => local.startup_moves.push(value),
                StartupFolderKind::Common => elevated.startup_moves.push(value),
            }
        }
        elevated.tasks = backup.tasks;
        elevated.services = backup.services;
        (local, elevated)
    }

    fn merge_backup(target: &mut PlatformBackup, source: PlatformBackup) {
        target.registry.extend(source.registry);
        target.startup_moves.extend(source.startup_moves);
        target.tasks.extend(source.tasks);
        target.services.extend(source.services);
    }

    fn validate_install_root(root: &Path) -> Result<(), PlatformError> {
        if !root.is_absolute() || protected_system_path(root) {
            return Err(PlatformError::ProtectedObject(root.display().to_string()));
        }
        Ok(())
    }

    fn unique_destination(folder: &Path, name: &OsStr) -> PathBuf {
        let initial = folder.join(name);
        if !initial.exists() {
            return initial;
        }
        for index in 1..10_000 {
            let candidate = folder.join(format!("{}.{}", name.to_string_lossy(), index));
            if !candidate.exists() {
                return candidate;
            }
        }
        folder.join(format!("{}.{}", name.to_string_lossy(), std::process::id()))
    }

    fn registry_root(hive: RegistryHive) -> &'static RegKey {
        match hive {
            RegistryHive::CurrentUser => HKCU,
            RegistryHive::LocalMachine => HKLM,
        }
    }

    const fn view_flags(view: Wow64View) -> u32 {
        match view {
            Wow64View::Default => 0,
            Wow64View::Wow64_32 => KEY_WOW64_32KEY,
            Wow64View::Wow64_64 => KEY_WOW64_64KEY,
        }
    }

    const fn saved_reg_type(value: u32) -> Option<RegType> {
        match value {
            1 => Some(REG_SZ),
            2 => Some(REG_EXPAND_SZ),
            _ => None,
        }
    }
}
