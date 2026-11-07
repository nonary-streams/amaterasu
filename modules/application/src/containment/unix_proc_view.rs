//! A conservative Linux procfs view for process-group retirement checks.
//!
//! The mount configuration must be controlled by a trusted administrator and
//! remain stable during a scan. In particular, a concurrent fsconfig change
//! could be changed back between the two snapshots and evade detection.

use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

const PROC_SUPER_MAGIC: libc::c_long = 0x9fa0;
const RESOLVE_NO_XDEV: u64 = 0x01;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_BENEATH: u64 = 0x08;
const MAX_PROC_ENTRIES: usize = 1_048_576;
const MAX_PROC_FILE: u64 = 1024 * 1024;
const MAX_MOUNTINFO: u64 = 16 * 1024 * 1024;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[derive(Debug, Eq, PartialEq)]
struct MountSnapshot {
    mount_id: u64,
    mountinfo_line: String,
}

/// Owns the exact procfs mount throughout enumeration and stat reads.
pub(super) struct ProcView {
    directory: OwnedFd,
    initial: MountSnapshot,
    enumeration_state: Cell<u8>,
}

impl ProcView {
    pub(super) fn open() -> io::Result<Self> {
        let directory = open_proc_directory()?;
        let initial = snapshot(&directory)?;
        let current_directory = open_proc_directory()?;
        if initial != snapshot(&current_directory)? {
            return Err(io::Error::other("procfs mount changed before process scan"));
        }
        Ok(Self {
            directory,
            initial,
            enumeration_state: Cell::new(0),
        })
    }

    pub(super) fn self_stat(&self) -> io::Result<String> {
        self.read_current_process_file("stat", MAX_PROC_FILE)
    }

    pub(super) fn self_status(&self) -> io::Result<String> {
        self.read_current_process_file("status", MAX_PROC_FILE)
    }

    fn read_current_process_file(&self, file: &str, max_bytes: u64) -> io::Result<String> {
        // `self` is a procfs magic link and cannot be used with NO_MAGICLINKS.
        // The caller must validate these records against its current PID and
        // PID namespace before using them as evidence.
        read_beneath(
            &self.directory,
            &format!("{}/{file}", std::process::id()),
            max_bytes,
        )
    }

    /// Enumerate once per view. The duplicate shares the directory offset.
    pub(super) fn process_ids(&self) -> io::Result<Vec<u32>> {
        if self.enumeration_state.get() != 0 {
            return Err(io::Error::other("procfs view was already enumerated"));
        }
        self.enumeration_state.set(1);
        // fdopendir owns the duplicate, while `self.directory` remains open.
        // SAFETY: fcntl duplicates a valid, live directory descriptor.
        let duplicate =
            unsafe { libc::fcntl(self.directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: duplicate is an owned descriptor; fdopendir takes ownership on success.
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            // SAFETY: fdopendir did not take ownership after failure.
            unsafe { libc::close(duplicate) };
            return Err(io::Error::last_os_error());
        }
        let stream = DirectoryStream(stream);
        let mut process_ids = Vec::new();
        let mut count = 0usize;
        loop {
            // SAFETY: Linux provides thread-local errno, and the stream is live.
            unsafe { *libc::__errno_location() = 0 };
            // SAFETY: the directory stream is exclusively owned by this function.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error);
                }
                break;
            }
            count += 1;
            if count > MAX_PROC_ENTRIES {
                return Err(io::Error::other("procfs directory scan exceeded its bound"));
            }
            // SAFETY: readdir returns a valid NUL-terminated d_name until its next call.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if !name.is_empty() && name.iter().all(u8::is_ascii_digit) {
                let text = std::str::from_utf8(name).map_err(invalid_data)?;
                let process_id = text.parse::<u32>().map_err(invalid_data)?;
                if process_id == 0 || process_id.to_string() != text {
                    return Err(io::Error::other("invalid procfs process directory"));
                }
                process_ids.push(process_id);
            }
        }
        self.enumeration_state.set(2);
        Ok(process_ids)
    }

    pub(super) fn process_stat(&self, process_id: u32) -> io::Result<String> {
        if process_id == 0 {
            return Err(io::Error::other("invalid procfs process ID"));
        }
        read_beneath(
            &self.directory,
            &format!("{process_id}/stat"),
            MAX_PROC_FILE,
        )
    }

    /// Must succeed after the complete scan before the scan can prove absence.
    pub(super) fn finish(&self) -> io::Result<()> {
        if self.enumeration_state.get() != 2 {
            return Err(io::Error::other("procfs enumeration did not complete"));
        }
        let held = snapshot(&self.directory)?;
        let current_directory = open_proc_directory()?;
        let current = snapshot(&current_directory)?;
        if self.initial != held || held != current {
            return Err(io::Error::other("procfs mount changed during process scan"));
        }
        Ok(())
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this wrapper uniquely owns the fdopendir stream.
        unsafe { libc::closedir(self.0) };
    }
}

