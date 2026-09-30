use super::rpc;
use serde_json::json;

const SERVICE: &str = "amaterasu.host-metrics/1";

enum Phase {
    Idle,
    Beginning(rpc::Pending),
    Active,
    Ending(rpc::Pending),
    Complete,
}

pub(super) struct HostMetricsActivity {
    session_id: String,
    phase: Phase,
    end_requested: bool,
}

impl HostMetricsActivity {
    pub fn new(record_id: u64) -> Self {
        Self {
            session_id: format!("stream-session-{:016x}", record_id),
            phase: Phase::Idle,
            end_requested: false,
        }
    }

    pub fn activated(&mut self, record_id: u64) {
        if !matches!(self.phase, Phase::Idle) {
            return;
        }
        self.phase = Phase::Beginning(activity_request(&self.session_id, record_id, true));
    }

    pub fn teardown_confirmed(&mut self, record_id: u64) {
        self.end_requested = true;
        if matches!(self.phase, Phase::Active) {
            self.phase = Phase::Ending(activity_request(&self.session_id, record_id, false));
        }
    }

    pub fn progress(&mut self, record_id: u64, now: u64, send: &mut rpc::Send<'_>) {
        let phase = std::mem::replace(&mut self.phase, Phase::Complete);
        self.phase = match phase {
            Phase::Beginning(mut pending) => match pending.poll(send, now) {
                rpc::Progress::Result(response) => {
                    if pending.release_result(send) {
                        if response["ok"] == true && response["active"] == true {
                            self.next_after_begin(record_id)
                        } else {
                            // A completed rejection proves no lease was retained.
                            Phase::Complete
                        }
                    } else {
                        Phase::Beginning(pending)
                    }
                }
                rpc::Progress::Failed if pending.admission_rejected() => Phase::Complete,
                rpc::Progress::Failed | rpc::Progress::Unresolved => {
                    // Keep the same operation until an ambiguous begin settles so
                    // an end request can never overtake it.
                    pending.retry();
                    Phase::Beginning(pending)
                }
                rpc::Progress::Waiting => Phase::Beginning(pending),
            },
            Phase::Active if self.end_requested => {
                Phase::Ending(activity_request(&self.session_id, record_id, false))
            }
            Phase::Active => Phase::Active,
            Phase::Ending(mut pending) => match pending.poll(send, now) {
                rpc::Progress::Result(response) => {
                    if pending.release_result(send) {
                        if response["ok"] == true {
                            Phase::Complete
                        } else {
                            pending.retry();
                            Phase::Ending(pending)
                        }
                    } else {
                        Phase::Ending(pending)
                    }
                }
                rpc::Progress::Failed | rpc::Progress::Unresolved => {
                    pending.retry();
                    Phase::Ending(pending)
                }
                rpc::Progress::Waiting => Phase::Ending(pending),
            },
            Phase::Idle => Phase::Idle,
            Phase::Complete => Phase::Complete,
        };
    }

    pub fn shutdown(&mut self, record_id: u64, now: u64, send: &mut rpc::Send<'_>) -> bool {
        self.teardown_confirmed(record_id);
        for turn in 0..16 {
            self.progress(record_id, now.saturating_add(turn), send);
            if matches!(self.phase, Phase::Complete | Phase::Idle) {
                return true;
            }
        }
        if let Phase::Beginning(pending) = &mut self.phase {
            if !pending.shutdown(send) {
                return false;
            }
            self.phase = Phase::Ending(activity_request(&self.session_id, record_id, false));
            for turn in 0..16 {
                self.progress(record_id, now.saturating_add(16 + turn), send);
                if matches!(self.phase, Phase::Complete) {
                    return true;
                }
            }
        }
        match &mut self.phase {
            Phase::Ending(pending) => pending.shutdown(send),
            Phase::Idle | Phase::Beginning(_) | Phase::Active | Phase::Complete => false,
        }
    }

    fn next_after_begin(&self, record_id: u64) -> Phase {
        if self.end_requested {
            Phase::Ending(activity_request(&self.session_id, record_id, false))
        } else {
            Phase::Active
        }
    }
}

fn activity_request(session_id: &str, record_id: u64, begin: bool) -> rpc::Pending {
    let (action, suffix) = if begin {
        ("stream_activity_begin", "begin")
    } else {
        ("stream_activity_end", "end")
    };
    rpc::Pending::new(
        SERVICE,
        json!({"action":action,"session_id":session_id}),
        format!("ss-{:016x}-host-metrics-{}", record_id, suffix),
        None,
    )
    .retry_uncertain_admission()
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_module_sdk::{Command, ModuleResult, Response};
    use serde_json::Value;
    use std::collections::VecDeque;

    struct Broker {
        responses: VecDeque<Response>,
        calls: Vec<Command>,
    }

    impl Broker {
        fn send(&mut self, command: Command) -> ModuleResult<Response> {
            self.calls.push(command);
            Ok(self.responses.pop_front().unwrap_or(Response::Pending))
        }
    }

    #[test]
    fn end_waits_for_an_ambiguous_begin_and_releases_once_cleanup_is_confirmed() {
        let mut activity = HostMetricsActivity::new(7);
        activity.activated(7);
        let mut broker = Broker {
            responses: VecDeque::from([
                Response::Accepted { id: 1 },
                Response::Result {
                    payload: json!({"ok":true,"active":true}),
                },
                Response::Ok,
                Response::Accepted { id: 2 },
                Response::Result {
                    payload: json!({"ok":true,"active":false}),
                },
                Response::Ok,
            ]),
            calls: Vec::new(),
        };
        activity.progress(7, 1, &mut |command| broker.send(command));
        activity.teardown_confirmed(7);
        activity.progress(7, 2, &mut |command| broker.send(command));
        assert!(matches!(activity.phase, Phase::Ending(_)));
        activity.progress(7, 3, &mut |command| broker.send(command));
        assert!(matches!(activity.phase, Phase::Ending(_)));
        activity.progress(7, 4, &mut |command| broker.send(command));
        activity.progress(7, 5, &mut |command| broker.send(command));
        assert!(matches!(activity.phase, Phase::Complete));
        let operations = broker
            .calls
            .iter()
            .filter_map(|command| match command {
                Command::Request { payload, .. } => payload.get("action").and_then(Value::as_str),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(operations, ["stream_activity_begin", "stream_activity_end"]);
    }

    #[test]
    fn failed_start_before_activation_never_creates_a_metric_consumer() {
        let mut activity = HostMetricsActivity::new(9);
        activity.teardown_confirmed(9);
        assert!(activity.shutdown(9, 0, &mut |_| Ok(Response::Ok)));
    }
}
