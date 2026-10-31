use tuneup_core::{
    error::TuneupError,
    model::{ProcessIdentity, ProcessInfo},
};

/// Terminates macOS processes gracefully: `SIGTERM` first, a short grace
/// period, then `SIGKILL` for survivors. Identity (pid + start_time) is
/// re-checked before every signal to survive PID reuse.
pub struct MacOsProcessControl;

impl MacOsProcessControl {
    /// Validates every process, then terminates them gracefully
    /// (`SIGTERM` → grace → `SIGKILL`). Already-exited PIDs succeed.
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
    use std::{
        io,
        path::Path,
        thread,
        time::{Duration, Instant},
    };

    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
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

    /// How long a SIGTERM'd process gets to exit cleanly before SIGKILL.
    const GRACE: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    pub fn terminate(
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        // Cheap checks from the request data reject the whole batch up front.
        let mut validation = Vec::new();
        for process in processes {
            if let Err(error) = validate_static(process) {
                validation.push(error.into());
            }
        }
        if !validation.is_empty() {
            return Err((Vec::new(), validation));
        }
        let identities = processes
            .iter()
            .map(|process| ProcessIdentity {
                pid: process.pid,
                start_time: process.start_time,
            })
            .collect();
        let (remaining, errors) = terminate_identities_impl(identities);
        if remaining.is_empty() {
            Ok(())
        } else {
            Err((remaining, errors))
        }
    }

    pub fn terminate_identities(
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        terminate_identities_impl(identities)
    }

    fn terminate_identities_impl(
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        let mut remaining = Vec::new();
        let mut errors = Vec::new();
        // One shared snapshot instead of a full system scan per PID.
        let mut system = System::new();

        // Phase 1: graceful SIGTERM for everything still matching its identity.
        let mut terminating: Vec<ProcessIdentity> = Vec::new();
        for identity in &identities {
            match verify_live_identity(&mut system, identity) {
                Ok(()) => match signal(identity.pid, libc::SIGTERM) {
                    Ok(()) => terminating.push(identity.clone()),
                    Err(error) => {
                        errors.push(error.into());
                        remaining.push(identity.clone());
                    }
                },
                // Already exited — counts as success.
                Err(ProcessError::OpenFailed { detail, .. }) if detail == "процесс не найден" =>
                    {}
                Err(error) => {
                    errors.push(error.into());
                    remaining.push(identity.clone());
                }
            }
        }

        // Phase 2: give processes time to save state and exit.
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline && !terminating.is_empty() {
            thread::sleep(POLL_INTERVAL);
            terminating.retain(|identity| verify_live_identity(&mut system, identity).is_ok());
        }

        // Phase 3: SIGKILL the survivors.
        for identity in &terminating {
            match verify_live_identity(&mut system, identity) {
                Ok(()) => {
                    if let Err(error) = signal(identity.pid, libc::SIGKILL) {
                        errors.push(error.into());
                        remaining.push(identity.clone());
                    }
                }
                Err(_) => {
                    // Exited (or PID reused) between the poll and now — done.
                }
            }
        }
        (remaining, errors)
    }

    fn validate_static(process: &ProcessInfo) -> Result<(), ProcessError> {
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
        Ok(())
    }

    fn signal(pid: u32, signal: libc::c_int) -> Result<(), ProcessError> {
        let pid = i32::try_from(pid).map_err(|_| ProcessError::OpenFailed {
            pid,
            detail: "PID не помещается в pid_t".to_owned(),
        })?;
        // SAFETY: PID identity was verified by the caller immediately before;
        // the signal number is a fixed libc constant.
        if unsafe { libc::kill(pid, signal) } != 0 {
            return Err(ProcessError::SignalFailed {
                pid: pid as u32,
                detail: io::Error::last_os_error().to_string(),
            });
        }
        Ok(())
    }

    /// Verifies (and refreshes) a single PID inside the shared snapshot.
    fn verify_live_identity(
        system: &mut System,
        identity: &ProcessIdentity,
    ) -> Result<(), ProcessError> {
        let pid = Pid::from_u32(identity.pid);
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::everything().without_tasks(),
        );
        let process = system
            .process(pid)
            .ok_or_else(|| ProcessError::OpenFailed {
                pid: identity.pid,
                detail: "процесс не найден".to_owned(),
            })?;
        if process.start_time() != identity.start_time {
            return Err(ProcessError::IdentityMismatch { pid: identity.pid });
        }
        let name = process.name().to_string_lossy();
        if denied_name(&name)
            || process.exe().is_some_and(denied_executable_path)
            || identity.pid <= 1
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

    /// End-to-end check of the graceful terminate path against a real process.
    ///
    /// The worker is a second instance of the test binary itself (Apple blocks
    /// running copied system binaries), filtered down to a single ignored test
    /// that sleeps.
    #[cfg(test)]
    mod graceful_tests {
        use std::process::{Command, Stdio};
        use std::time::Duration;

        use super::*;

        #[test]
        #[ignore = "worker process; spawned by terminate_gracefully_stops_spawned_worker"]
        fn sleep_worker() {
            if std::env::var_os("TUNEUP_TEST_WORKER").is_none() {
                return;
            }
            std::thread::sleep(Duration::from_secs(60));
        }

        #[test]
        fn terminate_gracefully_stops_spawned_worker() {
            let exe = std::env::current_exe().expect("current exe");
            let mut child = Command::new(exe)
                .args([
                    "--exact",
                    "sleep_worker",
                    "--ignored",
                    "--test-threads",
                    "1",
                ])
                .env("TUNEUP_TEST_WORKER", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn worker");
            let pid = child.id();
            let mut system = System::new();
            let info = loop {
                system.refresh_processes_specifics(
                    ProcessesToUpdate::All,
                    true,
                    ProcessRefreshKind::everything().without_tasks(),
                );
                if let Some(process) = system.process(Pid::from_u32(pid))
                    && process.start_time() > 0
                {
                    break ProcessInfo {
                        pid,
                        name: process.name().to_string_lossy().into_owned(),
                        path: process.exe().map(ToOwned::to_owned),
                        parent_pid: process.parent().map(|parent| parent.as_u32()),
                        start_time: process.start_time(),
                        cpu_usage: 0.0,
                        memory_bytes: 0,
                    };
                }
                std::thread::sleep(Duration::from_millis(50));
            };

            let result = terminate(&[info]);
            assert!(result.is_ok(), "graceful terminate failed: {result:?}");
            // The worker must actually be gone (SIGTERM suffices for libtest).
            let mut exited = false;
            for _ in 0..50 {
                if child.try_wait().ok().flatten().is_some() {
                    exited = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = child.kill();
            let _ = child.wait();
            assert!(exited, "worker did not exit after terminate");
        }
    }
}
