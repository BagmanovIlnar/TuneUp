//! Linux process termination with PID reuse protection.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use tuneup_core::{
    TuneupError,
    error::ProcessError,
    model::{ProcessIdentity, ProcessInfo},
};

/// Terminates Linux processes with PID reuse protection: `SIGTERM` first,
/// a short grace period, then `SIGKILL` for survivors.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxProcessControl;

impl LinuxProcessControl {
    /// Validates every process, then terminates them gracefully
    /// (`SIGTERM` → grace → `SIGKILL`). Already-exited PIDs succeed.
    ///
    /// # Example
    ///
    /// ```ignore
    /// LinuxProcessControl.terminate(&group.processes)?;
    /// ```
    pub fn terminate(
        &self,
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        let errors = processes
            .iter()
            .filter_map(validate_process)
            .map(TuneupError::from)
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            return Err((Vec::new(), errors));
        }
        imp::terminate(processes)
    }

    /// Force-kills previously recorded identities (recovery / migration).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (remaining, errors) = LinuxProcessControl.terminate_identities(stale);
    /// ```
    pub fn terminate_identities(
        &self,
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        imp::terminate_identities(identities)
    }
}

impl tuneup_platform::ProcessControl for LinuxProcessControl {
    fn terminate(
        &self,
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        LinuxProcessControl::terminate(self, processes)
    }

    fn terminate_identities(
        &self,
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        LinuxProcessControl::terminate_identities(self, identities)
    }
}

fn validate_process(process: &ProcessInfo) -> Option<ProcessError> {
    const DENYLIST: &[&str] = &[
        "init",
        "systemd",
        "systemd-logind",
        "systemd-journald",
        "systemd-udevd",
        "kthreadd",
        "dbus-daemon",
        "polkitd",
        "sshd",
        "login",
        "agetty",
    ];
    if process.pid <= 1 || process.pid == std::process::id() {
        return Some(ProcessError::Protected {
            pid: process.pid,
            reason: "init или собственный процесс".to_owned(),
        });
    }
    let name = process.name.to_ascii_lowercase();
    if DENYLIST.contains(&name.as_str())
        || name.starts_with("systemd-")
        || (name.starts_with('[') && name.ends_with(']'))
    {
        return Some(ProcessError::Protected {
            pid: process.pid,
            reason: format!("защищённое имя {}", process.name),
        });
    }
    if process.path.as_deref().is_some_and(protected_executable) {
        return Some(ProcessError::Protected {
            pid: process.pid,
            reason: "системный executable".to_owned(),
        });
    }
    None
}

fn protected_executable(path: &std::path::Path) -> bool {
    ["/sbin", "/usr/sbin", "/lib", "/usr/lib"]
        .iter()
        .any(|root| path.starts_with(root))
}

