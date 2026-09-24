use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::*;

pub(crate) struct ContainedChild {
    child: Child,
    job: OwnedHandle,
    group_finished: bool,
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: caller passes a newly created, exclusively owned Win32 handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

impl ContainedChild {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        // SAFETY: null security attributes/name create an unnamed, non-inheritable
        // job. OwnedHandle closes the sole handle even on error or host termination.
        let job = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the handle is live and the buffer matches the information class.
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // No application code can run before job assignment. Preserve std's native
        // argument quoting, environment, cwd, and null stdio setup.
        let mut child = command.creation_flags(CREATE_SUSPENDED).spawn()?;
        // SAFETY: both handles remain owned throughout assignment.
        let assigned =
            unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
        let setup = if assigned == 0 {
            Err(io::Error::last_os_error())
        } else {
            resume_primary_thread(&child)
        };
        if let Err(error) = setup {
            // Assignment/resume errors fail closed: never run an uncontained child.
            let _ = child.kill();
            // Do not block forever on an OS termination failure.
            let _ = child.try_wait();
            return Err(error);
        }
        Ok(Self {
            child,
            job,
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
        Ok(self.child.try_wait()?.map(|status| status.success()))
    }
    // Windows has no universal graceful termination primitive for arbitrary GUI
    // and console programs. Retain a grace interval, then terminate the owned job.
    pub fn request_stop(&mut self) -> io::Result<()> {
        Ok(())
    }
    pub fn force_stop(&mut self) -> io::Result<()> {
        if !self.group_finished {
            // SAFETY: use the owned job, never a recycled numeric process ID.
            if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            self.group_finished = true;
        }
        Ok(())
    }
    pub fn try_reap(&mut self) -> io::Result<Option<ExitStatus>> {
        if !self.group_finished {
            return Ok(None);
        }
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: this owned job remains live; the writable structure and size
        // match the information class. TerminateJobObject is asynchronous, so
        // retain ownership until every job process has stopped, not only the root.
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if accounting.ActiveProcesses != 0 {
            return Ok(None);
        }
        self.child.try_wait()
    }
}

fn resume_primary_thread(child: &Child) -> io::Result<()> {
    // The suspended process cannot create additional threads itself. Toolhelp
    // supplies the primary thread because std intentionally closes that handle.
    // SAFETY: all pointers refer to initialized, live stack structures; every
    // returned handle is exclusively owned and closed by OwnedHandle.
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut available = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while available != 0 {
        if entry.th32OwnerProcessID == child.id() {
            let thread = owned(unsafe {
                OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                )
            })?;
            // Revalidate the handle against the still-owned process, avoiding a
            // stale snapshot targeting a thread belonging to another process.
            if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != child.id() {
                return Err(io::Error::other("thread ownership changed"));
            }
            let previous_count = unsafe { ResumeThread(thread.as_raw_handle()) };
            if previous_count == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            if previous_count != 1 {
                return Err(io::Error::other("unexpected primary thread suspend count"));
            }
            return Ok(());
        }
        available = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Err(io::Error::other("suspended primary thread not found"))
}
