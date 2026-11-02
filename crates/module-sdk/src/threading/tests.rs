use super::*;
use std::{
    cell::RefCell,
    panic::{AssertUnwindSafe, catch_unwind},
};

#[derive(Default)]
struct Scheduler {
    priority: Cell<i32>,
    requests: RefCell<Vec<ThreadPriority>>,
}
impl ThreadScheduler for &Scheduler {
    type SavedPriority = i32;
    fn enter(&self, priority: ThreadPriority) -> io::Result<i32> {
        self.requests.borrow_mut().push(priority);
        Ok(self.priority.replace(priority.native()))
    }
    fn restore(&self, previous: i32) -> io::Result<()> {
        self.priority.set(previous);
        Ok(())
    }
}
#[derive(Default)]
struct Replacement {
    active: Cell<Option<ThreadPriority>>,
}
impl ThreadScheduler for &Replacement {
    type SavedPriority = Option<ThreadPriority>;
    fn enter(&self, priority: ThreadPriority) -> io::Result<Self::SavedPriority> {
        Ok(self.active.replace(Some(priority)))
    }
    fn restore(&self, previous: Self::SavedPriority) -> io::Result<()> {
        self.active.set(previous);
        Ok(())
    }
}
fn stream_step<S: ThreadScheduler>(scheduler: S, work: impl FnOnce()) {
    let _priority = ScopedThreadPriority::with_scheduler(scheduler, ThreadPriority::High).unwrap();
    work();
}
#[test]
fn unchanged_consumer_accepts_a_replacement_scheduler_and_restores_on_unwind() {
    let scheduler = Scheduler::default();
    stream_step(&scheduler, || {
        assert_eq!(scheduler.priority.get(), ThreadPriority::High.native())
    });
    assert_eq!(scheduler.priority.get(), 0);
    let replacement = Replacement::default();
    let failed = catch_unwind(AssertUnwindSafe(|| {
        stream_step(&replacement, || {
            assert_eq!(replacement.active.get(), Some(ThreadPriority::High));
            let _control =
                ScopedThreadPriority::with_scheduler(&replacement, ThreadPriority::Critical)
                    .unwrap();
            assert_eq!(replacement.active.get(), Some(ThreadPriority::Critical));
            panic!("a provider failed");
        })
    }));
    assert!(failed.is_err());
    assert_eq!(replacement.active.get(), None);
    assert_eq!(scheduler.priority.get(), 0);
    assert_eq!(*scheduler.requests.borrow(), [ThreadPriority::High]);
}
#[test]
fn permission_failure_does_not_run_restore_for_an_unacquired_priority() {
    struct Denied;
    impl ThreadScheduler for Denied {
        type SavedPriority = ();
        fn enter(&self, _: ThreadPriority) -> io::Result<()> {
            Err(io::ErrorKind::PermissionDenied.into())
        }
        fn restore(&self, _: ()) -> io::Result<()> {
            panic!("denied priority acquired no saved state")
        }
    }
    assert!(ScopedThreadPriority::with_scheduler(Denied, ThreadPriority::Critical).is_err());
}
#[test]
fn priorities_match_the_reference_operating_system_levels() {
    assert_eq!(
        ThreadPriority::High.native(),
        if cfg!(windows) { 1 } else { -10 }
    );
    assert_eq!(
        ThreadPriority::Critical.native(),
        if cfg!(windows) { 2 } else { -15 }
    );
}
