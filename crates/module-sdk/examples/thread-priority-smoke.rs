//! Opt-in installed-host check: run as the normal user with permitted
//! CAP_SYS_NICE on this executable. No root process or real-time policy is used.
#[cfg(target_os = "linux")]
fn main() {
    use amaterasu_module_sdk::threading::{
        ScopedThreadPriority, ThreadPriority, set_current_thread_priority,
    };
    fn nice() -> i32 {
        // SAFETY: read-only query of this thread's ordinary nice level.
        unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) }
    }
    fn capabilities() -> String {
        // SAFETY: gettid is a read-only current-thread identity query.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        let status = std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).unwrap();
        status
            .lines()
            .filter(|line| line.starts_with("Cap"))
            .collect::<Vec<_>>()
            .join("\n")
    }
    let original = nice();
    let before = capabilities();
    {
        let _video = ScopedThreadPriority::enter(ThreadPriority::High).unwrap();
        assert_eq!(nice(), -10);
        {
            let _control = ScopedThreadPriority::enter(ThreadPriority::Critical).unwrap();
            assert_eq!(nice(), -15);
            let child = std::process::Command::new("/usr/bin/python3")
                .args(["-c", "import os; print(os.getpriority(os.PRIO_PROCESS, 0))"])
                .output()
                .unwrap();
            assert!(child.status.success());
            assert_eq!(String::from_utf8(child.stdout).unwrap().trim(), "-15");
        }
        assert_eq!(nice(), -10);
        assert_eq!(capabilities(), before);
    }
    assert_eq!(nice(), original);
    assert_eq!(capabilities(), before);
    let workers = [ThreadPriority::High, ThreadPriority::Critical].map(|priority| {
        std::thread::spawn(move || {
            let before = capabilities();
            set_current_thread_priority(priority).unwrap();
            assert_eq!(capabilities(), before);
            nice()
        })
    });
    assert_eq!(workers.map(|worker| worker.join().unwrap()), [-10, -15]);
    assert_eq!(nice(), original);
    assert_eq!(capabilities(), before);
    println!(
        "verified: encoder=-10 capture=-15 scoped control=-15; caller priority and capability sets restored"
    );
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This installed-host smoke check requires Linux");
}
