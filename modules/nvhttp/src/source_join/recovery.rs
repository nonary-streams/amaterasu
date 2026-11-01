//! Reconcile an uncertain bind without assuming cancellation undid it.
use amaterasu_module_sdk::{
    Command, ModuleResult, Response,
    effect_source_retirement::{Receipt, Request as RetirementRequest, State},
    nvhttp_source::BindFeedbackSource,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PAIRING: &str = "amaterasu.pairing-effects/1";

enum Phase {
    FenceSubmit,
    FenceWait(u64),
    FenceRetryRelease(u64),
    FenceRelease(u64, Value),
    DiscoverSubmit,
    DiscoverWait(u64),
    DiscoverRelease(u64, Value),
    Fenced,
    Failed,
    LookupSubmit(u64),
    LookupWait(u64, u64),
    LookupRelease(u64, u64, Value),
    StatusSubmit(u64),
    StatusWait(u64, u64),
    StatusRelease(u64, u64, Value),
}

pub struct Recovery {
    request: BindFeedbackSource,
    original_request: u64,
    operation_id: String,
    phase: Phase,
}

pub enum Outcome {
    Pending,
    FenceSettled {
        original_request: u64,
        operation_id: String,
        fence_confirmed: bool,
    },
    Reply {
        request: u64,
        payload: Value,
        retired: bool,
    },
}

impl Recovery {
    pub fn new(original_request: u64, request: BindFeedbackSource) -> Self {
        let operation_id = request.retirement_operation_id();
        Self {
            request,
            original_request,
            operation_id,
            phase: Phase::FenceSubmit,
        }
    }

    pub fn begin_lookup(
        &mut self,
        request_id: u64,
        expected: &BindFeedbackSource,
    ) -> Result<(), &'static str> {
        if &self.request != expected {
            return Err("source_retirement_conflict");
        }
        if !matches!(self.phase, Phase::Fenced | Phase::Failed) {
            return Err("source_fence_pending");
        }
        self.phase = Phase::LookupSubmit(request_id);
        Ok(())
    }

    pub fn advance(&mut self, mut send: impl FnMut(Command) -> ModuleResult<Response>) -> Outcome {
        let op = self.operation_id.clone();
        match &mut self.phase {
            Phase::FenceSubmit => {
                let payload = serde_json::to_value(RetirementRequest::FenceSource {
                    epoch: self.request.authority_epoch.clone(),
                    source: self.request.source.clone(),
                    scope: self.request.scope.clone(),
                    operation_id: op.clone(),
                })
                .expect("retirement request is JSON");
                match send(Command::Request {
                    service: PAIRING.into(),
                    payload,
                    timeout_ms: 5000,
                    operation_id: broker_operation_id(&self.request, &op, "fence", None),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => self.phase = Phase::FenceWait(id),
                    Ok(Response::Error { .. }) => {
                        self.phase = Phase::Failed;
                        return Outcome::FenceSettled {
                            original_request: self.original_request,
                            operation_id: op,
                            fence_confirmed: false,
                        };
                    }
                    _ => {} // Retry the exact admission until the broker identifies it.
                }
            }
            Phase::FenceWait(id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => self.phase = Phase::FenceRelease(*id, payload),
                Ok(Response::Error { .. }) => self.phase = Phase::FenceRetryRelease(*id),
                _ => {}
            },
            Phase::FenceRetryRelease(id) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    self.phase = Phase::FenceSubmit;
                }
            }
            Phase::FenceRelease(id, payload) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    if payload["code"] == "source_retirement_conflict" {
                        self.phase = Phase::DiscoverSubmit;
                        return Outcome::Pending;
                    }
                    let receipt = serde_json::from_value::<Receipt>(payload.clone()).ok();
                    let fence_confirmed = receipt.as_ref().is_some_and(|value| {
                        value.matches(
                            &self.request.authority_epoch,
                            &self.request.source,
                            &self.request.scope,
                            &op,
                        )
                    });
                    if fence_confirmed {
                        self.phase = Phase::Fenced;
                    } else {
                        self.phase = Phase::Failed;
                    }
                    return Outcome::FenceSettled {
                        original_request: self.original_request,
                        operation_id: op,
                        fence_confirmed,
                    };
                }
            }
            Phase::DiscoverSubmit => {
                let payload =
                    serde_json::to_value(RetirementRequest::LookupCurrentSourceRetirement {
                        epoch: self.request.authority_epoch.clone(),
                        source: self.request.source.clone(),
                        scope: self.request.scope.clone(),
                    })
                    .expect("retirement query is JSON");
                match send(Command::Request {
                    service: PAIRING.into(),
                    payload,
                    timeout_ms: 5000,
                    operation_id: broker_operation_id(&self.request, &op, "discover", None),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => self.phase = Phase::DiscoverWait(id),
                    Ok(Response::Error { .. }) => {
                        self.phase = Phase::Failed;
                        return Outcome::FenceSettled {
                            original_request: self.original_request,
                            operation_id: op,
                            fence_confirmed: false,
                        };
                    }
                    _ => {}
                }
            }
            Phase::DiscoverWait(id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => {
                    self.phase = Phase::DiscoverRelease(*id, payload)
                }
                Ok(Response::Error { .. }) => self.phase = Phase::DiscoverRelease(*id, json!({})),
                _ => {}
            },
            Phase::DiscoverRelease(id, payload) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    let receipt = serde_json::from_value::<Receipt>(payload.clone()).ok();
                    let found = receipt.filter(|value| {
                        value.matches_source(
                            &self.request.authority_epoch,
                            &self.request.source,
                            &self.request.scope,
                        )
                    });
                    let fence_confirmed = found.is_some();
                    if let Some(receipt) = found {
                        self.operation_id = receipt.operation_id;
                        self.phase = Phase::Fenced;
                    } else {
                        self.phase = Phase::Failed;
                    }
                    return Outcome::FenceSettled {
                        original_request: self.original_request,
                        operation_id: self.operation_id.clone(),
                        fence_confirmed,
                    };
                }
            }
            Phase::Fenced | Phase::Failed => {}
            Phase::LookupSubmit(parent) => {
                if !matches!(
                    send(Command::CheckRequest { request: *parent }),
                    Ok(Response::Ok)
                ) {
                    self.phase = Phase::Fenced;
                    return Outcome::Pending;
                }
                let payload = serde_json::to_value(RetirementRequest::LookupSourceRetirement {
                    epoch: self.request.authority_epoch.clone(),
                    source: self.request.source.clone(),
                    scope: self.request.scope.clone(),
                    operation_id: op.clone(),
                })
                .expect("retirement lookup is JSON");
                match send(Command::Request {
                    service: PAIRING.into(),
                    payload,
                    timeout_ms: 5000,
                    operation_id: broker_operation_id(&self.request, &op, "lookup", Some(*parent)),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => self.phase = Phase::LookupWait(*parent, id),
                    Ok(Response::Error { .. }) => {
                        let request = *parent;
                        self.phase = Phase::Fenced;
                        return Outcome::Reply {
                            request,
                            payload: json!({"ok":false,"code":"authority_unavailable"}),
                            retired: false,
                        };
                    }
                    _ => {}
                }
            }
            Phase::LookupWait(parent, id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => {
                    self.phase = Phase::LookupRelease(*parent, *id, payload)
                }
                Ok(Response::Error { .. }) => {
                    self.phase = Phase::LookupRelease(*parent, *id, json!({"ok":false}))
                }
                _ => {}
            },
            Phase::LookupRelease(parent, id, payload) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    if payload["code"] == "stale_epoch" {
                        self.phase = Phase::StatusSubmit(*parent);
                        return Outcome::Pending;
                    }
                    let receipt = serde_json::from_value::<Receipt>(payload.clone()).ok();
                    let retired = receipt.as_ref().is_some_and(|value| {
                        value.matches(
                            &self.request.authority_epoch,
                            &self.request.source,
                            &self.request.scope,
                            &op,
                        ) && value.state == State::Retired
                    });
                    let response = if retired {
                        json!({"ok":true,"state":"retired","epoch":self.request.authority_epoch,"source":self.request.source,"scope":self.request.scope,"operation_id":op})
                    } else {
                        json!({"ok":false,"code":"source_retirement_pending"})
                    };
                    let request = *parent;
                    self.phase = Phase::Fenced;
                    return Outcome::Reply {
                        request,
                        payload: response,
                        retired,
                    };
                }
            }
            Phase::StatusSubmit(parent) => {
                if !matches!(
                    send(Command::CheckRequest { request: *parent }),
                    Ok(Response::Ok)
                ) {
                    self.phase = Phase::Fenced;
                    return Outcome::Pending;
                }
                match send(Command::Request {
                    service: PAIRING.into(),
                    payload: json!({"action":"status"}),
                    timeout_ms: 5000,
                    operation_id: broker_operation_id(
                        &self.request,
                        &op,
                        "restart-status",
                        Some(*parent),
                    ),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => self.phase = Phase::StatusWait(*parent, id),
                    Ok(Response::Error { .. }) => {
                        let request = *parent;
                        self.phase = Phase::Fenced;
                        return Outcome::Reply {
                            request,
                            payload: json!({"ok":false,"code":"authority_unavailable"}),
                            retired: false,
                        };
                    }
                    _ => {}
                }
            }
            Phase::StatusWait(parent, id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => {
                    self.phase = Phase::StatusRelease(*parent, *id, payload)
                }
                Ok(Response::Error { .. }) => {
                    self.phase = Phase::StatusRelease(*parent, *id, json!({}))
                }
                _ => {}
            },
            Phase::StatusRelease(parent, id, payload) => {
                if released(send(Command::ReleaseResult { request: *id })) {
                    let retired = super::startup::clean_status(payload)
                        && payload["epoch"]
                            .as_str()
                            .is_some_and(|epoch| epoch != self.request.authority_epoch);
                    let response = if retired {
                        json!({"ok":true,"state":"retired","epoch":self.request.authority_epoch,
                            "current_epoch":payload["epoch"],"source":self.request.source,"scope":self.request.scope,
                            "operation_id":op,"reconciled_by":"authority_epoch"})
                    } else {
                        json!({"ok":false,"code":"source_retirement_pending"})
                    };
                    let request = *parent;
                    self.phase = Phase::Fenced;
                    return Outcome::Reply {
                        request,
                        payload: response,
                        retired,
                    };
                }
            }
        }
        Outcome::Pending
    }
}

// Owner IDs have a wider grammar than broker operation IDs. Bind the broker
// identity to the full source, exact owner ID, phase, and request instead of
// copying an owner-controlled string into the broker namespace.
fn broker_operation_id(
    request: &BindFeedbackSource,
    owner_operation: &str,
    phase: &str,
    parent: Option<u64>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"amaterasu.nvhttp.recovery-broker-operation/v1\0");
    for field in [
        request.retirement_operation_id(),
        owner_operation.into(),
        phase.into(),
    ] {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field.as_bytes());
    }
    match parent {
        Some(id) => {
            digest.update([1]);
            digest.update(id.to_be_bytes());
        }
        None => digest.update([0]),
    }
    format!("nvhttp.recovery.{:x}", digest.finalize())
}

fn released(result: ModuleResult<Response>) -> bool {
    match result {
        Ok(Response::Ok) => true,
        Ok(Response::Error { code, .. }) if code == "not_found" => true,
        _ => false,
    }
}
