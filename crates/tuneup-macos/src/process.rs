use tuneup_core::{
    error::TuneupError,
    model::{ProcessIdentity, ProcessInfo},
};

/// Force-terminates macOS processes using identity-checked `SIGKILL`.
pub struct MacOsProcessControl;

impl MacOsProcessControl {
    /// Validates every process, then force-kills them. Already-exited PIDs succeed.
    ///
    /// On partial failure returns still-alive identities with errors.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let control = MacOsProcessControl;
    /// control.terminate(&group.processes)?;
    /// ```
    pub fn terminate(
        &self,
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        imp::terminate(processes)
    }

    /// Force-kills previously recorded identities (recovery / migration).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (remaining, errors) = MacOsProcessControl.terminate_identities(stale);
    /// ```
    pub fn terminate_identities(
        &self,
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        imp::terminate_identities(identities)
    }
}

impl tuneup_platform::ProcessControl for MacOsProcessControl {
    fn terminate(
        &self,
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        MacOsProcessControl::terminate(self, processes)
    }

    fn terminate_identities(
        &self,
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        MacOsProcessControl::terminate_identities(self, identities)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use tuneup_core::error::PlatformError;

    use super::*;

    pub fn terminate(
        _processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        Err((
            Vec::new(),
            vec![PlatformError::Unsupported("SIGKILL доступен только в macOS".to_owned()).into()],
        ))
    }

    pub fn terminate_identities(
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        (
            identities,
            vec![PlatformError::Unsupported("SIGKILL доступен только в macOS".to_owned()).into()],
        )
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{io, path::Path};

    use sysinfo::{Pid, System};
    use tuneup_core::error::ProcessError;

    use super::*;

    const DENIED_NAMES: &[&str] = &[
        "launchd",
        "kernel_task",
        "windowserver",
        "loginwindow",
        "finder",
        "dock",
        "systemuiserver",
        "securityd",
        "trustd",
        "runningboardd",
        "syspolicyd",
        "opendirectoryd",
        "notifyd",
    ];

    pub fn terminate(
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        let mut validation = Vec::new();
        for process in processes {
            if let Err(error) = validate_requested(process) {
                validation.push(error.into());
            }
        }
        if !validation.is_empty() {
            return Err((Vec::new(), validation));
        }

        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        for process in processes {
            let identity = ProcessIdentity {
                pid: process.pid,
                start_time: process.start_time,
            };
            match signal_kill(&identity) {
                Ok(()) => {}
                Err(ProcessError::OpenFailed { detail, .. }) if detail == "процесс не найден" => {}
                Err(error) => {
                    errors.push(error.into());
                    remaining.push(identity);
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
            match signal_kill(&identity) {
                Ok(()) => {}
                Err(ProcessError::OpenFailed { detail, .. }) if detail == "процесс не найден" => {}
                Err(error) => {
                    errors.push(error.into());
                    remaining.push(identity);
                }
            }
        }
        (remaining, errors)
    }

    fn validate_requested(process: &ProcessInfo) -> Result<(), ProcessError> {
        if process.pid == 0 || process.pid == 1 || process.pid == std::process::id() {
            return Err(protected(process.pid, "системный или собственный PID"));
        }
        if denied_name(&process.name) {
            return Err(protected(
                process.pid,
                &format!("защищённое имя {}", process.name),
            ));
        }
        if process.path.as_deref().is_some_and(denied_executable_path) {
            return Err(protected(
                process.pid,
                "исполняемый файл находится в системном каталоге",
            ));
        }
        verify_live_identity(&ProcessIdentity {
            pid: process.pid,
            start_time: process.start_time,
        })
    }

    fn signal_kill(identity: &ProcessIdentity) -> Result<(), ProcessError> {
        verify_live_identity(identity)?;
        let pid = i32::try_from(identity.pid).map_err(|_| ProcessError::OpenFailed {
            pid: identity.pid,
            detail: "PID не помещается в pid_t".to_owned(),
        })?;
        // SAFETY: PID identity was refreshed immediately above; SIGKILL is fixed.
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            return Err(ProcessError::SignalFailed {
                pid: identity.pid,
                detail: io::Error::last_os_error().to_string(),
            });
        }
        Ok(())
    }

    fn verify_live_identity(identity: &ProcessIdentity) -> Result<(), ProcessError> {
        let system = System::new_all();
        let process = system.process(Pid::from_u32(identity.pid)).ok_or_else(|| {
            ProcessError::OpenFailed {
                pid: identity.pid,
                detail: "процесс не найден".to_owned(),
            }
        })?;
        if process.start_time() != identity.start_time {
            return Err(ProcessError::IdentityMismatch { pid: identity.pid });
        }
        let name = process.name().to_string_lossy();
        if denied_name(&name)
            || process.exe().is_some_and(denied_executable_path)
            || identity.pid == 1
            || identity.pid == std::process::id()
        {
            return Err(protected(identity.pid, "системный процесс"));
        }
        Ok(())
    }

    fn denied_name(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        DENIED_NAMES.contains(&name.as_str())
    }

    fn denied_executable_path(path: &Path) -> bool {
        path.starts_with("/System")
            || path.starts_with("/usr/bin")
            || path.starts_with("/usr/lib")
            || path.starts_with("/usr/sbin")
            || path.starts_with("/bin")
            || path.starts_with("/sbin")
    }

    fn protected(pid: u32, reason: &str) -> ProcessError {
        ProcessError::Protected {
            pid,
            reason: reason.to_owned(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn denylist_is_case_insensitive() {
            assert!(denied_name("WindowServer"));
            assert!(denied_name("FINDER"));
            assert!(!denied_name("ExampleWorker"));
        }
    }
}
