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
        "[Unit]\nDescription=Amaterasu LinuxSession ({})\n# HostExecutableSha256={host_executable_digest}\nAfter=graphical-session.target\nPartOf=graphical-session.target\nStartLimitIntervalSec=0\n\n[Service]\nType=simple\nExecStart={executable} service-user {root} {digest}\nRestart=on-failure\nRestartSec=5s\nKillSignal=SIGTERM\nKillMode=mixed\nTimeoutStopSec=20s\n\n[Install]\nWantedBy=graphical-session.target\n",
        &digest[..12],
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
