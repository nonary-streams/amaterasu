use std::collections::BTreeSet;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};
#[path = "windows_user_process.rs"]
mod windows_user_process;
use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, ERROR_TIMEOUT, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LPARAM,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, SendNotifyMessageW, WM_CLOSE,
};
use windows_user_process::{
    CreatedUserProcess, NativeProcess, active_user_token_for_system_host,
    create_suspended_user_process, create_uncontained_user_process, resume_user_process,
};

pub(crate) struct ContainedChild {
    child: SuspendedProcess,
    job: OwnedHandle,
    group_finished: bool,
}

enum ProcessHandle {
    Standard(Child),
    User(NativeProcess),
}

impl ProcessHandle {
    fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
            Self::User(child) => child.id(),
        }
    }

    fn raw_handle(&self) -> HANDLE {
        match self {
            Self::Standard(child) => child.as_raw_handle(),
            Self::User(child) => child.raw_handle(),
        }
    }

    fn terminate(&mut self) -> io::Result<()> {
        match self {
            Self::Standard(child) => child.kill(),
            Self::User(child) => child.terminate(),
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self {
            Self::Standard(child) => child.try_wait(),
            Self::User(child) => child.try_wait(),
        }
    }
}

pub(crate) struct DetachedChild {
    process: ProcessHandle,
}

impl DetachedChild {
    pub fn output_redirection_supported(elevated: bool) -> io::Result<bool> {
        Ok(active_user_token_for_system_host(elevated)?.is_none())
    }

    pub fn spawn(command: &mut Command, elevated: bool) -> io::Result<Self> {
        let user_token = active_user_token_for_system_host(elevated)?;
        let process = if let Some(token) = user_token.as_ref() {
            ProcessHandle::User(create_uncontained_user_process(
                command,
                token.as_raw_handle(),
            )?)
        } else {
            ProcessHandle::Standard(command.spawn()?)
        };
        Ok(Self { process })
    }

    pub fn spawn_with_output(
        command: &mut Command,
        elevated: bool,
        stdout: Option<&str>,
        stderr: Option<&str>,
    ) -> io::Result<Self> {
        if stdout.is_none() && stderr.is_none() {
            return Self::spawn(command, elevated);
        }
        let user_token = active_user_token_for_system_host(elevated)?;
        if user_token.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "detached output requires an interactive-user broker on a SYSTEM host",
            ));
        }
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
        let process = ProcessHandle::Standard(command.spawn()?);
        Ok(Self { process })
    }

    pub fn id(&self) -> u32 {
        self.process.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.process.try_wait()
    }
}

struct SuspendedProcess {
    process: ProcessHandle,
    user_primary_thread: Option<OwnedHandle>,
}

impl SuspendedProcess {
    fn id(&self) -> u32 {
        self.process.id()
    }

    fn raw_handle(&self) -> HANDLE {
        self.process.raw_handle()
    }

    fn terminate(&mut self) -> io::Result<()> {
        self.process.terminate()
    }

