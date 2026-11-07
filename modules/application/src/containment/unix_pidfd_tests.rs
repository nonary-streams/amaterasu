use super::*;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

// The forked fixture calls only libc operations before it exits. Its sole
// sibling blocks on a pipe until the test releases it; the test owns the
// child's process group and always releases and reaps it, including on panic.
struct ThreadGroupFixture {
    pid: libc::pid_t,
    release: RawFd,
    ready: RawFd,
    reaped: bool,
}

struct ThreadArgs {
    release: RawFd,
    ready: RawFd,
}

// After the timeout, retaining ownership requires waiting for the killed
// fixture child. A kernel task stuck in uninterruptible sleep can delay this
// wait indefinitely; SIGKILL acceptance alone is not retirement evidence.
fn wait_owned_after_kill(pid: libc::pid_t) -> bool {
    loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pid, &mut status, 0) };
        if result == pid {
            return true;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            eprintln!("fixture child {pid} could not be reaped: {error}");
            return false;
        }
    }
}

extern "C" fn held_sibling(argument: *mut libc::c_void) -> libc::c_int {
    // SAFETY: the argument points into the forked process's stack, which stays
    // mapped after its leader exits; this thread is its only reader.
    let args = unsafe { &*(argument.cast::<ThreadArgs>()) };
    let byte = [1_u8];
    // SAFETY: these descriptors and buffers belong to this fixture process.
    unsafe {
        libc::write(args.ready, byte.as_ptr().cast(), 1);
        let mut signal = [0_u8];
        libc::read(args.release, signal.as_mut_ptr().cast(), 1);
    }
    0
}

