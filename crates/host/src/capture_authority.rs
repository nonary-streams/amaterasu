//! The root capture supervisor hands this worker one private channel before
//! dropping privileges. This module never opens a privileged service socket.
use amaterasu_package::{PackageResult, VerifiedPackage};
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::OnceLock,
};

const CHANNEL_FD: i32 = 198;
struct AuthorityContext {
    component: String,
    digest: String,
    channel: OwnedFd,
    source_preflight: bool,
}
static AUTHORITY: OnceLock<AuthorityContext> = OnceLock::new();

/// Adopts only the private root-peer channel. A caller naming a component or
/// setting a number in package JSON does not create authority.
pub fn adopt(component: &str, digest: &str) -> PackageResult<()> {
    adopt_protocol(component, digest, false)
}

/// The precheck-aware protected launcher uses a distinct command. A legacy
/// launcher keeps the legacy command and can never authorize a new opcode.
pub fn adopt_v2(component: &str, digest: &str) -> PackageResult<()> {
    adopt_protocol(component, digest, true)
}

fn adopt_protocol(component: &str, digest: &str, source_preflight: bool) -> PackageResult<()> {
    if !amaterasu_contracts::valid_name(component)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid approved worker identity".into());
    }
    crate::privilege::require_user_process()?;
    // SAFETY: fcntl checks descriptor existence without taking ownership.
    if unsafe { libc::fcntl(CHANNEL_FD, libc::F_GETFD) } < 0 {
        return Err("approved worker is missing its private authority channel".into());
    }
    let mut kind = 0i32;
    let mut kind_len = std::mem::size_of::<i32>() as libc::socklen_t;
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut peer_len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: all output pointers name initialized stack storage of stated size.
    let valid = unsafe {
        libc::getsockopt(CHANNEL_FD, libc::SOL_SOCKET, libc::SO_TYPE, (&mut kind as *mut i32).cast(), &mut kind_len) == 0
        && kind == libc::SOCK_SEQPACKET
        && libc::getsockopt(CHANNEL_FD, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut peer as *mut libc::ucred).cast(), &mut peer_len) == 0
        && peer.uid == 0
        && libc::fcntl(CHANNEL_FD, libc::F_SETFD, libc::FD_CLOEXEC) == 0
        // Exec resets dumpability, so reassert it in this unprivileged process
        // before native code or user-facing service startup.
        && libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0
        && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
    };
    if !valid {
        return Err("approved worker channel is not a private root-peer seqpacket socket".into());
    }
    // SAFETY: successful adoption transfers fd198 into this single owner; it is
    // not duplicated into subsequently launched executables (FD_CLOEXEC).
    let channel = unsafe { OwnedFd::from_raw_fd(CHANNEL_FD) };
    AUTHORITY
        .set(AuthorityContext {
            component: component.to_owned(),
            digest: digest.to_owned(),
            channel,
            source_preflight,
        })
        .map_err(|_| "approved capture channel already adopted".to_owned())
}

pub fn inject(
    package: &VerifiedPackage,
    settings: &mut std::collections::BTreeMap<String, serde_json::Value>,
) -> PackageResult<()> {
    let precheck_requests = reserved_settings(settings)?;
    if let Some(authority) = AUTHORITY.get() {
        if package.digest != authority.digest
            || !package.privileged_requests.iter().any(|r| {
                r.component == authority.component && r.manifest_digest == authority.digest
            })
        {
            return Err(
                "loaded package does not match the private capture authority channel".into(),
            );
        }
        let value = settings
            .get_mut(&authority.component)
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("approved capture component has no settings object")?;
        if value
            .get("authority_channel")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return Err("approved capture component did not select its authority channel".into());
        }
        value.insert(
            "authority_channel_fd".into(),
            authority.channel.as_raw_fd().into(),
        );
        // Only new components explicitly requesting this reserved injection
        // receive it; old deny-unknown-fields schemas remain byte-compatible.
        inject_precheck(
            value,
            precheck_requests.contains(&authority.component),
            authority.source_preflight,
        );
    }
    for (component, value) in settings.iter() {
        if value
            .get("authority_channel")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && !AUTHORITY
                .get()
                .is_some_and(|authority| authority.component == *component)
        {
            return Err(format!(
                "{component} requires a package-approved capture authority launch"
            ));
        }
    }
    Ok(())
}

