//! Parent-bound, asynchronous reservation of a shortcut action. Neither
//! request performs an action; failures must leave the source key unconsumed.
use amaterasu_extension_api::ActionInvoke;
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde_json::{Value, json};

const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const ACTIONS: &str = "amaterasu.extension-actions/1";

#[derive(Clone, Debug)]
pub(crate) struct ApprovedReservation {
    pub invoke: ActionInvoke,
}

enum Phase {
    LifecycleSubmit,
    LifecycleWait(u64),
    ActionSubmit,
    ActionWait(u64),
    Finished,
}

pub(crate) struct ReservationJob {
    parent: u64,
    input_binding: String,
    invoke: ActionInvoke,
    timeout_ms: u64,
    phase: Phase,
    reservation_attempted: bool,
    instance: String,
}

impl ReservationJob {
    pub fn new(
        parent: u64,
        input_binding: String,
        invoke: ActionInvoke,
        timeout_ms: u64,
    ) -> Result<Self, String> {
        if input_binding.is_empty()
            || invoke.input_reservation_id.is_some()
            || invoke.sequence == 0
            || invoke.operation_id != format!("{}:{}", invoke.epoch, invoke.sequence)
            || timeout_ms == 0
        {
            return Err("invalid shortcut reservation request".into());
        }
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| "reservation identity unavailable")?;
        let instance = random.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            instance,
            parent,
            input_binding,
            invoke,
            timeout_ms,
            phase: Phase::LifecycleSubmit,
            reservation_attempted: false,
        })
    }

    /// An ambiguous reservation attempt must remain fenced until reconciled.
    pub fn may_have_reservation(&self) -> bool {
        self.reservation_attempted
    }

    /// Each step submits or polls one broker call. Any unknown or rejected
    /// result fails closed; the caller must not publish a consumed key.
    pub fn advance(
        &mut self,
        send: &mut impl FnMut(Command) -> ModuleResult<Response>,
    ) -> Result<Option<ApprovedReservation>, String> {
        match self.phase {
            Phase::LifecycleSubmit => {
                self.reservation_attempted = true;
                let payload = json!({"operation":"reserve_input_action", "operation_id":self.invoke.operation_id,
                    "token":self.input_binding, "action_id":self.invoke.action_id});
                let operation_id = format!("input-lifecycle-reserve-{}", self.instance);
                match send(Command::Request {
                    service: LIFECYCLE.into(),
                    payload,
                    timeout_ms: self.timeout_ms,
                    operation_id,
                    parent: Some(self.parent),
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.phase = Phase::LifecycleWait(id);
                        Ok(None)
                    }
                    _ => Err("shortcut lifecycle reservation unavailable".into()),
                }
            }
            Phase::LifecycleWait(request) => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => Ok(None),
                Ok(Response::Result { payload }) => {
                    if !matches!(send(Command::ReleaseResult { request }), Ok(Response::Ok)) {
                        return Err("shortcut lifecycle result release failed".into());
                    }
                    let reservation = payload["reservation_id"]
                        .as_str()
                        .filter(|id| !id.is_empty());
                    let expected_target = serde_json::to_value(&self.invoke.target)
                        .map_err(|_| "invalid action target")?;
                    if payload["status"] != "succeeded"
                        || payload["operation_id"] != self.invoke.operation_id
                        || payload["action_id"] != self.invoke.action_id
                        || payload["target"] != expected_target
                        || reservation.is_none()
                    {
                        return Err("shortcut lifecycle reservation rejected".into());
                    }
                    self.invoke.input_reservation_id = reservation.map(str::to_owned);
                    self.phase = Phase::ActionSubmit;
                    Ok(None)
                }
                _ => Err("shortcut lifecycle reservation unresolved".into()),
            },
            Phase::ActionSubmit => {
                let mut payload =
                    serde_json::to_value(&self.invoke).map_err(|_| "invalid shortcut action")?;
                payload["operation"] = json!("reserve_input");
                let operation_id = format!("input-action-reserve-{}", self.instance);
                match send(Command::Request {
                    service: ACTIONS.into(),
                    payload,
                    timeout_ms: self.timeout_ms,
                    operation_id,
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => {
                        self.phase = Phase::ActionWait(id);
                        Ok(None)
                    }
                    _ => Err("shortcut action admission unavailable".into()),
                }
            }
            Phase::ActionWait(request) => match send(Command::TakeResult { request }) {
                Ok(Response::Pending) => Ok(None),
                Ok(Response::Result { payload }) => {
                    if !matches!(send(Command::ReleaseResult { request }), Ok(Response::Ok)) {
                        return Err("shortcut action result release failed".into());
                    }
                    if payload["status"] != "reserved"
                        || payload["operation_id"] != self.invoke.operation_id
                    {
                        return Err("shortcut action admission rejected".into());
                    }
                    self.phase = Phase::Finished;
                    Ok(Some(ApprovedReservation {
                        invoke: self.invoke.clone(),
                    }))
                }
                _ => Err("shortcut action admission rejected or unresolved".into()),
            },
            Phase::Finished => Err("shortcut reservation already completed".into()),
        }
    }
}

