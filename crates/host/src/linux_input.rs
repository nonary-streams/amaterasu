//! Session-scoped Linux input permits. No device or socket is opened unless
//! the host policy opts in the exact, independently verified signed set.
use crate::input_companion::CompanionPolicy;
use amaterasu_input_companion::LinuxInputService;
use amaterasu_package::{BootstrapConfig, PackageResult, read_json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) struct Supervisor {
    policy: Option<CompanionPolicy>,
    service: Option<LinuxInputService>,
    worker: Option<(u32, u64)>,
    last_refresh: Instant,
}

impl Supervisor {
    pub(crate) fn new(root: &Path, digest: &str) -> PackageResult<Self> {
        let policy = CompanionPolicy::load(root, digest)?;
        let service = if policy.is_some() {
            let configuration = verified_input_configuration(root, digest)?;
            let settings = configuration
                .settings
                .get("amaterasu.input")
                .ok_or("signed set has no input settings")?;
            if settings
                .get("allow_injection")
                .and_then(|value| value.as_bool())
                != Some(true)
            {
                return Err("input companion policy requires enabled signed input settings".into());
            }
            let socket = match settings.get("linux_companion_socket_path") {
                Some(value) => PathBuf::from(
                    value
                        .as_str()
                        .ok_or("signed input companion socket path is invalid")?,
                ),
                None => default_socket_path()?,
            };
            if !socket.is_absolute() {
                return Err("signed input companion socket path must be absolute".into());
            }
            Some(
                LinuxInputService::start(&socket, Path::new("/dev/uinput"))
                    .map_err(|error| format!("Linux input companion startup failed: {error:?}"))?,
            )
        } else {
            None
        };
        Ok(Self {
            policy,
            service,
            worker: None,
            last_refresh: Instant::now(),
        })
    }

    /// Refresh at most once per second, including a re-read of the host policy.
    /// The companion expires a permit if refreshes cease for two seconds.
    pub(crate) fn refresh(
        &mut self,
        root: &Path,
        digest: &str,
        pid: u32,
        starttime: u64,
    ) -> PackageResult<()> {
        let (Some(policy), Some(_)) = (&self.policy, &self.service) else {
            return Ok(());
        };
        let policy = policy.clone();
        if self.worker == Some((pid, starttime)) && self.last_refresh.elapsed() < REFRESH_INTERVAL {
            return Ok(());
        }
        if CompanionPolicy::load(root, digest)?.as_ref() != Some(&policy) {
            self.revoke();
            return Err("Linux input companion policy changed; worker permit revoked".into());
        }
        let active: amaterasu_package::ActivationState = read_json(&root.join("selection.json"))?;
        if active.active.as_deref() != Some(digest) {
            self.revoke();
            return Err("Linux input companion selected set changed; worker permit revoked".into());
        }
        if self.worker.is_some() && self.worker != Some((pid, starttime)) {
            self.revoke();
        }
        let (keyboard, mouse, gamepad) = policy.capabilities();
        let service = self
            .service
            .as_ref()
            .ok_or("Linux input companion permit service was revoked")?;
        if let Err(error) =
            service.authorize_worker(pid, starttime, digest, keyboard, mouse, gamepad)
        {
            self.revoke();
            return Err(format!(
                "Linux input companion rejected worker permit: {error:?}"
            ));
        }
        self.worker = Some((pid, starttime));
        self.last_refresh = Instant::now();
        Ok(())
    }

    pub(crate) fn revoke(&mut self) {
        if let (Some(service), Some((pid, starttime))) = (&self.service, self.worker.take())
            && service.revoke_worker(pid, starttime).is_err()
        {
            // Dropping the service removes every session permit.
            self.service = None;
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.revoke();
    }
}

fn verified_input_configuration(
    root: &Path,
    digest: &str,
) -> PackageResult<amaterasu_contracts::RuntimeConfig> {
    let bootstrap: BootstrapConfig = read_json(&root.join("bootstrap.json"))?;
    crate::publisher_selection::with_verified_selected(
        root,
        digest,
        &bootstrap,
        crate::publisher_selection::SelectionPurpose::Worker,
        &mut |_, _| Ok(()),
        |package| {
            if !package.signed
                || package.digest != digest
                || package.manifest.host_context_schema != 1
            {
                return Err("input companion requires the verified signed active package".into());
            }
            Ok(package.runtime_config)
        },
    )
}

fn default_socket_path() -> PackageResult<PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(directory).join("amaterasu-input.sock"));
    }
    // The companion library independently checks ownership and mode of the
    // parent directory before it binds a socket.
    let uid = unsafe { libc::geteuid() };
    Ok(PathBuf::from(format!(
        "/run/user/{uid}/amaterasu-input.sock"
    )))
}

pub(crate) fn process_starttime(pid: u32) -> PackageResult<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|error| format!("cannot read supervised worker identity: {error}"))?;
    parse_starttime(&stat)
}

fn parse_starttime(stat: &str) -> PackageResult<u64> {
    let (_, fields) = stat
        .rsplit_once(") ")
        .ok_or("supervised worker stat is malformed")?;
    fields
        .split_ascii_whitespace()
        .nth(19)
        .ok_or("supervised worker start time is missing")?
        .parse::<u64>()
        .map_err(|_| "supervised worker start time is invalid".into())
}

#[cfg(test)]
mod tests {
    use super::{Supervisor, parse_starttime, process_starttime};

    #[test]
    fn stat_parser_handles_parentheses_in_process_name() {
        let mut fields = vec!["S".to_owned(); 20];
        fields[19] = "12345".into();
        let stat = format!("42 (worker ) name) {}", fields.join(" "));
        assert_eq!(parse_starttime(&stat).unwrap(), 12345);
        assert!(parse_starttime("42 (missing) S").is_err());
        assert!(process_starttime(std::process::id()).unwrap() > 0);
    }

    #[test]
    fn absent_policy_does_not_open_a_device_or_require_a_package() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-linux-input-disabled-{}",
            amaterasu_package::random_identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        let supervisor = Supervisor::new(&root, &"a".repeat(64)).unwrap();
        assert!(supervisor.service.is_none());
        std::fs::remove_dir(root).unwrap();
    }
}
