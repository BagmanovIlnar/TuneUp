//! Linux process termination with PID reuse protection.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use tuneup_core::{
    TuneupError,
    error::ProcessError,
    model::{ProcessIdentity, ProcessInfo},
};

/// Force-terminates Linux processes with `SIGKILL`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxProcessControl;

impl LinuxProcessControl {
    /// Validates every process, then force-kills them. Already-exited PIDs succeed.
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
    use std::{fs, io};

    use super::*;

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
                Err(ProcessError::OpenFailed { detail, .. })
                    if detail.contains("No such file or directory") => {}
                Err(error) => {
                    remaining.push(identity);
                    errors.push(error.into());
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
                Err(ProcessError::OpenFailed { detail, .. })
                    if detail.contains("No such file or directory") => {}
                Err(error) => {
                    remaining.push(identity);
                    errors.push(error.into());
                }
            }
        }
        (remaining, errors)
    }

    fn kill(identity: &ProcessIdentity) -> Result<(), ProcessError> {
        verify_identity(identity)?;
        // SAFETY: `kill` receives a positive, identity-verified PID and SIGKILL.
        if unsafe { libc::kill(identity.pid as libc::pid_t, libc::SIGKILL) } != 0 {
            return Err(ProcessError::SignalFailed {
                pid: identity.pid,
                detail: io::Error::last_os_error().to_string(),
            });
        }
        Ok(())
    }

    fn verify_identity(identity: &ProcessIdentity) -> Result<(), ProcessError> {
        let stat_path = format!("/proc/{}/stat", identity.pid);
        let stat = fs::read_to_string(&stat_path).map_err(|error| ProcessError::OpenFailed {
            pid: identity.pid,
            detail: error.to_string(),
        })?;
        let proc_stat =
            fs::read_to_string("/proc/stat").map_err(|error| ProcessError::OpenFailed {
                pid: identity.pid,
                detail: error.to_string(),
            })?;
        let boot_time = proc_stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| ProcessError::OpenFailed {
                pid: identity.pid,
                detail: "в /proc/stat нет btime".to_owned(),
            })?;
        // SAFETY: `_SC_CLK_TCK` is a read-only process configuration query.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if ticks <= 0 {
            return Err(ProcessError::OpenFailed {
                pid: identity.pid,
                detail: "sysconf(_SC_CLK_TCK) вернул ошибку".to_owned(),
            });
        }
        let actual = proc_start_time(&stat, boot_time, ticks as u64).map_err(|detail| {
            ProcessError::OpenFailed {
                pid: identity.pid,
                detail,
            }
        })?;
        if actual != identity.start_time {
            return Err(ProcessError::IdentityMismatch { pid: identity.pid });
        }
        Ok(())
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
}