fn invalid_data(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn open_proc_directory() -> io::Result<OwnedFd> {
    let path = c"/proc";
    // SAFETY: path is NUL-terminated and open returns a new owned descriptor.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is newly opened and owned by this function.
    let directory = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: zeroed statfs is writable and directory remains live.
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(directory.as_raw_fd(), &mut info) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if info.f_type != PROC_SUPER_MAGIC {
        return Err(io::Error::other("process scan requires procfs"));
    }
    Ok(directory)
}

fn open_beneath(directory: RawFd, path: &str) -> io::Result<OwnedFd> {
    let path = CString::new(path).map_err(invalid_data)?;
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: RESOLVE_BENEATH | RESOLVE_NO_XDEV | RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: the open_how and path pointers remain valid for the syscall.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory,
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat2 returns a newly owned file descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

fn read_beneath(directory: &OwnedFd, path: &str, max_bytes: u64) -> io::Result<String> {
    let descriptor = open_beneath(directory.as_raw_fd(), path)?;
    let mut bytes = Vec::new();
    File::from(descriptor)
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::other("procfs file exceeded its size bound"));
    }
    String::from_utf8(bytes).map_err(invalid_data)
}

fn snapshot(directory: &OwnedFd) -> io::Result<MountSnapshot> {
    let pid = std::process::id();
    // SAFETY: gettid has no arguments and returns this calling task's ID.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    if tid <= 0 {
        return Err(io::Error::other("cannot identify procfs scanning task"));
    }
    let task = format!("{pid}/task/{tid}");
    let fdinfo = read_beneath(
        directory,
        &format!("{task}/fdinfo/{}", directory.as_raw_fd()),
        MAX_PROC_FILE,
    )?;
    let mount_id = parse_fdinfo_mount_id(&fdinfo)?;
    let mountinfo = read_beneath(directory, &format!("{task}/mountinfo"), MAX_MOUNTINFO)?;
    let mountinfo_line = validate_mountinfo(&mountinfo, mount_id)?;
    Ok(MountSnapshot {
        mount_id,
        mountinfo_line,
    })
}

fn parse_fdinfo_mount_id(fdinfo: &str) -> io::Result<u64> {
    let mut found = None;
    for line in fdinfo.lines() {
        if let Some(value) = line.strip_prefix("mnt_id:") {
            let id = value.trim().parse::<u64>().map_err(invalid_data)?;
            if id == 0 || found.replace(id).is_some() {
                return Err(io::Error::other("invalid procfs fdinfo mount ID"));
            }
        }
    }
    found.ok_or_else(|| io::Error::other("procfs fdinfo has no mount ID"))
}

