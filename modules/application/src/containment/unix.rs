use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

pub(crate) struct ContainedChild {
    child: Child,
    group_finished: bool,
}

impl ContainedChild {
    pub fn spawn(command: &mut Command, _elevated: bool) -> io::Result<Self> {
        // std sets the group before exec; no allocating pre_exec callback is needed.
        let child = command.process_group(0).spawn()?;
        Ok(Self {
            child,
            group_finished: false,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn exited(&mut self) -> io::Result<bool> {
        Ok(self.successful_exit()?.is_some())
    }

    pub fn successful_exit(&mut self) -> io::Result<Option<bool>> {
        // SAFETY: info is initialized writable storage. WNOWAIT deliberately keeps
        // our exclusive child unreaped, pinning its PID/PGID until final signaling.
        // The embedding host must not install SIGCHLD=SIG_IGN, SA_NOCLDWAIT, or reap
        // this module's children independently.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful waitid initialized siginfo_t with child status or zero.
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        // SAFETY: si_code and si_status are initialized for the reported child.
        Ok(Some(
            siginfo_code(&info) == libc::CLD_EXITED && siginfo_status(&info) == 0,
        ))
    }

    fn signal(&mut self, signal: i32) -> io::Result<bool> {
        // Validate ownership before every group signal, including retries. ECHILD
        // stops signaling if an embedding host has violated exclusive wait ownership.
        self.exited()?;
        // SAFETY: our unreaped leader reserves this positive PID. A negative PID
        // addresses only its group. No signal is sent after reaping the leader.
        if unsafe { libc::kill(-(self.id() as i32), signal) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();

        #[cfg(target_os = "macos")]
        if error.raw_os_error() == Some(libc::EPERM)
            && self.exited()?
            && macos_group_is_dead(self.id())?
        {
            return Ok(false);
        }
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    pub fn request_stop(&mut self) -> io::Result<bool> {
        self.signal(libc::SIGTERM)
    }

    pub fn force_stop(&mut self) -> io::Result<()> {
        if !self.group_finished {
            let _ = self.signal(libc::SIGKILL)?;
            self.group_finished = true;
        }
        Ok(())
    }

    pub fn group_running(&self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            linux_group_has_live_members(self.id())
        }
        #[cfg(target_os = "macos")]
        {
            return Ok(!macos_group_is_dead(self.id())?);
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // Other Unix targets retain conservative process-group probing.
            // The pinned product's supported Linux and macOS paths use member
            // enumeration so the deliberately unreaped leader is not mistaken
            // for a live descendant.
            let result = unsafe { libc::kill(-(self.id() as i32), 0) };
            if result == 0 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    pub fn try_reap(&mut self) -> io::Result<Option<ExitStatus>> {
        // Only final group signaling permits releasing the PID reservation.
        if !self.group_finished {
            return Ok(None);
        }
        self.child.try_wait()
    }
}

pub(crate) struct DetachedChild {
    child: Child,
}

impl DetachedChild {
    pub fn spawn(command: &mut Command, _elevated: bool) -> io::Result<Self> {
        Ok(Self {
            child: command.spawn()?,
        })
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

#[cfg(target_os = "linux")]
fn linux_group_has_live_members(group_id: u32) -> io::Result<bool> {
    const MAX_PROCESS_SCAN: usize = 131_072;

    let processes = std::fs::read_dir("/proc")?;
    for (index, entry) in processes.enumerate() {
        if index >= MAX_PROCESS_SCAN {
            return Err(io::Error::other("procfs process scan exceeded its bound"));
        }
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            // An unreadable process could be a live group member. Keep the
            // stream alive until the configured timeout forces group cleanup.
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(true),
            Err(error) => return Err(error),
        };
        if linux_stat_is_live_group_member(&stat, group_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn linux_stat_is_live_group_member(stat: &str, group_id: u32) -> bool {
    let Some(command_end) = stat.rfind(')') else {
        return false;
    };
    let mut fields = stat[command_end + 1..].split_whitespace();
    let Some(state) = fields.next() else {
        return false;
    };
    let _parent_process = fields.next();
    let Some(process_group) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
        return false;
    };
    process_group == group_id && !matches!(state, "Z" | "X")
}

#[cfg(target_os = "macos")]
fn macos_group_is_dead(group: u32) -> io::Result<bool> {
    // Darwin killpg returns EPERM when a group contains only zombies. Never
    // suppress a real permission failure: inspect every member before accepting
    // this case. The unreaped leader still reserves the group identity.
    let mut members = [0i32; 4096];
    let capacity = std::mem::size_of_val(&members) as i32;
    // SAFETY: members is writable aligned storage of exactly capacity bytes.
    let size =
        unsafe { libc::proc_listpgrppids(group as i32, members.as_mut_ptr().cast(), capacity) };
    if size <= 0 {
        // The libproc wrapper converts syscall errors to zero, so an empty or
        // failed scan cannot prove that the process group is dead.
        return Ok(false);
    }
    if size as usize >= members.len() {
        return Ok(false);
    }

    Ok(super::macos_members_are_dead(
        members[..size as usize].iter().copied(),
        |pid| {
            // SAFETY: zero is valid initialization and proc_pidinfo receives the
            // matching buffer size/flavor. This is inspection only, never PID signaling.
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let read = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    std::mem::size_of_val(&info) as i32,
                )
            };
            if read == 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return Some(true);
            }
            (read as usize == std::mem::size_of_val(&info))
                .then_some(info.pbi_status == libc::SZOMB)
        },
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn siginfo_code(info: &libc::siginfo_t) -> i32 {
    info.si_code
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn siginfo_code(info: &libc::siginfo_t) -> i32 {
    // SAFETY: callers only inspect siginfo_t after waitid initialized its result.
    unsafe { info.si_code() }
}

#[cfg(target_os = "macos")]
fn siginfo_status(info: &libc::siginfo_t) -> i32 {
    info.si_status
}

#[cfg(not(target_os = "macos"))]
fn siginfo_status(info: &libc::siginfo_t) -> i32 {
    // SAFETY: callers only inspect siginfo_t after waitid initialized its result.
    unsafe { info.si_status() }
}
