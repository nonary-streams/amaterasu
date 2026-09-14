use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus};

pub(crate) struct ContainedChild {
    child: Child,
    group_finished: bool,
}

impl ContainedChild {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
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
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn signal(&mut self, signal: i32) -> io::Result<()> {
        // Validate ownership before every group signal, including retries. ECHILD
        // stops signaling if an embedding host has violated exclusive wait ownership.
        self.exited()?;
        // SAFETY: our unreaped leader reserves this positive PID. A negative PID
        // addresses only its group. No signal is sent after reaping the leader.
        if unsafe { libc::kill(-(self.id() as i32), signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();

        #[cfg(target_os = "macos")]
        if error.raw_os_error() == Some(libc::EPERM)
            && self.exited()?
            && macos_group_is_dead(self.id())?
        {
            return Ok(());
        }
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub fn request_stop(&mut self) -> io::Result<()> {
        self.signal(libc::SIGTERM)
    }

    pub fn force_stop(&mut self) -> io::Result<()> {
        if !self.group_finished {
            self.signal(libc::SIGKILL)?;
            self.group_finished = true;
        }
        Ok(())
    }

    pub fn try_reap(&mut self) -> io::Result<Option<ExitStatus>> {
        // Only final group signaling permits releasing the PID reservation.
        if !self.group_finished {
            return Ok(None);
        }
        self.child.try_wait()
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
        return Ok(false);
    }
    if size as usize >= members.len() {
        return Ok(false);
    }
    for &pid in &members[..size as usize] {
        if pid == 0 || pid == group as i32 {
            continue;
        }
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
            continue;
        }
        if read as usize != std::mem::size_of_val(&info) || info.pbi_status != libc::SZOMB {
            return Ok(false);
        }
    }
    Ok(true)
}
