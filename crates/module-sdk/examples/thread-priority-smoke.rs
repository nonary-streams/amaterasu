//! Opt-in installed-host check: run as the normal user. With permitted
//! CAP_SYS_NICE the reference levels are expected; otherwise each level is
//! limited by the process RLIMIT_NICE. No root process or real-time policy is used.
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
    fn expected(reference: i32) -> i32 {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit writes one rlimit through this valid pointer.
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NICE, &mut limit) }, 0);
        let permitted = std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("CapPrm:"))
            .map(|mask| u64::from_str_radix(mask.trim(), 16).unwrap() & (1 << 23) != 0)
            .unwrap();
        if permitted {
            reference
        } else {
            reference.max(20 - limit.rlim_cur.min(40) as i32)
        }
    }
    let (high, critical) = (expected(-10), expected(-15));
    let original = nice();
    let before = capabilities();
    {
        let _video = ScopedThreadPriority::enter(ThreadPriority::High).unwrap();
        assert_eq!(nice(), high);
        {
            let _control = ScopedThreadPriority::enter(ThreadPriority::Critical).unwrap();
            assert_eq!(nice(), critical);
            let child = std::process::Command::new("/usr/bin/python3")
                .args(["-c", "import os; print(os.getpriority(os.PRIO_PROCESS, 0))"])
                .output()
                .unwrap();
            assert!(child.status.success());
            assert_eq!(
                String::from_utf8(child.stdout).unwrap().trim(),
                critical.to_string()
            );
        }
        assert_eq!(nice(), high);
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
    assert_eq!(
        workers.map(|worker| worker.join().unwrap()),
        [high, critical]
    );
    assert_eq!(nice(), original);
    assert_eq!(capabilities(), before);
    println!(
        "verified: encoder={high} capture={critical} scoped control={critical}; caller priority and capability sets restored"
    );
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This installed-host smoke check requires Linux");
}