fn proc_start_time(stat: &str, boot_time: u64, ticks_per_second: u64) -> Result<u64, String> {
    let close = stat
        .rfind(')')
        .ok_or_else(|| "нет закрывающей скобки comm".to_owned())?;
    let tail = stat
        .get(close + 1..)
        .ok_or_else(|| "повреждён /proc stat".to_owned())?;
    let ticks = tail
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| "нет поля starttime".to_owned())?
        .parse::<u64>()
        .map_err(|error| error.to_string())?;
    Ok(boot_time.saturating_add(ticks / ticks_per_second.max(1)))
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    pub fn terminate(
        _processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
        Err((
            Vec::new(),
            vec![TuneupError::Process(ProcessError::SignalFailed {
                pid: 0,
                detail: "SIGKILL доступен только в Linux".to_owned(),
            })],
        ))
    }

    pub fn terminate_identities(
        identities: Vec<ProcessIdentity>,
    ) -> (Vec<ProcessIdentity>, Vec<TuneupError>) {
        (
            identities,
            vec![TuneupError::Process(ProcessError::SignalFailed {
                pid: 0,
                detail: "SIGKILL доступен только в Linux".to_owned(),
            })],
        )
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::{
        fs, io, thread,
        time::{Duration, Instant},
    };

    use super::*;

    /// How long a SIGTERM'd process gets to exit cleanly before SIGKILL.
    const GRACE: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(100);

    pub fn terminate(
        processes: &[ProcessInfo],
    ) -> Result<(), (Vec<ProcessIdentity>, Vec<TuneupError>)> {
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

        // Phase 1: graceful SIGTERM for everything still matching its identity.
        let mut terminating: Vec<ProcessIdentity> = Vec::new();
        for identity in &identities {
            match signal_identity(identity, libc::SIGTERM) {
                Ok(()) => terminating.push(identity.clone()),
                // Already exited — counts as success.
                Err(ProcessError::OpenFailed { detail, .. })
                    if detail.contains("No such file or directory") => {}
                Err(error) => {
                    remaining.push(identity.clone());
                    errors.push(error.into());
                }
            }
        }

        // Phase 2: give processes time to save state and exit.
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline && !terminating.is_empty() {
            thread::sleep(POLL_INTERVAL);
            terminating.retain(|identity| verify_identity(identity).is_ok());
        }

        // Phase 3: SIGKILL the survivors.
        for identity in &terminating {
            match signal_identity(identity, libc::SIGKILL) {
                Ok(()) => {}
                Err(ProcessError::OpenFailed { detail, .. })
                    if detail.contains("No such file or directory") => {}
                Err(error) => {
                    remaining.push(identity.clone());
                    errors.push(error.into());
                }
            }
        }
        (remaining, errors)
    }

    fn signal_identity(
        identity: &ProcessIdentity,
        signal: libc::c_int,
    ) -> Result<(), ProcessError> {
        verify_identity(identity)?;
        // SAFETY: `kill` receives a positive, identity-verified PID and a libc signal.
        if unsafe { libc::kill(identity.pid as libc::pid_t, signal) } != 0 {
            return Err(ProcessError::SignalFailed {
                pid: identity.pid,
                detail: io::Error::last_os_error().to_string(),
            });
        }
        Ok(())
    }

    fn verify_identity(identity: &ProcessIdentity) -> Result<(), ProcessError> {
        let actual =
            current_start_time(identity.pid).map_err(|detail| ProcessError::OpenFailed {
                pid: identity.pid,
                detail,
            })?;
        if actual != identity.start_time {
            return Err(ProcessError::IdentityMismatch { pid: identity.pid });
        }
        Ok(())
    }

    /// Reads the current start_time of a PID straight from `/proc`.
    pub(super) fn current_start_time(pid: u32) -> Result<u64, String> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
        let proc_stat = fs::read_to_string("/proc/stat").map_err(|e| e.to_string())?;
        let boot_time = proc_stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| "в /proc/stat нет btime".to_owned())?;
        // SAFETY: `_SC_CLK_TCK` is a read-only process configuration query.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks <= 0 {
            return Err("sysconf(_SC_CLK_TCK) вернул ошибку".to_owned());
        }
        proc_start_time(&stat, boot_time, ticks as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_start_time_after_parenthesized_name() {
        let fields = (3..=21).map(|value| value.to_string()).collect::<Vec<_>>();
        let stat = format!("42 (name with ) spaces) {} 500 23", fields.join(" "));
        assert_eq!(proc_start_time(&stat, 1_000, 100).unwrap(), 1_005);
    }

    #[test]
    fn denies_system_executables() {
        let process = ProcessInfo {
            pid: 100,
            name: "unknown".to_owned(),
            path: Some("/usr/sbin/unknown".into()),
            parent_pid: Some(1),
            start_time: 1,
            cpu_usage: 0.0,
            memory_bytes: 0,
        };
        assert!(validate_process(&process).is_some());
    }

    /// End-to-end check of the graceful terminate path against a real process
    /// (a second instance of the test binary running one ignored sleeping test).
    #[cfg(target_os = "linux")]
    mod graceful_tests {
        use std::process::{Command, Stdio};
        use std::time::Duration;

        use super::super::imp;
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
            let identity = loop {
                if let Ok(start_time) = imp::current_start_time(pid) {
                    break ProcessIdentity { pid, start_time };
                }
                std::thread::sleep(Duration::from_millis(50));
            };

            let result = imp::terminate(&[identity.into_info()]);
            assert!(result.is_ok(), "graceful terminate failed: {result:?}");
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

        trait IdentityExt {
            fn into_info(self) -> ProcessInfo;
        }
        impl IdentityExt for ProcessIdentity {
            fn into_info(self) -> ProcessInfo {
                ProcessInfo {
                    pid: self.pid,
                    name: "sleep_worker".to_owned(),
                    path: None,
                    parent_pid: None,
                    start_time: self.start_time,
                    cpu_usage: 0.0,
                    memory_bytes: 0,
                }
            }
        }
    }
}