fn validate_mountinfo(mountinfo: &str, mount_id: u64) -> io::Result<String> {
    let mut found = None;
    for line in mountinfo.lines() {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        let id = fields
            .first()
            .ok_or_else(|| io::Error::other("malformed mountinfo"))?
            .parse::<u64>()
            .map_err(invalid_data)?;
        if id != mount_id {
            continue;
        }
        if found.is_some() {
            return Err(io::Error::other("duplicate procfs mount ID"));
        }
        let separators: Vec<_> = fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| (*field == "-").then_some(index))
            .collect();
        if separators.len() != 1
            || separators[0] < 6
            || separators[0] + 4 != fields.len()
            || fields[3] != "/"
            || !fields[4].starts_with('/')
            || fields[separators[0] + 1] != "proc"
        {
            return Err(io::Error::other("procfs mount is not a complete root view"));
        }
        let parent = fields[1].parse::<u64>().map_err(invalid_data)?;
        let (major, minor) = fields[2]
            .split_once(':')
            .ok_or_else(|| io::Error::other("malformed procfs mount device"))?;
        if parent == 0 || major.parse::<u64>().is_err() || minor.parse::<u64>().is_err() {
            return Err(io::Error::other("malformed procfs mount identity"));
        }
        let mut hidepid_seen = false;
        for option in fields[5]
            .split(',')
            .chain(fields[separators[0] + 3].split(','))
        {
            if option.is_empty() {
                return Err(io::Error::other("malformed procfs mount option"));
            }
            if option.starts_with("hidepid") {
                if hidepid_seen || !matches!(option, "hidepid=0" | "hidepid=off") {
                    return Err(io::Error::other(
                        "procfs hidepid configuration cannot prove visibility",
                    ));
                }
                hidepid_seen = true;
            }
        }
        found = Some(line.to_owned());
    }
    found.ok_or_else(|| io::Error::other("procfs mount ID missing from mountinfo"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNT: &str = "31 22 0:25 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw";

    #[test]
    fn validates_unrestricted_procfs_mounts() {
        for suffix in ["rw", "rw,hidepid=0", "rw,hidepid=off"] {
            let line = format!("31 22 0:25 / /proc rw - proc proc {suffix}");
            assert_eq!(validate_mountinfo(&line, 31).unwrap(), line);
        }
        assert_eq!(
            parse_fdinfo_mount_id("pos:\t0\nmnt_id:\t31\nino:\t1\n").unwrap(),
            31
        );
    }

    #[test]
    fn rejects_hidden_partial_and_ambiguous_proc_views() {
        for line in [
            "31 22 0:25 / /proc rw - proc proc rw,hidepid=2",
            "31 22 0:25 / /proc rw - proc proc rw,hidepid=4",
            "31 22 0:25 / /proc rw - proc proc rw,hidepid",
            "31 22 0:25 / /proc rw - proc proc rw,hidepid=0,hidepid=0",
            "31 22 0:25 /123 /proc rw - proc proc rw",
            "31 22 0:25 / /proc rw - tmpfs tmpfs rw",
            "31 22 0:25 / /proc rw - proc proc",
            "31 wrong 0:25 / /proc rw - proc proc rw",
            "31 22 broken / /proc rw - proc proc rw",
            "31 22 0:25 / relative rw - proc proc rw",
        ] {
            assert!(validate_mountinfo(line, 31).is_err(), "{line}");
        }
        assert!(validate_mountinfo(&format!("{MOUNT}\n{MOUNT}"), 31).is_err());
        assert!(validate_mountinfo(MOUNT, 32).is_err());
        for fdinfo in [
            "mnt_id:\t0\n",
            "mnt_id: x\n",
            "mnt_id: 31\nmnt_id: 31\n",
            "pos: 0\n",
        ] {
            assert!(parse_fdinfo_mount_id(fdinfo).is_err(), "{fdinfo}");
        }
    }

    #[test]
    #[ignore = "requires unrestricted procfs and openat2 support"]
    fn local_proc_view_is_consistent_when_available() {
        let view = ProcView::open().expect("current procfs mount is accessible and unrestricted");
        let pid = std::process::id();
        assert!(view.self_stat().unwrap().starts_with(&format!("{pid} ")));
        assert!(view.self_status().unwrap().contains("NStgid:"));
        assert!(view.process_ids().unwrap().contains(&pid));
        assert!(view.process_ids().is_err());
        assert!(
            view.process_stat(pid)
                .unwrap()
                .starts_with(&format!("{pid} "))
        );
        view.finish().unwrap();
    }
}