impl ThreadGroupFixture {
    fn spawn() -> Self {
        let mut release = [-1; 2];
        let mut ready = [-1; 2];
        // SAFETY: each pointer names a writable two-element descriptor array.
        assert_eq!(
            unsafe { libc::pipe2(release.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        if unsafe { libc::pipe2(ready.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            unsafe {
                libc::close(release[0]);
                libc::close(release[1]);
            }
            panic!("could not create fixture readiness pipe");
        }
        // Allocate before fork; the child does not invoke the Rust allocator.
        let mut stack = vec![0_u128; 16 * 1024];
        let mut args = ThreadArgs {
            release: release[0],
            ready: ready[1],
        };
        // SAFETY: the child calls only the async-signal-safe libc operations
        // below and a clone trampoline whose body uses only read/write.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            unsafe {
                libc::close(release[1]);
                libc::close(ready[0]);
                if libc::setpgid(0, 0) != 0 {
                    libc::_exit(121);
                }
                let stack_top = stack.as_mut_ptr().add(stack.len()).cast();
                let flags = libc::CLONE_VM
                    | libc::CLONE_FS
                    | libc::CLONE_FILES
                    | libc::CLONE_SIGHAND
                    | libc::CLONE_THREAD
                    | libc::CLONE_SYSVSEM;
                if libc::clone(
                    held_sibling,
                    stack_top,
                    flags,
                    (&mut args as *mut ThreadArgs).cast(),
                ) < 0
                {
                    libc::_exit(122);
                }
                // Exit just the leader, leaving the sibling alive. Calling
                // _exit would instead terminate the complete thread group.
                libc::syscall(libc::SYS_exit, 0);
                libc::_exit(123);
            }
        }
        unsafe {
            libc::close(release[0]);
            libc::close(ready[1]);
        }
        if pid < 0 {
            unsafe {
                libc::close(release[1]);
                libc::close(ready[0]);
            }
            panic!("could not fork fixture process");
        }
        Self {
            pid,
            release: release[1],
            ready: ready[0],
            reaped: false,
        }
    }

    fn await_zombie_leader(&self) {
        let mut poll = libc::pollfd {
            fd: self.ready,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one valid descriptor structure.
        assert_eq!(
            unsafe { libc::poll(&mut poll, 1, 5_000) },
            1,
            "sibling did not start"
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", self.pid))
                && stat
                    .rsplit_once(')')
                    .is_some_and(|(_, fields)| fields.split_whitespace().next() == Some("Z"))
            {
                break;
            }
            assert!(Instant::now() < deadline, "leader did not become a zombie");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn release_and_wait(&mut self) {
        if self.reaped {
            return;
        }
        let byte = [1_u8];
        // SAFETY: the descriptors are owned by the fixture, and the buffer is
        // valid. SIGPIPE is ignored by the Rust runtime.
        unsafe {
            libc::write(self.release, byte.as_ptr().cast(), 1);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut status = 0;
            let result = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
            if result == self.pid {
                self.reaped = true;
                break;
            }
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                eprintln!(
                    "fixture wait ownership was lost: {}",
                    io::Error::last_os_error()
                );
                break;
            }
            if Instant::now() >= deadline {
                // setpgid ran before the readiness notification. The exact
                // still-owned fixture group is safe to kill if it is stuck.
                unsafe {
                    libc::kill(-self.pid, libc::SIGKILL);
                }
                self.reaped = wait_owned_after_kill(self.pid);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for ThreadGroupFixture {
    fn drop(&mut self) {
        self.release_and_wait();
        unsafe {
            libc::close(self.release);
            libc::close(self.ready);
        }
    }
}

#[test]
#[ignore = "requires unrestricted procfs, openat2, pidfds and clone support"]
fn pidfd_and_group_observer_keep_a_zombie_leader_with_a_live_sibling() {
    let mut fixture = ThreadGroupFixture::spawn();
    fixture.await_zombie_leader();
    assert!(!linux_process_has_exited(fixture.pid as u32).unwrap());
    assert!(linux_group_has_live_members(fixture.pid as u32).unwrap());

    let byte = [1_u8];
    assert_eq!(
        unsafe { libc::write(fixture.release, byte.as_ptr().cast(), 1) },
        1
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !linux_process_has_exited(fixture.pid as u32).unwrap() {
        assert!(Instant::now() < deadline, "sibling did not retire");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!linux_group_has_live_members(fixture.pid as u32).unwrap());
    fixture.release_and_wait();
    assert!(fixture.reaped, "fixture root was not reaped");
}

// The root waits for an explicit acknowledgement before reaping its child, so
// the descendant PID cannot be recycled while the observer opens its pidfd.
struct DescendantFixture {
    root: libc::pid_t,
    descendant: libc::pid_t,
    release: RawFd,
    acknowledge: RawFd,
    pid_read: RawFd,
    ready_read: RawFd,
    descendant_pidfd: RawFd,
    reaped: bool,
}

impl DescendantFixture {
    fn spawn() -> Self {
        let mut pipes = [[-1; 2]; 4];
        for index in 0..pipes.len() {
            if unsafe { libc::pipe2(pipes[index].as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                for pipe in &pipes[..index] {
                    unsafe {
                        libc::close(pipe[0]);
                        libc::close(pipe[1]);
                    }
                }
                panic!("could not create descendant fixture pipe");
            }
        }
        let mut stack = vec![0_u128; 16 * 1024];
        let mut args = ThreadArgs {
            release: pipes[2][0],
            ready: pipes[1][1],
        };
        // SAFETY: after fork both fixture processes use libc operations only.
        let root = unsafe { libc::fork() };
        if root == 0 {
            unsafe {
                libc::close(pipes[0][0]);
                libc::close(pipes[1][0]);
                libc::close(pipes[2][1]);
                libc::close(pipes[3][1]);
                if libc::setpgid(0, 0) != 0 {
                    libc::_exit(121);
                }
                let descendant = libc::fork();
                if descendant == 0 {
                    libc::close(pipes[0][1]);
                    libc::close(pipes[3][0]);
                    let flags = libc::CLONE_VM
                        | libc::CLONE_FS
                        | libc::CLONE_FILES
                        | libc::CLONE_SIGHAND
                        | libc::CLONE_THREAD
                        | libc::CLONE_SYSVSEM;
                    let top = stack.as_mut_ptr().add(stack.len()).cast();
                    if libc::clone(
                        held_sibling,
                        top,
                        flags,
                        (&mut args as *mut ThreadArgs).cast(),
                    ) < 0
                    {
                        libc::_exit(122);
                    }
                    libc::syscall(libc::SYS_exit, 0);
                    libc::_exit(123);
                }
                if descendant < 0 {
                    libc::_exit(124);
                }
                libc::close(pipes[1][1]);
                libc::close(pipes[2][0]);
                let bytes = descendant.to_ne_bytes();
                if libc::write(pipes[0][1], bytes.as_ptr().cast(), bytes.len())
                    != bytes.len() as isize
                {
                    libc::_exit(125);
                }
                let mut byte = [0_u8];
                libc::read(pipes[3][0], byte.as_mut_ptr().cast(), 1);
                let mut status = 0;
                let waited = loop {
                    let result = libc::waitpid(descendant, &mut status, 0);
                    if result >= 0 || *libc::__errno_location() != libc::EINTR {
                        break result;
                    }
                };
                if waited != descendant
                    || !libc::WIFEXITED(status)
                    || libc::WEXITSTATUS(status) != 0
                {
                    libc::_exit(126);
                }
                libc::_exit(0);
            }
        }
        for (index, pipe) in pipes.iter().enumerate() {
            unsafe {
                libc::close(pipe[if index < 2 { 1 } else { 0 }]);
            }
        }
        if root < 0 {
            for (index, pipe) in pipes.iter().enumerate() {
                unsafe {
                    libc::close(pipe[if index < 2 { 0 } else { 1 }]);
                }
            }
            panic!("could not fork descendant fixture root");
        }
        let mut fixture = Self {
            root,
            descendant: 0,
            release: pipes[2][1],
            acknowledge: pipes[3][1],
            pid_read: pipes[0][0],
            ready_read: pipes[1][0],
            descendant_pidfd: -1,
            reaped: false,
        };
        let mut poll = libc::pollfd {
            fd: pipes[0][0],
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut poll, 1, 5_000) },
            1,
            "root did not report its descendant"
        );
        let mut bytes = [0_u8; std::mem::size_of::<libc::pid_t>()];
        assert_eq!(
            unsafe { libc::read(pipes[0][0], bytes.as_mut_ptr().cast(), bytes.len()) },
            bytes.len() as isize
        );
        unsafe {
            libc::close(pipes[0][0]);
        }
        fixture.pid_read = -1;
        fixture.descendant = libc::pid_t::from_ne_bytes(bytes);
        assert!(fixture.descendant > 0);
        fixture.descendant_pidfd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, fixture.descendant, 0) as RawFd };
        assert!(
            fixture.descendant_pidfd >= 0,
            "could not pin descendant identity"
        );
        poll.fd = pipes[1][0];
        poll.revents = 0;
        assert_eq!(
            unsafe { libc::poll(&mut poll, 1, 5_000) },
            1,
            "descendant sibling did not start"
        );
        unsafe {
            libc::close(pipes[1][0]);
        }
        fixture.ready_read = -1;
        fixture
    }
}

impl Drop for DescendantFixture {
    fn drop(&mut self) {
        if !self.reaped {
            let byte = [1_u8];
            unsafe {
                libc::write(self.release, byte.as_ptr().cast(), 1);
                libc::write(self.acknowledge, byte.as_ptr().cast(), 1);
            }
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let mut status = 0;
                let result = unsafe { libc::waitpid(self.root, &mut status, libc::WNOHANG) };
                if result == self.root {
                    self.reaped = true;
                    break;
                }
                if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    eprintln!(
                        "fixture root wait ownership was lost: {}",
                        io::Error::last_os_error()
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    // Signal the pinned descendant first, allowing its root to
                    // reap it. The fallback group is still reserved by root.
                    unsafe {
                        if self.descendant_pidfd >= 0 {
                            libc::syscall(
                                libc::SYS_pidfd_send_signal,
                                self.descendant_pidfd,
                                libc::SIGKILL,
                                std::ptr::null::<libc::siginfo_t>(),
                                0,
                            );
                        }
                    }
                    let final_deadline = Instant::now() + Duration::from_secs(5);
                    loop {
                        let result =
                            unsafe { libc::waitpid(self.root, &mut status, libc::WNOHANG) };
                        if result == self.root {
                            self.reaped = true;
                            break;
                        }
                        if result < 0
                            && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                        {
                            eprintln!(
                                "fixture root wait ownership was lost: {}",
                                io::Error::last_os_error()
                            );
                            break;
                        }
                        if Instant::now() >= final_deadline {
                            // Last resort: the root may not have reaped its
                            // descendant; killing the group cannot prove it.
                            unsafe {
                                libc::kill(-self.root, libc::SIGKILL);
                            }
                            self.reaped = wait_owned_after_kill(self.root);
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        unsafe {
            libc::close(self.release);
            libc::close(self.acknowledge);
            if self.pid_read >= 0 {
                libc::close(self.pid_read);
            }
            if self.ready_read >= 0 {
                libc::close(self.ready_read);
            }
            if self.descendant_pidfd >= 0 {
                libc::close(self.descendant_pidfd);
            }
        }
    }
}

#[test]
#[ignore = "requires unrestricted procfs, pidfds and clone support"]
fn pidfd_observes_non_child_descendant_until_its_sibling_exits() {
    let mut fixture = DescendantFixture::spawn();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", fixture.descendant))
            && stat
                .rsplit_once(')')
                .is_some_and(|(_, fields)| fields.split_whitespace().next() == Some("Z"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "descendant leader did not become a zombie"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!linux_process_has_exited(fixture.descendant as u32).unwrap());
    let byte = [1_u8];
    assert_eq!(
        unsafe { libc::write(fixture.release, byte.as_ptr().cast(), 1) },
        1
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !linux_process_has_exited(fixture.descendant as u32).unwrap() {
        assert!(Instant::now() < deadline, "descendant did not retire");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        unsafe { libc::write(fixture.acknowledge, byte.as_ptr().cast(), 1) },
        1
    );
    let mut status = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = unsafe { libc::waitpid(fixture.root, &mut status, libc::WNOHANG) };
        if result == fixture.root {
            break;
        }
        assert_eq!(result, 0, "root wait failed");
        assert!(
            Instant::now() < deadline,
            "root did not reap its descendant"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    fixture.reaped = true;
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "root failed to reap its descendant"
    );
}
