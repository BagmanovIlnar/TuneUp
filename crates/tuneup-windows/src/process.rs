use tuneup_core::{
    error::{ProcessError, TuneupError},
    model::{ProcessIdentity, ProcessInfo},
};

pub struct ProcessController;

impl ProcessController {
    /// Validates every process, then force-terminates them.
    ///
    /// # Example
    ///
    /// ```ignore
    /// ProcessController::terminate(&group.processes)?;
    /// ```
    pub fn terminate(
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        let validation = processes
            .iter()
            .filter_map(validate_process)
            .map(TuneupError::from)
            .collect::<Vec<_>>();
        if !validation.is_empty() {
            return Err((Vec::new(), validation));
        }
        platform::terminate(processes)
    }

    /// Force-terminates previously recorded identities (recovery / migration).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (remaining, errors) = ProcessController::terminate_identities(stale);
    /// ```
    pub fn terminate_identities(
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        platform::terminate_identities(identities)
    }
}

impl tuneup_platform::ProcessControl for ProcessController {
    fn terminate(
        &self,
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        ProcessController::terminate(processes)
    }

    fn terminate_identities(
        &self,
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        ProcessController::terminate_identities(identities)
    }
}

fn validate_process(process: &ProcessInfo) -> Option<ProcessError> {
    if process.pid == 0 || process.pid == 4 || process.pid == std::process::id() {
        return Some(ProcessError::Protected {
            pid: process.pid,
            reason: "системный или собственный процесс".to_owned(),
        });
    }
    const PROTECTED: &[&str] = &[
        "system",
        "registry",
        "smss.exe",
        "csrss.exe",
        "wininit.exe",
        "services.exe",
        "lsass.exe",
        "winlogon.exe",
        "fontdrvhost.exe",
        "dwm.exe",
        "explorer.exe",
        "securityhealthservice.exe",
        "msmpeng.exe",
    ];
    PROTECTED
        .contains(&process.name.to_ascii_lowercase().as_str())
        .then(|| ProcessError::Protected {
            pid: process.pid,
            reason: format!("защищённое имя {}", process.name),
        })
}

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub fn terminate(
        _processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        Err((
            Vec::new(),
            vec![TuneupError::Process(ProcessError::OpenFailed {
                pid: 0,
                detail: "завершение поддерживается только в Windows".to_owned(),
            })],
        ))
    }

    pub fn terminate_identities(
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        (
            identities,
            vec![TuneupError::Process(ProcessError::OpenFailed {
                pid: 0,
                detail: "завершение поддерживается только в Windows".to_owned(),
            })],
        )
    }
}

#[cfg(windows)]
mod platform {
    use std::io;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        System::Threading::{
            GetProcessTimes, IsProcessCritical, OpenProcess, TerminateProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
        },
    };

    use super::*;

    const WINDOWS_TO_UNIX_EPOCH_100NS: u64 = 116_444_736_000_000_000;
    const TICKS_PER_SECOND: u64 = 10_000_000;

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: Handle is non-null, owned, and closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub fn terminate(
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        for process in processes {
            let identity = ProcessIdentity {
                pid: process.pid,
                start_time: process.start_time,
            };
            match kill(&identity) {
                Ok(()) => {}
                Err(error) => {
                    remaining.push(identity);
                    errors.push(TuneupError::Process(error));
                }
            }
        }
        if remaining.is_empty() {
            Ok(())
        } else {
            Err((remaining, errors))
        }
    }

    pub fn terminate_identities(
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        for identity in identities {
            match kill(&identity) {
                Ok(()) => {}
                Err(error) => {
                    remaining.push(identity);
                    errors.push(TuneupError::Process(error));
                }
            }
        }
        (remaining, errors)
    }

    fn kill(identity: &ProcessIdentity) -> Result<(), ProcessError> {
        // SAFETY: Flags are valid and PID comes from a process snapshot.
        let raw = unsafe {
            OpenProcess(
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                identity.pid,
            )
        };
        if raw.is_null() {
            let error = io::Error::last_os_error();
            // ERROR_INVALID_PARAMETER (87) — process already gone.
            if error.raw_os_error() == Some(87) {
                return Ok(());
            }
            return Err(ProcessError::OpenFailed {
                pid: identity.pid,
                detail: error.to_string(),
            });
        }
        let handle = OwnedHandle(raw);
        verify_identity(&handle, identity)?;
        verify_not_critical(&handle, identity.pid)?;
        // SAFETY: Handle is identity-checked, non-critical and grants terminate access.
        let success = unsafe { TerminateProcess(handle.0, 1) };
        if success == 0 {
            return Err(ProcessError::SignalFailed {
                pid: identity.pid,
                detail: io::Error::last_os_error().to_string(),
            });
        }
        Ok(())
    }

    fn verify_identity(
        handle: &OwnedHandle,
        expected: &ProcessIdentity,
    ) -> Result<(), ProcessError> {
        let mut creation = empty_filetime();
        let mut exit = empty_filetime();
        let mut kernel = empty_filetime();
        let mut user = empty_filetime();
        // SAFETY: All output pointers are valid and handle grants query access.
        let success =
            unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) };
        if success == 0 {
            return Err(ProcessError::OpenFailed {
                pid: expected.pid,
                detail: io::Error::last_os_error().to_string(),
            });
        }
        if filetime_to_unix_seconds(creation) != expected.start_time {
            return Err(ProcessError::IdentityMismatch { pid: expected.pid });
        }
        Ok(())
    }

    fn verify_not_critical(handle: &OwnedHandle, pid: u32) -> Result<(), ProcessError> {
        let mut critical = 0;
        // SAFETY: Output pointer is valid and handle grants query access.
        if unsafe { IsProcessCritical(handle.0, &mut critical) } == 0 {
            return Err(ProcessError::OpenFailed {
                pid,
                detail: io::Error::last_os_error().to_string(),
            });
        }
        if critical != 0 {
            return Err(ProcessError::Critical { pid });
        }
        Ok(())
    }

    const fn empty_filetime() -> FILETIME {
        FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }
    }

    fn filetime_to_unix_seconds(value: FILETIME) -> u64 {
        let ticks = (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime);
        ticks.saturating_sub(WINDOWS_TO_UNIX_EPOCH_100NS) / TICKS_PER_SECOND
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_critical_names() {
        let process = ProcessInfo {
            pid: 100,
            name: "csrss.exe".to_owned(),
            path: None,
            parent_pid: None,
            start_time: 1,
            cpu_usage: 0.0,
            memory_bytes: 0,
        };
        assert!(validate_process(&process).is_some());
    }
}
