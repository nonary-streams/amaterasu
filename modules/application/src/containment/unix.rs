use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

pub(crate) struct ContainedChild {
    child: Child,
    force_requested: bool,
}

impl ContainedChild {
    pub fn spawn(command: &mut Command, _elevated: bool) -> Result<Self, super::SpawnError> {
        // std sets the group before exec; no allocating pre_exec callback is needed.
        let child = command.process_group(0).spawn()?;
        Ok(Self {
            child,
            force_requested: false,
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
        if !self.force_requested {
            let _ = self.signal(libc::SIGKILL)?;
            self.force_requested = true;
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.try_reap_with_group_observation(Self::group_running)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            // kill(PGID, 0) cannot distinguish our reserved zombie leader from
            // live descendants. Reaping it would discard the only PGID reuse
            // protection before native tree retirement could be established.
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "confirmed process-group retirement requires Linux or macOS member inspection",
            ))
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", test))]
    fn try_reap_with_group_observation(
        &mut self,
        group_running: impl FnOnce(&Self) -> io::Result<bool>,
    ) -> io::Result<Option<ExitStatus>> {
        // Signal acceptance does not prove that every group member has exited.
        // Keep the leader wait-owned until a complete native group inspection
        // confirms retirement, so neither cleanup nor PGID reuse can race it.
        if !self.force_requested {
            return Ok(None);
        }
        // WNOWAIT validates exclusive ownership before inspecting the numeric
        // group. ECHILD must not fall back to Child's cached root exit status.
        if !self.exited()? || group_running(self)? {
            return Ok(None);
        }
        self.child.try_wait()
    }
}

pub(crate) struct DetachedChild {
    child: Child,
}

impl DetachedChild {
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn output_redirection_supported(_elevated: bool) -> io::Result<bool> {
        Ok(true)
    }

    pub fn spawn(command: &mut Command, _elevated: bool) -> io::Result<Self> {
        Ok(Self {
            child: command.spawn()?,
        })
    }

    pub fn spawn_with_output(
        command: &mut Command,
        elevated: bool,
        stdout: Option<&str>,
        stderr: Option<&str>,
    ) -> io::Result<Self> {
        if let Some(path) = stdout {
            command.stdout(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            );
        }
        if let Some(path) = stderr {
            command.stderr(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?,
            );
        }
        Self::spawn(command, elevated)
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

#[cfg(target_os = "linux")]
fn linux_group_has_live_members(group_id: u32) -> io::Result<bool> {
    let self_stat = std::fs::read_to_string("/proc/self/stat")?;
    let self_status = std::fs::read_to_string("/proc/self/status")?;
    let processes = std::fs::read_dir("/proc")?.map(|entry| {
        let entry = entry?;
        let Some(process_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            return Ok(None);
        };
        linux_process_stat_from_read(
            process_id,
            std::fs::read_to_string(entry.path().join("stat")),
        )
    });
    linux_group_has_live_members_from_stats(
        group_id,
        std::process::id(),
        &self_stat,
        &self_status,
        processes,
    )
}

#[cfg(target_os = "linux")]
fn linux_process_stat_from_read(
    process_id: u32,
    result: io::Result<String>,
) -> io::Result<Option<(u32, String)>> {
    match result {
        Ok(stat) => Ok(Some((process_id, stat))),
        // procfs may report ESRCH if the task exits after the stat file was
        // opened but before it was read. It is as absent as an ENOENT entry;
        // the scan still requires the wait-owned group leader to be present.
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            Ok(None)
        }
        // An unreadable process could be a live group member. Keep native
        // ownership until a complete observation can confirm retirement.
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn linux_group_has_live_members_from_stats(
    group_id: u32,
    current_process_id: u32,
    self_stat: &str,
    self_status: &str,
    processes: impl IntoIterator<Item = io::Result<Option<(u32, String)>>>,
) -> io::Result<bool> {
    const MAX_PROCESS_SCAN: usize = 131_072;

    linux_validate_proc_pid_view(current_process_id, self_stat, self_status)?;
    let mut leader_seen = false;
    for (index, entry) in processes.into_iter().enumerate() {
        if index >= MAX_PROCESS_SCAN {
            return Err(io::Error::other("procfs process scan exceeded its bound"));
        }
        let Some((process_id, stat)) = entry? else {
            continue;
        };
        let process = LinuxProcessStat::parse(&stat)?;
        if process.process_id != process_id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "procfs stat does not match its process directory",
            ));
        }
        if process_id == group_id {
            if process.process_group != group_id {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "procfs leader does not match the owned process group",
                ));
            }
            leader_seen = true;
        }
        if process.is_live_group_member(group_id) {
            return Ok(true);
        }
    }
    // WNOWAIT still owns the unreaped group leader. A scan that omits it is
    // partial or uses another PID view and cannot establish group absence.
    if leader_seen {
        Ok(false)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "procfs scan omitted the owned process-group leader",
        ))
    }
}

#[cfg(target_os = "linux")]
fn linux_validate_proc_pid_view(
    current_process_id: u32,
    self_stat: &str,
    self_status: &str,
) -> io::Result<()> {
    let invalid_view = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "procfs PID namespace does not match the current process",
        )
    };
    if LinuxProcessStat::parse(self_stat)?.process_id != current_process_id {
        return Err(invalid_view());
    }
    // NStgid starts with the PID namespace owning the procfs mount and lists
    // inner namespace identities afterward. Numeric coincidences across two
    // namespaces do not establish that group IDs use our signal/wait view.
    let mut namespace_fields = self_status
        .lines()
        .filter_map(|line| line.strip_prefix("NStgid:"));
    let mut namespace_ids = namespace_fields
        .next()
        .ok_or_else(invalid_view)?
        .split_whitespace();
    let namespace_pid = namespace_ids
        .next()
        .and_then(|pid| pid.parse::<u32>().ok())
        .ok_or_else(invalid_view)?;
    if namespace_fields.next().is_some()
        || namespace_ids.next().is_some()
        || namespace_pid != current_process_id
    {
        return Err(invalid_view());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxProcessStat {
    process_id: u32,
    process_group: u32,
    live: bool,
}

#[cfg(target_os = "linux")]
impl LinuxProcessStat {
    fn parse(stat: &str) -> io::Result<Self> {
        let malformed = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "malformed procfs process identity",
            )
        };
        let command_start = stat.find('(').ok_or_else(malformed)?;
        let command_end = stat
            .rfind(')')
            .filter(|end| *end > command_start)
            .ok_or_else(malformed)?;
        let process_id = stat[..command_start]
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|process_id| *process_id != 0)
            .ok_or_else(malformed)?;
        let mut fields = stat[command_end + 1..].split_whitespace();
        let state = fields.next().ok_or_else(malformed)?;
        let _parent_process = fields
            .next()
            .and_then(|field| field.parse::<u32>().ok())
            .ok_or_else(malformed)?;
        let process_group = fields
            .next()
            .and_then(|field| field.parse::<u32>().ok())
            .ok_or_else(malformed)?;
        Ok(Self {
            process_id,
            process_group,
            live: !matches!(state, "Z" | "X" | "x"),
        })
    }

    fn is_live_group_member(&self, group_id: u32) -> bool {
        self.process_group == group_id && self.live
    }
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

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
