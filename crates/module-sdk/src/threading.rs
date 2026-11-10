//! Per-thread streaming priorities. Linux uses ordinary nice levels, never a
//! real-time scheduling class. Windows uses the corresponding Win32 levels.
//! Scheduling is best effort: permission failures are reported, not mistaken
//! for a successfully raised priority. On Linux a request the installation does
//! not permit falls back to the strongest level RLIMIT_NICE allows, and that
//! reduced level is reported once.
use std::{cell::Cell, io, marker::PhantomData, rc::Rc};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadPriority {
    High,
    Critical,
}

impl ThreadPriority {
    fn native(self) -> i32 {
        match self {
            Self::High => {
                if cfg!(windows) {
                    1
                } else {
                    -10
                }
            }
            Self::Critical => {
                if cfg!(windows) {
                    2
                } else {
                    -15
                }
            }
        }
    }
}

/// An explicitly selected scheduling adapter. Saved priority is owned by the
/// adapter and never interpreted by a consumer. Calls act on the calling thread.
pub trait ThreadScheduler {
    type SavedPriority;
    fn enter(&self, priority: ThreadPriority) -> io::Result<Self::SavedPriority>;
    fn restore(&self, saved: Self::SavedPriority) -> io::Result<()>;
}

pub struct PlatformThreadScheduler;
pub struct SavedPriority(i32);

impl ThreadScheduler for PlatformThreadScheduler {
    type SavedPriority = SavedPriority;
    fn enter(&self, priority: ThreadPriority) -> io::Result<SavedPriority> {
        let previous = platform::current()?;
        let desired = priority.native();
        if previous != desired {
            let applied = platform::raise_toward(desired)?;
            if applied != desired {
                report_limited_once(priority, applied);
            }
        }
        Ok(SavedPriority(previous))
    }
    fn restore(&self, saved: SavedPriority) -> io::Result<()> {
        if platform::current()? != saved.0 {
            platform::set(saved.0)?;
        }
        Ok(())
    }
}

/// Restores the calling thread's priority on return or unwind. This guard cannot
/// move between threads. Use it around multiplexed module work so later game
/// launches, helpers and unrelated modules retain their own scheduling policy.
pub struct ScopedThreadPriority<S: ThreadScheduler = PlatformThreadScheduler> {
    scheduler: S,
    saved: Option<S::SavedPriority>,
    _same_thread: PhantomData<Rc<()>>,
}
impl<S: ThreadScheduler> ScopedThreadPriority<S> {
    pub fn with_scheduler(scheduler: S, priority: ThreadPriority) -> io::Result<Self> {
        let saved = scheduler.enter(priority)?;
        Ok(Self {
            scheduler,
            saved: Some(saved),
            _same_thread: PhantomData,
        })
    }
}
impl ScopedThreadPriority {
    pub fn enter(priority: ThreadPriority) -> io::Result<Self> {
        Self::with_scheduler(PlatformThreadScheduler, priority)
    }
    pub fn best_effort(priority: ThreadPriority) -> Option<Self> {
        match Self::enter(priority) {
            Ok(guard) => Some(guard),
            Err(error) => {
                report_once(priority, &error);
                None
            }
        }
    }
}
impl<S: ThreadScheduler> Drop for ScopedThreadPriority<S> {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take()
            && let Err(error) = self.scheduler.restore(saved)
        {
            // Continuing could launch a game or run unrelated work at an
            // unintended priority. Keep restoration failure explicit and fatal.
            eprintln!("thread priority restoration failed: {error}");
            std::process::abort();
        }
    }
}

/// Set a dedicated worker's priority before opening its backend. OS permissions
/// remain controlled by installation; this function grants no capability.
pub fn set_current_thread_priority(priority: ThreadPriority) -> io::Result<()> {
    let applied = platform::raise_toward(priority.native())?;
    if applied != priority.native() {
        report_limited_once(priority, applied);
    }
    Ok(())
}
pub fn configure_worker_priority(priority: ThreadPriority) {
    if let Err(error) = set_current_thread_priority(priority) {
        report_once(priority, &error);
    }
}
fn report_once(priority: ThreadPriority, error: &io::Error) {
    if first_report(priority, 0) {
        eprintln!(
            "streaming thread {:?} priority {priority:?} unavailable: {error}",
            std::thread::current().name()
        );
    }
}
fn report_limited_once(priority: ThreadPriority, applied: i32) {
    if first_report(priority, 2) {
        eprintln!(
            "streaming thread {:?} priority {priority:?} limited to level {applied} by the installation's scheduling limit",
            std::thread::current().name()
        );
    }
}
/// Each priority/outcome pair is reported once per thread.
fn first_report(priority: ThreadPriority, outcome_shift: u8) -> bool {
    thread_local! { static REPORTED: Cell<u8> = const { Cell::new(0) }; }
    let bit = match priority {
        ThreadPriority::High => 1,
        ThreadPriority::Critical => 2,
    } << outcome_shift;
    REPORTED.with(|reported| {
        let first = reported.get() & bit == 0;
        reported.set(reported.get() | bit);
        first
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use std::io;
    pub fn current() -> io::Result<i32> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn set(_: i32) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub fn raise_toward(_: i32) -> io::Result<i32> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(test)]
mod tests;