fn reserved_settings(
    settings: &mut std::collections::BTreeMap<String, serde_json::Value>,
) -> PackageResult<std::collections::BTreeSet<String>> {
    let mut requests = std::collections::BTreeSet::new();
    for (component, value) in settings {
        if let Some(object) = value.as_object_mut() {
            object.remove("authority_channel_fd");
            match object.remove("authority_precheck_supported") {
                None => {}
                Some(serde_json::Value::Bool(false)) => {
                    requests.insert(component.clone());
                }
                Some(_) => {
                    return Err(
                        "authority_precheck_supported is a reserved false injection placeholder"
                            .into(),
                    );
                }
            }
        }
    }
    Ok(requests)
}

fn inject_precheck(
    settings: &mut serde_json::Map<String, serde_json::Value>,
    requested: bool,
    supported: bool,
) {
    if requested {
        settings.insert("authority_precheck_supported".into(), supported.into());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn protocol_support_is_reserved_and_old_component_settings_stay_unchanged() {
        use serde_json::json;
        for supported in [false, true] {
            let mut settings = std::collections::BTreeMap::from([
                (
                    "old.capture".into(),
                    json!({"enabled":true,"authority_channel_fd":100}),
                ),
                (
                    "new.capture".into(),
                    json!({"enabled":true,"authority_precheck_supported":false}),
                ),
            ]);
            let requests = super::reserved_settings(&mut settings).unwrap();
            assert_eq!(requests, ["new.capture".to_owned()].into());
            for (component, value) in &mut settings {
                super::inject_precheck(
                    value.as_object_mut().unwrap(),
                    requests.contains(component),
                    supported,
                );
            }
            assert_eq!(settings["old.capture"], json!({"enabled":true}));
            assert_eq!(
                settings["new.capture"]["authority_precheck_supported"],
                supported
            );
        }
        for invalid in [json!(true), json!(1), json!(null)] {
            let mut settings = std::collections::BTreeMap::from([(
                "capture".into(),
                json!({"authority_precheck_supported":invalid}),
            )]);
            assert!(super::reserved_settings(&mut settings).is_err());
        }
    }
    #[test]
    #[ignore = "requires root to launch a disposable unprivileged authority worker"]
    fn approved_channel_hardening_survives_exec() {
        use std::os::unix::process::CommandExt;
        if std::env::var_os("AMATERASU_TEST_AUTHORITY_CHILD").is_some() {
            super::adopt("example.capture", &"a".repeat(64)).unwrap();
            // SAFETY: these queries observe this test child's own process/fd.
            assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }, 0);
            assert_eq!(
                unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) },
                1
            );
            assert_ne!(
                unsafe { libc::fcntl(super::CHANNEL_FD, libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
            return;
        }
        // SAFETY: getuid only reads the process identity.
        assert_eq!(
            unsafe { libc::getuid() },
            0,
            "run this ignored case as root"
        );
        let mut sockets = [-1; 2];
        // SAFETY: socketpair receives a writable two-element output array.
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    sockets.as_mut_ptr(),
                )
            },
            0
        );
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        // SAFETY: socketpair returned two fresh descriptors, adopted exactly once.
        let (_parent, child) = unsafe {
            (
                OwnedFd::from_raw_fd(sockets[0]),
                OwnedFd::from_raw_fd(sockets[1]),
            )
        };
        let fd = child.as_raw_fd();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg("capture_authority::tests::approved_channel_hardening_survives_exec")
            .arg("--exact")
            .arg("--ignored")
            .env_clear()
            .env("AMATERASU_TEST_AUTHORITY_CHILD", "1");
        // SAFETY: only async-signal-safe syscalls execute in forked child.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(fd, super::CHANNEL_FD) < 0
                    || libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setresgid(65534, 65534, 65534) != 0
                    || libc::setresuid(65534, 65534, 65534) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        assert!(command.status().unwrap().success());
    }

    #[test]
    fn arbitrary_caller_cannot_adopt_a_privileged_channel() {
        // The normal test runner has no root-peer inherited fd198.
        assert!(super::adopt("example.capture", &"a".repeat(64)).is_err());
    }
}
