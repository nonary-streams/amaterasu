//! One lookup per process: the provider can terminate and reap an OS resolver
//! call without leaving code executing inside an unloaded native library.

#[allow(dead_code)]
#[path = "helperprotocol.rs"]
pub(crate) mod helperprotocol;

use std::ffi::OsString;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

use helperprotocol::{MAX_ADDRESSES, Request, Response};

fn resolve(request: Request) -> io::Result<Vec<SocketAddr>> {
    collect_addresses((request.host.as_str(), request.port).to_socket_addrs()?)
}

fn collect_addresses(
    resolved: impl IntoIterator<Item = SocketAddr>,
) -> io::Result<Vec<SocketAddr>> {
    let mut addresses = Vec::new();
    for address in resolved {
        if !addresses.contains(&address) {
            if addresses.len() == MAX_ADDRESSES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "too many DNS addresses",
                ));
            }
            addresses.push(address);
        }
    }
    if addresses.is_empty() {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    Ok(addresses)
}

pub(crate) fn parent_process_id(arguments: impl IntoIterator<Item = OsString>) -> io::Result<u32> {
    let mut arguments = arguments.into_iter();
    let flag = arguments.next();
    let value = arguments.next();
    if flag.as_deref() != Some(std::ffi::OsStr::new("--parent-pid")) || arguments.next().is_some() {
        return Err(invalid_parent_argument());
    }
    value
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|parent| *parent > 0)
        .ok_or_else(invalid_parent_argument)
}

fn invalid_parent_argument() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "resolver helper requires --parent-pid followed by one nonzero process ID",
    )
}

#[cfg(target_os = "linux")]
pub(crate) fn prepare_parent_lifetime(expected_parent: u32) -> io::Result<()> {
    use std::ffi::{c_int, c_ulong};

    const PR_SET_PDEATHSIG: c_int = 1;
    const SIGKILL: c_ulong = 9;

    unsafe extern "C" {
        fn prctl(option: c_int, ...) -> c_int;
        fn getppid() -> c_int;
    }

    // SAFETY: These libc calls have no pointer arguments or retained Rust state.
    // All optional prctl arguments are supplied as the ABI's unsigned-long values.
    // SIGKILL terminates the helper if the creating provider thread dies; checking
    // the exact parent immediately after arming closes the pre-prctl parent-exit race.
    if unsafe {
        prctl(
            PR_SET_PDEATHSIG,
            SIGKILL,
            0 as c_ulong,
            0 as c_ulong,
            0 as c_ulong,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: getppid takes no arguments and returns the kernel-owned process ID.
    if u32::try_from(unsafe { getppid() }).ok() != Some(expected_parent) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resolver helper parent changed before lifetime containment was armed",
        ));
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn prepare_parent_lifetime(expected_parent: u32) -> io::Result<()> {
    prepare_parent_watchdog(expected_parent)
}

#[cfg(any(all(unix, not(target_os = "linux")), all(unix, test)))]
fn prepare_parent_watchdog(expected_parent: u32) -> io::Result<()> {
    use std::ffi::c_int;

    unsafe extern "C" {
        fn getppid() -> c_int;
    }

    fn parent_matches(expected_parent: u32) -> bool {
        // SAFETY: the Unix libc function takes no pointers and returns the
        // kernel-owned parent process ID. It retains no Rust state.
        u32::try_from(unsafe { getppid() }).ok() == Some(expected_parent)
    }

    if !parent_matches(expected_parent) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "resolver helper parent changed before lifetime containment was armed",
        ));
    }
    // macOS has no Linux-style parent-death signal. This thread lives entirely
    // inside the independent helper executable, never in the provider library.
    // Start it before entering NSS and terminate the entire helper if its exact
    // parent changes; a blocked name-service call cannot keep an orphan alive.
    std::thread::Builder::new()
        .name("resolver-parent-watchdog".into())
        .spawn(move || {
            loop {
                if !parent_matches(expected_parent) {
                    std::process::exit(1);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })?;
    Ok(())
}

#[cfg(windows)]
pub(crate) fn prepare_parent_lifetime(_expected_parent: u32) -> io::Result<()> {
    // Windows child lifetime is contained atomically by the parent's Job object.
    // This worker performs no executable lookup or native fallback of its own.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn prepare_parent_lifetime(_expected_parent: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "resolver helper lifetime containment is unavailable on this platform",
    ))
}

fn main() {
    let result = parent_process_id(std::env::args_os().skip(1))
        .and_then(prepare_parent_lifetime)
        .and_then(|()| Request::read_from(io::stdin().lock()))
        .and_then(resolve);
    if Response::from_result(result)
        .write_to(io::stdout().lock())
        .is_err()
    {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_preserves_first_result_order_and_deduplicates() {
        let ipv4 = "127.0.0.1:48000".parse().unwrap();
        let ipv6 = "[::1]:48000".parse().unwrap();
        assert_eq!(
            collect_addresses([ipv6, ipv4, ipv6, ipv4]).unwrap(),
            vec![ipv6, ipv4]
        );
    }

    #[test]
    fn collection_rejects_empty_and_overfull_results_without_silent_truncation() {
        assert_eq!(
            collect_addresses([]).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let addresses = (0..=MAX_ADDRESSES)
            .map(|port| SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), port as u16));
        assert_eq!(
            collect_addresses(addresses).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn parent_argument_requires_one_nonzero_decimal_process_id() {
        assert_eq!(
            parent_process_id(["--parent-pid".into(), "1234".into()]).unwrap(),
            1234
        );
        for arguments in [
            vec![],
            vec!["--parent-pid"],
            vec!["--wrong", "1234"],
            vec!["--parent-pid", "0"],
            vec!["--parent-pid", "-1"],
            vec!["--parent-pid", "+1"],
            vec!["--parent-pid", "4294967296"],
            vec!["--parent-pid", "1234", "extra"],
        ] {
            assert_eq!(
                parent_process_id(arguments.into_iter().map(OsString::from))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_watchdog_rejects_parent_mismatch_before_starting() {
        assert_eq!(
            prepare_parent_watchdog(u32::MAX).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_watchdog_keeps_the_helper_alive_while_its_exact_parent_lives() {
        unsafe extern "C" {
            fn getppid() -> std::ffi::c_int;
        }
        // SAFETY: getppid takes no arguments and retains no Rust state.
        let parent = u32::try_from(unsafe { getppid() }).unwrap();
        prepare_parent_watchdog(parent).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(110));
    }
}