/// Construct the idempotent no-replay action commit only after the input
/// delivery receipt and dispatching phase are durably persisted.
pub(crate) fn commit_request(invoke: &ActionInvoke) -> Value {
    json!({"operation":"commit_input", "epoch":invoke.epoch, "sequence":invoke.sequence,
        "operation_id":invoke.operation_id})
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_extension_api::ActionTarget;

    fn invoke() -> ActionInvoke {
        ActionInvoke {
            epoch: "action-epoch".into(),
            sequence: 1,
            operation_id: "action-epoch:1".into(),
            action_id: "example/remove".into(),
            target: ActionTarget::Session {
                host_epoch: "host".into(),
                session_id: "session".into(),
                expected_generation: 3,
            },
            arguments: json!({}),
            expected_settings_revision: None,
            input_reservation_id: None,
        }
    }

    #[test]
    fn valid_authority_and_action_admission_are_required_before_approval() {
        let mut job = ReservationJob::new(17, "binding-token".into(), invoke(), 1000).unwrap();
        let mut sent = Vec::new();
        let mut send = |command: Command| -> ModuleResult<Response> {
            match command {
                Command::Request {
                    service,
                    payload,
                    parent,
                    operation_id,
                    ..
                } => {
                    assert!(
                        operation_id.len() <= 128
                            && operation_id.bytes().all(|b| b.is_ascii_lowercase()
                                || b.is_ascii_digit()
                                || b"._-/".contains(&b))
                    );
                    if service == LIFECYCLE {
                        assert_eq!(parent, Some(17));
                    } else {
                        assert_eq!(parent, None);
                    }
                    sent.push((service, payload));
                    Ok(Response::Accepted {
                        id: sent.len() as u64,
                    })
                }
                Command::ReleaseResult { .. } => Ok(Response::Ok),
                Command::TakeResult { request: 1 } => Ok(Response::Result {
                    payload: json!({
                    "status":"succeeded", "operation_id":"action-epoch:1", "action_id":"example/remove",
                    "target":invoke().target, "reservation_id":"reservation" }),
                }),
                Command::TakeResult { request: 2 } => Ok(Response::Result {
                    payload: json!({"status":"reserved","operation_id":"action-epoch:1"}),
                }),
                _ => panic!("unexpected broker command"),
            }
        };
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        assert!(job.advance(&mut send).unwrap().is_none());
        let approved = job.advance(&mut send).unwrap().unwrap();
        assert_eq!(
            approved.invoke.input_reservation_id.as_deref(),
            Some("reservation")
        );
        assert_eq!(sent[0].0, LIFECYCLE);
        assert_eq!(sent[1].1["operation"], "reserve_input");
        assert_eq!(
            commit_request(&approved.invoke)["operation"],
            "commit_input"
        );
    }

    #[test]
    fn authority_target_mismatch_never_submits_action() {
        let mut job = ReservationJob::new(17, "binding-token".into(), invoke(), 1000).unwrap();
        assert!(
            job.advance(&mut |_| Ok(Response::Accepted { id: 1 }))
                .unwrap()
                .is_none()
        );
        let result = job.advance(&mut |command| {
            if matches!(command, Command::ReleaseResult { .. }) {
                return Ok(Response::Ok);
            }
            Ok(Response::Result {
                payload: json!({
            "status":"succeeded", "operation_id":"action-epoch:1", "action_id":"example/remove",
            "target":{"kind":"host","host_epoch":"host"}, "reservation_id":"reservation" }),
            })
        });
        assert!(result.is_err());
    }
}
