//! Bounded line framing shared by worker pipes and development console input.
use amaterasu_contracts::HostLifecycleAction;
use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

const MAX_LINE_BYTES: usize = 65_536;
const MAX_HOST_RESTARTS: usize = 3;

#[derive(Default)]
pub struct RestartBudget {
    used: usize,
}

impl RestartBudget {
    pub fn next_delay(&mut self) -> Option<Duration> {
        if self.used >= MAX_HOST_RESTARTS {
            return None;
        }
        let delay = Duration::from_millis(100 << self.used);
        self.used += 1;
        Some(delay)
    }
}

pub fn host_action_line(action: HostLifecycleAction) -> &'static str {
    match action {
        HostLifecycleAction::Restart => "HOST_ACTION restart",
        HostLifecycleAction::Quit => "HOST_ACTION quit",
    }
}

pub fn parse_host_action(line: &str) -> Result<Option<HostLifecycleAction>, String> {
    match line {
        "HOST_ACTION restart" => Ok(Some(HostLifecycleAction::Restart)),
        "HOST_ACTION quit" => Ok(Some(HostLifecycleAction::Quit)),
        _ if line.starts_with("HOST_ACTION") => Err("invalid host worker action message".into()),
        _ => Ok(None),
    }
}

pub fn acknowledge_host_action(
    action: HostLifecycleAction,
    worker_exited_successfully: bool,
) -> Result<HostLifecycleAction, String> {
    if worker_exited_successfully {
        Ok(action)
    } else {
        Err("worker lifecycle action was not acknowledged by a clean exit".into())
    }
}

pub fn read_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            return String::from_utf8(line)
                .map(Some)
                .map_err(std::io::Error::other);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |position| position + 1);
        if line.len() + consumed > MAX_LINE_BYTES {
            return Err(std::io::Error::other("control line exceeds 64 KiB"));
        }
        line.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            line.pop();
            return String::from_utf8(line)
                .map(Some)
                .map_err(std::io::Error::other);
        }
    }
}

pub fn lines(input: impl Read + Send + 'static) -> Receiver<Result<String, String>> {
    let (sender, receiver) = mpsc::sync_channel(8);
    // A blocked console reader cannot block shutdown. Worker processes are reaped;
    // console input threads are reclaimed when the command-line process exits.
    std::thread::spawn(move || {
        let mut reader = BufReader::new(input);
        loop {
            match read_line(&mut reader) {
                Ok(Some(line)) => {
                    if sender.send(Ok(line)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let _ = sender.send(Err(error.to_string()));
                    break;
                }
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_final_line_and_rejects_oversized_or_invalid_utf8() {
        let mut input = &b"first\nlast"[..];
        assert_eq!(read_line(&mut input).unwrap().as_deref(), Some("first"));
        assert_eq!(read_line(&mut input).unwrap().as_deref(), Some("last"));
        assert!(read_line(&mut input).unwrap().is_none());
        assert!(read_line(&mut vec![b'x'; MAX_LINE_BYTES + 1].as_slice()).is_err());
        assert!(read_line(&mut &b"\xff\n"[..]).is_err());
    }

    #[test]
    fn host_action_protocol_has_only_fixed_actions_and_requires_clean_exit_ack() {
        for action in [HostLifecycleAction::Restart, HostLifecycleAction::Quit] {
            let line = host_action_line(action);
            assert_eq!(parse_host_action(line).unwrap(), Some(action));
            assert_eq!(acknowledge_host_action(action, true).unwrap(), action);
            assert!(acknowledge_host_action(action, false).is_err());
        }
        for malformed in [
            "HOST_ACTION exec",
            "HOST_ACTION restart arbitrary",
            "HOST_ACTION",
        ] {
            assert!(parse_host_action(malformed).is_err());
        }
        assert_eq!(parse_host_action("HEARTBEAT").unwrap(), None);
    }

    #[test]
    fn host_restart_budget_is_finite_and_uses_bounded_backoff() {
        let mut budget = RestartBudget::default();
        assert_eq!(budget.next_delay(), Some(Duration::from_millis(100)));
        assert_eq!(budget.next_delay(), Some(Duration::from_millis(200)));
        assert_eq!(budget.next_delay(), Some(Duration::from_millis(400)));
        assert_eq!(budget.next_delay(), None);
    }
}