    fn resume(&self) -> io::Result<()> {
        if let Some(thread) = &self.user_primary_thread {
            resume_user_process(thread)
        } else if let ProcessHandle::Standard(child) = &self.process {
            resume_primary_thread(child)
        } else {
            Err(io::Error::other(
                "user process is missing its primary thread",
            ))
        }
    }
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: caller passes a newly created, exclusively owned Win32 handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

impl ContainedChild {
    pub fn spawn(command: &mut Command, elevated: bool) -> io::Result<Self> {
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
        // The active user's process token is used when this module host is SYSTEM.
        // A standard non-admin account retains the token unchanged even when the
        // catalog requested elevation, matching the source's documented warning.
        let user_token = active_user_token_for_system_host(elevated)?;
        // Neither creation path can run app code before private-job assignment.
        let mut child = if let Some(token) = user_token.as_ref() {
            let CreatedUserProcess {
                process,
                primary_thread,
            } = create_suspended_user_process(command, token.as_raw_handle())?;
            SuspendedProcess {
                process: ProcessHandle::User(process),
                user_primary_thread: Some(primary_thread),
            }
        } else {
            let child = command.creation_flags(CREATE_SUSPENDED).spawn()?;
            SuspendedProcess {
                process: ProcessHandle::Standard(child),
                user_primary_thread: None,
            }
        };
        // SAFETY: both handles remain owned throughout assignment.
        #[cfg(test)]
        let fail_assignment = FAIL_NEXT_JOB_ASSIGNMENT.with(|failure| failure.replace(false));
        #[cfg(not(test))]
        let fail_assignment = false;
        let assigned = if fail_assignment {
            0
        } else {
            unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.raw_handle()) }
        };
        let setup = if assigned == 0 {
            Err(if fail_assignment {
                io::Error::other("injected job assignment failure")
            } else {
                io::Error::last_os_error()
            })
        } else {
            child.resume()
        };
        if let Err(error) = setup {
            // Assignment/resume errors fail closed: never run an uncontained child.
            let job_termination = if assigned != 0 {
                // SAFETY: the job owns this child and kill-on-close is enabled.
                if unsafe { TerminateJobObject(job.as_raw_handle(), 1) } == 0 {
                    Some(io::Error::last_os_error())
                } else {
                    None
                }
            } else {
                None
            };
            let process_termination = child.terminate().err();
            // Termination is asynchronous. Retain the process handle through a
            // bounded confirmation attempt; report an unsettled child explicitly.
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut wait_failure = None;
            loop {
                match child.process.try_wait() {
                    Ok(Some(_)) => return Err(error),
                    Ok(None) => {}
                    Err(wait_error) => wait_failure = Some(wait_error),
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            eprintln!(
                "Application launch could not confirm child exit after {error} (job termination: {job_termination:?}, process termination: {process_termination:?}, last wait error: {wait_failure:?})"
            );
            // The manager retains raw OS errors in launch_failed records. Use
            // ERROR_TIMEOUT so the caller can distinguish unsettled ownership
            // from the original assignment or resume error.
            return Err(io::Error::from_raw_os_error(ERROR_TIMEOUT as i32));
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
        Ok(self
            .child
            .process
            .try_wait()?
            .map(|status| status.success()))
    }
    pub fn group_running(&self) -> io::Result<bool> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the owned job remains live and the writable buffer and size
        // match JobObjectBasicAccountingInformation.
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
        Ok(accounting.ActiveProcesses != 0)
    }

    // Windows has no universal graceful-stop primitive for console processes.
    // Ask every top-level window in the job to close, matching the sibling's
    // GUI shutdown behavior; callers force the owned job if none accepts it.
    pub fn request_stop(&mut self) -> io::Result<bool> {
        // A failed job query cannot establish that there are no windows. Keep
        // the job owned and let the caller retry instead of forcing it to stop.
        let process_ids = process_ids_in_job(self.job.as_raw_handle())?;
        if process_ids.is_empty() {
            return Ok(false);
        }
        let mut context = CloseWindowsContext {
            process_ids: process_ids.into_iter().collect(),
            requested_exit: false,
        };
        // SAFETY: EnumWindows calls the callback synchronously while `context`
        // remains alive and exclusively borrowed for the duration of the call.
        unsafe {
            EnumWindows(
                Some(request_group_window),
                (&mut context as *mut CloseWindowsContext) as LPARAM,
            );
        }
        Ok(context.requested_exit)
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
        self.child.process.try_wait()
    }
}

const MAX_JOB_PROCESS_IDS: usize = 65_536;

fn process_ids_in_job(job: HANDLE) -> io::Result<Vec<u32>> {
    #[cfg(test)]
    if FAIL_NEXT_JOB_PROCESS_QUERY.with(|failure| failure.replace(false)) {
        return Err(io::Error::other("injected job process query failure"));
    }
    let header_size = std::mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>();
    let max_bytes = header_size + (MAX_JOB_PROCESS_IDS - 1) * std::mem::size_of::<usize>();
    let mut storage = vec![0usize; header_size.div_ceil(std::mem::size_of::<usize>())];

    loop {
        let buffer_size = storage.len() * std::mem::size_of::<usize>();
        let mut required_size = 0u32;
        // SAFETY: Vec<usize> provides aligned writable storage whose length is
        // passed exactly; the job handle is privately owned by this child.
        let succeeded = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                storage.as_mut_ptr().cast(),
                buffer_size as u32,
                &mut required_size,
            )
        };
        if succeeded != 0 {
            // SAFETY: a successful query initialized the two counts and each
            // process ID within the supplied, aligned buffer.
            let list = unsafe { &*(storage.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
            let capacity = (buffer_size
                - std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList))
                / std::mem::size_of::<usize>();
            let count = list.NumberOfProcessIdsInList as usize;
            if count > capacity {
                return Err(io::Error::other("job returned an invalid process count"));
            }
            // SAFETY: `count` was checked against the process-ID capacity above.
            let process_ids =
                unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count) };
            return process_ids
                .iter()
                .map(|process_id| {
                    u32::try_from(*process_id)
                        .map_err(|_| io::Error::other("job returned an invalid process ID"))
                })
                .collect();
        }

        // SAFETY: GetLastError reads the thread-local error from the failed
        // QueryInformationJobObject call above.
        if unsafe { GetLastError() } != ERROR_MORE_DATA {
            return Err(io::Error::last_os_error());
        }
        let required_size = (required_size as usize).max(buffer_size.saturating_mul(2));
        if required_size > max_bytes {
            return Err(io::Error::other("job process list exceeds its bound"));
        }
        storage.resize(required_size.div_ceil(std::mem::size_of::<usize>()), 0);
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_JOB_PROCESS_QUERY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_JOB_ASSIGNMENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn fail_next_job_process_query() {
    FAIL_NEXT_JOB_PROCESS_QUERY.with(|failure| failure.set(true));
}

#[cfg(test)]
pub(crate) fn fail_next_job_assignment() {
    FAIL_NEXT_JOB_ASSIGNMENT.with(|failure| failure.set(true));
}

struct CloseWindowsContext {
    process_ids: BTreeSet<u32>,
    requested_exit: bool,
}

unsafe extern "system" fn request_group_window(
    window: windows_sys::Win32::Foundation::HWND,
    parameter: LPARAM,
) -> windows_sys::core::BOOL {
    if parameter == 0 {
        return 1;
    }
    // SAFETY: `parameter` points to the live context borrowed by EnumWindows.
    let context = unsafe { &mut *(parameter as *mut CloseWindowsContext) };
    let mut process_id = 0u32;
    // SAFETY: the HWND was supplied by EnumWindows and `process_id` is writable.
    if unsafe { GetWindowThreadProcessId(window, &mut process_id) } == 0
        || !context.process_ids.contains(&process_id)
    {
        return 1;
    }
    // SAFETY: the window handle is valid for the duration of this enumeration;
    // SendNotifyMessageW posts WM_CLOSE asynchronously to the owning UI thread.
    if unsafe { SendNotifyMessageW(window, WM_CLOSE, 0, 0) } != 0 {
        context.requested_exit = true;
    }
    1
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
