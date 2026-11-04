use crate::{Snapshot, new_snapshot};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// Xwayland cannot establish the compositor's foreground window. Only an
// explicitly identified native X11 session is eligible.
fn local_x11_session(session: Option<&str>, display: Option<&str>, wayland: bool) -> bool {
    if session != Some("x11") || wayland {
        return false;
    }
    let Some(display) = display.and_then(|value| value.strip_prefix(':')) else {
        return false;
    };
    let mut parts = display.split('.');
    let valid_number = |part: &str| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit());
    parts.next().is_some_and(valid_number)
        && parts.next().is_none_or(valid_number)
        && parts.next().is_none()
}

fn run_helper(program: &str, arguments: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut output = Vec::new();
                child
                    .stdout
                    .take()?
                    .take(4097)
                    .read_to_end(&mut output)
                    .ok()?;
                if output.len() > 4096 {
                    return None;
                }
                return String::from_utf8(output).ok();
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn xprop(arguments: &[&str]) -> Option<String> {
    run_helper("xprop", arguments, Duration::from_millis(200))
}

fn active_window() -> Option<String> {
    let output = xprop(&["-root", "_NET_ACTIVE_WINDOW"])?;
    let value = output
        .trim()
        .strip_prefix("_NET_ACTIVE_WINDOW(WINDOW): window id # ")?;
    let value = value.strip_prefix("0x")?;
    if value.is_empty()
        || value.len() > 8
        || !value.bytes().all(|b| b.is_ascii_hexdigit())
        || u32::from_str_radix(value, 16).ok()? == 0
    {
        return None;
    }
    Some(format!("0x{value}"))
}

fn properties(window: &str) -> Option<(u32, Option<bool>)> {
    let output = xprop(&["-id", window, "_NET_WM_PID", "_NET_WM_STATE"])?;
    parse_properties(&output)
}

fn parse_properties(output: &str) -> Option<(u32, Option<bool>)> {
    let mut pid = None;
    let mut fullscreen = None;
    for line in output.lines() {
        if let Some(value) = line.strip_prefix("_NET_WM_PID(CARDINAL) = ") {
            let parsed = value.parse::<u32>().ok()?;
            if parsed == 0 || pid.replace(parsed).is_some() {
                return None;
            }
        }
        if let Some(value) = line.strip_prefix("_NET_WM_STATE(ATOM) = ") {
            if fullscreen.is_some() {
                return None;
            }
            fullscreen = Some(
                value
                    .split(',')
                    .any(|state| state.trim() == "_NET_WM_STATE_FULLSCREEN"),
            );
        }
    }
    Some((pid?, fullscreen))
}

pub fn observe() -> Option<Snapshot> {
    let session = std::env::var("XDG_SESSION_TYPE").ok();
    let display = std::env::var("DISPLAY").ok();
    if session.as_deref() == Some("wayland") {
        return kwin();
    }
    if !local_x11_session(
        session.as_deref(),
        display.as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
    ) {
        return None;
    }
    let window = active_window()?;
    let (pid, fullscreen) = properties(&window)?;
    let start = process_start(pid)?;
    let executable = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()?;
    // Confirm the observation did not change while resolving the process.
    if active_window()? != window
        || properties(&window)? != (pid, fullscreen)
        || process_start(pid)? != start
    {
        return None;
    }
    new_snapshot(executable, fullscreen)
}

fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command field can contain spaces and parentheses. The final closing
    // parenthesis precedes field 3 (state); field 22 is offset 19 thereafter.
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires a running KWin session and its Python D-Bus dependencies"]
    fn live_kwin_snapshot_is_bounded_and_consistent() {
        let snapshot = kwin().expect("KWin did not supply a stable active window");
        assert!(snapshot.validate().is_ok());
        assert!(snapshot.fullscreen.is_some());
    }

    #[test]
    fn rejects_xwayland_remote_and_ambiguous_sessions() {
        assert!(local_x11_session(Some("x11"), Some(":0.1"), false));
        for (session, display, wayland) in [
            (Some("wayland"), Some(":0"), false),
            (Some("x11"), Some(":0"), true),
            (None, Some(":0"), false),
            (Some("x11"), Some("host:0"), false),
            (Some("x11"), Some(":0.1.2"), false),
        ] {
            assert!(!local_x11_session(session, display, wayland));
        }
    }
    #[test]
    fn parses_only_explicit_fullscreen_state_and_process() {
        assert_eq!(
            parse_properties(
                "_NET_WM_PID(CARDINAL) = 123\n_NET_WM_STATE(ATOM) = _NET_WM_STATE_FULLSCREEN, _NET_WM_STATE_ABOVE\n"
            ),
            Some((123, Some(true)))
        );
        assert_eq!(
            parse_properties("_NET_WM_PID(CARDINAL) = 123\n_NET_WM_STATE: not found.\n"),
            Some((123, None))
        );
        assert_eq!(parse_properties("_NET_WM_PID(CARDINAL) = 0\n"), None);
        assert_eq!(
            parse_properties("_NET_WM_PID(CARDINAL) = 1\n_NET_WM_PID(CARDINAL) = 2\n"),
            None
        );
    }
}

pub fn reconcile() {
    if std::env::var("XDG_SESSION_TYPE").as_deref() == Ok("wayland") {
        let _ = run_helper(
            "python3",
            &["-c", include_str!("kwin.py"), "--reconcile"],
            Duration::from_millis(210),
        );
    }
}

#[derive(serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct KwinObservation {
    pid: u32,
    fullscreen: bool,
}

fn kwin_observation() -> Option<KwinObservation> {
    let output = run_helper(
        "python3",
        &["-c", include_str!("kwin.py")],
        Duration::from_millis(210),
    )?;
    let observation: KwinObservation = serde_json::from_str(&output).ok()?;
    (observation.pid != 0).then_some(observation)
}

fn kwin() -> Option<Snapshot> {
    let first = kwin_observation()?;
    let start = process_start(first.pid)?;
    let executable = std::fs::read_link(format!("/proc/{}/exe", first.pid))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()?;
    if kwin_observation()? != first || process_start(first.pid)? != start {
        return None;
    }
    new_snapshot(executable, Some(first.fullscreen))
}
