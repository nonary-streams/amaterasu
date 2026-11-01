//! One-use first-host initialization after trusted host and State setup.
use super::*;
use amaterasu_module_sdk::effect_authority::HostConfigurationTransactionStatus;
use amaterasu_module_sdk::state::{FEEDBACK_BOOTSTRAP_KEY, FeedbackBootstrapClaim};
use sha2::{Digest, Sha256};

enum Stage {
    Start,
    Loading(u64),
    Saving {
        request: u64,
        configuration_revision: u64,
        certificate_hash: String,
    },
    Checking {
        request: u64,
        configuration_revision: u64,
        certificate_hash: String,
    },
    Done,
    Failed,
}

pub(super) struct Bootstrap {
    claim: FeedbackBootstrapClaim,
    stage: Stage,
}

impl Bootstrap {
    pub(super) fn new(claim: FeedbackBootstrapClaim) -> Self {
        Self {
            claim,
            stage: Stage::Start,
        }
    }
}

fn submit(
    send: &mut impl FnMut(Command) -> Transport,
    service: &str,
    payload: Value,
    operation_id: &str,
) -> Option<u64> {
    match send(Command::Request {
        service: service.into(),
        payload,
        timeout_ms: 30_000,
        operation_id: operation_id.into(),
        parent: None,
    }) {
        Ok(Response::Accepted { id }) => Some(id),
        _ => None,
    }
}

fn take(send: &mut impl FnMut(Command) -> Transport, request: u64) -> Option<Result<Value, ()>> {
    match send(Command::TakeResult { request }) {
        Ok(Response::Pending) => None,
        Ok(Response::Result { payload }) => {
            let _ = send(Command::ReleaseResult { request });
            Some(Ok(payload))
        }
        _ => Some(Err(())),
    }
}

fn identity(pipeline: &Pipeline) -> Option<(u64, String)> {
    let identity = pipeline.configured_identity()?;
    let hash = Sha256::digest(identity.certificate_pem.as_bytes());
    Some((
        identity.provider_revision,
        hash.iter().map(|byte| format!("{byte:02x}")).collect(),
    ))
}

impl Pipeline {
    pub(super) fn progress_feedback_bootstrap(
        &mut self,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> ModuleResult<()> {
        let Some(mut bootstrap) = self.feedback_bootstrap.take() else {
            return Ok(());
        };
        let stage = std::mem::replace(&mut bootstrap.stage, Stage::Failed);
        bootstrap.stage = match stage {
            Stage::Start => {
                let matches_owner = self.live_feedback_authorized
                    && self.effect_authority.as_ref().is_some_and(|authority| {
                        authority.effect_owner == bootstrap.claim.effect_owner
                            && authority.configuration_owner.is_some()
                    });
                if !matches_owner || bootstrap.claim.version != 1 {
                    Stage::Failed
                } else if !self.configuration_reconciled
                    || self.configuration_hold.is_some()
                    || identity(self).is_none()
                {
                    Stage::Start
                } else {
                    match submit(
                        send,
                        STATE_SERVICE,
                        json!({"action":"get","key":FEEDBACK_BOOTSTRAP_KEY}),
                        "feedback-bootstrap-load",
                    ) {
                        Some(request) => Stage::Loading(request),
                        None => Stage::Failed,
                    }
                }
            }
            Stage::Loading(request) => match take(send, request) {
                None => Stage::Loading(request),
                Some(Ok(payload))
                    if payload["ok"] == true
                        && payload["revision"] == 1
                        && payload["found"] == true
                        && payload["value"]["phase"] == "prepared"
                        && payload["value"]["claim"] == json!(bootstrap.claim) =>
                {
                    if let Some((configuration_revision, certificate_hash)) = identity(self) {
                        let value = json!({"claim":bootstrap.claim,"phase":"active",
                                "configuration_revision":configuration_revision,"certificate_sha256":certificate_hash});
                        match submit(
                            send,
                            STATE_SERVICE,
                            json!({"action":"set","key":FEEDBACK_BOOTSTRAP_KEY,
                                "value":value,"expected_revision":1}),
                            "feedback-bootstrap-commit",
                        ) {
                            Some(request) => Stage::Saving {
                                request,
                                configuration_revision,
                                certificate_hash,
                            },
                            None => Stage::Failed,
                        }
                    } else {
                        Stage::Failed
                    }
                }
                _ => Stage::Failed,
            },
            Stage::Saving {
                request,
                configuration_revision,
                certificate_hash,
            } => match take(send, request) {
                None => Stage::Saving {
                    request,
                    configuration_revision,
                    certificate_hash,
                },
                Some(Ok(payload)) if payload["ok"] == true && payload["revision"] == 2 => {
                    match submit(
                        send,
                        HOST_CONFIGURATION_SERVICE,
                        json!({"action":"transaction_status"}),
                        "feedback-bootstrap-status",
                    ) {
                        Some(request) => Stage::Checking {
                            request,
                            configuration_revision,
                            certificate_hash,
                        },
                        None => Stage::Failed,
                    }
                }
                _ => Stage::Failed,
            },
            Stage::Checking {
                request,
                configuration_revision,
                certificate_hash,
            } => match take(send, request) {
                None => Stage::Checking {
                    request,
                    configuration_revision,
                    certificate_hash,
                },
                Some(Ok(payload)) => {
                    let status =
                        serde_json::from_value::<HostConfigurationTransactionStatus>(payload);
                    let stable = identity(self) == Some((configuration_revision, certificate_hash))
                        && self.configuration_reconciled
                        && self.configuration_hold.is_none()
                        && status.is_ok_and(|status| {
                            status.valid()
                                && status.configuration_revision == 0
                                && status.state_revision == 0
                                && status.configuration_revision == configuration_revision
                                && status.transaction.is_none()
                                && status.terminal_receipt.is_none()
                        });
                    if stable
                        && self.effect_gate.as_mut().is_some_and(|gate| {
                            gate.startup_recovery_required()
                                && gate.recover(&bootstrap.claim.claim).is_ok()
                        })
                    {
                        Stage::Done
                    } else {
                        Stage::Failed
                    }
                }
                _ => Stage::Failed,
            },
            Stage::Done => Stage::Done,
            Stage::Failed => Stage::Failed,
        };
        self.feedback_bootstrap = Some(bootstrap);
        Ok(())
    }
}
