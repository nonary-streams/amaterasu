//! Bounded subprocess ownership for KScreen queries and adjustments.
use crate::engine::Result;
use std::{
    io::{self, Read},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

pub(super) fn run(arguments: &[String], timeout: Duration) -> Result<Vec<u8>> {
    let child = Command::new("kscreen-doctor")
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| "display_backend_unavailable")?;
    collect(child, timeout)
}

struct OwnedCommand {
    child: Child,
    status: Option<ExitStatus>,
    complete: bool,
}
impl Drop for OwnedCommand {
    fn drop(&mut self) {
        if !self.complete {
            // SAFETY: every caller starts a fresh process group for this exact
            // child. Kill that group so descendants cannot outlive the owned
            // adjustment or retain its output pipe after a rejected completion.
            unsafe { libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL) };
        }
        if self.status.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn collect(child: Child, timeout: Duration) -> Result<Vec<u8>> {
    let mut command = OwnedCommand {
        child,
        status: None,
        complete: false,
    };
    let mut pipe = command
        .child
        .stdout
        .take()
        .ok_or("display_backend_failed")?;
    // SAFETY: the descriptor belongs to this live pipe and fcntl reads/updates
    // its file status flags. The owned pipe stays alive throughout collection.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err("display_backend_failed".into());
    }
    let deadline = Instant::now() + timeout;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        if command.status.is_none() {
            command.status = command
                .child
                .try_wait()
                .map_err(|_| "display_backend_failed")?;
        }
        let mut eof = false;
        let mut would_block = false;
        // A continuously writing process cannot postpone the next deadline check.
        for _ in 0..8 {
            match pipe.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(length) => {
                    if bytes.len().saturating_add(length) > MAX_OUTPUT_BYTES {
                        return Err("display_backend_failed".into());
                    }
                    bytes.extend_from_slice(&buffer[..length]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    would_block = true;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("display_backend_failed".into()),
            }
        }
        if let Some(status) = command.status {
            if eof {
                command.complete = true;
                return if status.success() {
                    Ok(bytes)
                } else {
                    Err("display_backend_failed".into())
                };
            }
            if would_block {
                // The direct child is finished but another process still owns
                // its pipe. Completion is unknown; retire the whole group.
                return Err("display_backend_failed".into());
            }
        }
        if Instant::now() >= deadline {
            return Err("display_backend_failed".into());
        }
        if would_block || eof {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn child(script: &str) -> Child {
        Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap()
    }
    #[test]
    fn collects_complete_query_and_rejects_failed_or_unbounded_output() {
        assert_eq!(
            collect(child("printf '{\"outputs\":[]}'"), Duration::from_secs(1)).unwrap(),
            br#"{"outputs":[]}"#
        );
        assert!(collect(child("printf partial; exit 1"), Duration::from_secs(1)).is_err());
        assert!(collect(child("head -c 1100000 /dev/zero"), Duration::from_secs(1)).is_err());
    }
    #[test]
    fn retires_descendants_that_keep_the_completed_commands_pipe_open() {
        let path = std::env::temp_dir().join(format!(
            "amaterasu-display-settings-command-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let child = Command::new("sh")
            .args([
                "-c",
                "(sleep 0.15; printf late > \"$1\") & printf partial",
                "display-command",
            ])
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let start = Instant::now();
        assert!(collect(child, Duration::from_secs(1)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        thread::sleep(Duration::from_millis(300));
        let outlived = path.exists();
        let _ = std::fs::remove_file(path);
        assert!(!outlived, "the adjustment left a descendant alive");
    }
    #[test]
    fn deadline_does_not_wait_for_descendants_to_close_pipes() {
        let start = Instant::now();
        assert!(collect(child("sleep 2 & exec sleep 5"), Duration::from_millis(30)).is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
