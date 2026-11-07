//! Canonical systemd user unit rendering shared by release and host.

use crate::PackageResult;
use std::path::Path;

pub fn render_service_unit(
    executable: &Path,
    root: &Path,
    digest: &str,
    host_executable_digest: &str,
) -> PackageResult<String> {
    if !valid_digest(digest) || !valid_digest(host_executable_digest) {
        return Err("service unit digests must be SHA-256 values".into());
    }
    let executable = systemd_quote(executable)?;
    let root = systemd_quote(root)?;
    Ok(format!(
        "[Unit]\nDescription=Amaterasu LinuxSession ({})\n# HostExecutableSha256={host_executable_digest}\nAfter=graphical-session.target\nPartOf=graphical-session.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart={executable} service-user {root} {digest}\nRestart=on-failure\nRestartSec=5s\nKillSignal=SIGTERM\nKillMode=mixed\nTimeoutStopSec={stop_seconds}s\n\n[Install]\nWantedBy=graphical-session.target\n",
        &digest[..12],
        stop_seconds = amaterasu_contracts::HOST_SERVICE_STOP_TIMEOUT_MS / 1_000,
    ))
}

pub fn systemd_quote(path: &Path) -> PackageResult<String> {
    let value = path
        .to_str()
        .ok_or("systemd unit paths must be valid UTF-8")?;
    if value.chars().any(char::is_control) {
        return Err("systemd unit paths cannot contain control characters".into());
    }
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            value => quoted.push(value),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod shutdown_budget_tests {
    use super::*;
    use amaterasu_contracts::{
        HOST_MODULE_SHUTDOWN_TIMEOUT_MS, HOST_SERVICE_STOP_TIMEOUT_MS,
        HOST_WORKER_SHUTDOWN_TIMEOUT_MS,
    };

    #[test]
    fn service_deadline_outlasts_worker_and_module_drain() {
        let unit = render_service_unit(
            Path::new("/test/amaterasu"),
            Path::new("/test/root"),
            &"a".repeat(64),
            &"b".repeat(64),
        )
        .unwrap();
        let timeout = unit
            .lines()
            .find_map(|line| line.strip_prefix("TimeoutStopSec="))
            .unwrap()
            .strip_suffix('s')
            .unwrap()
            .parse::<u64>()
            .unwrap()
            * 1_000;
        let budgets = [
            HOST_MODULE_SHUTDOWN_TIMEOUT_MS,
            HOST_WORKER_SHUTDOWN_TIMEOUT_MS,
            timeout,
        ];
        assert!(budgets.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(timeout, HOST_SERVICE_STOP_TIMEOUT_MS);
    }
}
