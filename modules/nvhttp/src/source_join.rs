//! NVHTTP's authenticated lifecycle-to-pairing feedback-source join.
//!
//! The effect owner supplies its selected stream scope as a trusted assertion;
//! lifecycle supplies the source label, and NVHTTP obtains the certificate only
//! from its private paired-session record. Pairing still gates production binding.
use amaterasu_module_sdk::{
    Command, ModuleResult, Response, effect_authority::Source,
    nvhttp_source::BindFeedbackSource as Request,
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub mod recovery;
pub mod startup;

const LIFECYCLE: &str = "amaterasu.session-lifecycle/1";
const PAIRING: &str = "amaterasu.pairing-effects/1";

pub struct Evidence {
    pub session_target: Value,
    pub certificate: Vec<u8>,
}

enum Stage {
    ResolveSubmit,
    ResolveWait(u64),
    ResolveRelease(u64, Value),
    BindSubmit,
    BindWait(u64),
    BindRelease(u64, Value),
}

pub struct Join {
    request_id: u64,
    request: Request,
    input_consumer: String,
    evidence: Evidence,
    deadline: Instant,
    stage: Stage,
}

pub enum Outcome {
    Pending,
    Done(Value),
    /// A pairing bind may have taken effect; no further join is permitted until
    /// an owner-coordinated retirement/recovery protocol clears that uncertainty.
    RecoveryRequired,
}

impl Join {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn into_recovery(self) -> recovery::Recovery {
        recovery::Recovery::new(self.request_id, self.request)
    }
    pub fn start(
        request_id: u64,
        request: Request,
        input_consumer: String,
        evidence: Evidence,
        remaining_ms: u64,
    ) -> Result<Self, &'static str> {
        if !request.valid() || remaining_ms == 0 {
            return Err("invalid_source_join");
        }
        Ok(Self {
            request_id,
            request,
            input_consumer,
            evidence,
            deadline: Instant::now() + Duration::from_millis(remaining_ms.min(30_000)),
            stage: Stage::ResolveSubmit,
        })
    }

    /// `evidence` must resolve the current live record and check current paired
    /// View permission on each invocation. Neither incoming JSON nor a cached
    /// lifecycle label may manufacture certificate ownership.
    pub fn advance(
        &mut self,
        mut evidence: impl FnMut(&Source) -> Option<Evidence>,
        mut send: impl FnMut(Command) -> ModuleResult<Response>,
    ) -> ModuleResult<Outcome> {
        let bind_may_exist = matches!(self.stage, Stage::BindWait(_) | Stage::BindRelease(_, _));
        let active = Instant::now() < self.deadline
            && matches!(
                send(Command::CheckRequest {
                    request: self.request_id
                }),
                Ok(Response::Ok)
            );
        let current = evidence(&self.request.source).filter(|current| {
            current.session_target == self.evidence.session_target
                && current.certificate == self.evidence.certificate
        });
        if !active || current.is_none() {
            return Ok(if bind_may_exist {
                Outcome::RecoveryRequired
            } else {
                Outcome::Done(json!({"ok":false,"code":"source_denied"}))
            });
        }
        match &mut self.stage {
            Stage::ResolveSubmit => {
                let payload = json!({"operation":"resolve_effect_sources","target":self.evidence.session_target});
                match send(Command::Request {
                    service: LIFECYCLE.into(),
                    payload,
                    timeout_ms: 5000,
                    operation_id: format!("nvhttp.source.{}.resolve", self.request_id),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => self.stage = Stage::ResolveWait(id),
                    Ok(Response::Error { .. }) => {
                        return Ok(Outcome::Done(
                            json!({"ok":false,"code":"lifecycle_unavailable"}),
                        ));
                    }
                    _ => {} // Retry the exact admission to resolve a lost reply.
                }
            }
            Stage::ResolveWait(id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => {
                    self.stage = Stage::ResolveRelease(*id, payload)
                }
                _ => {
                    return Ok(Outcome::Done(
                        json!({"ok":false,"code":"lifecycle_unavailable"}),
                    ));
                }
            },
            Stage::ResolveRelease(id, payload) => {
                if matches!(
                    send(Command::ReleaseResult { request: *id }),
                    Ok(Response::Ok)
                ) {
                    if !matches_lifecycle(
                        &self.request,
                        &self.evidence.session_target,
                        &self.input_consumer,
                        payload,
                    ) {
                        return Ok(Outcome::Done(json!({"ok":false,"code":"source_denied"})));
                    }
                    self.stage = Stage::BindSubmit;
                }
            }
            Stage::BindSubmit => {
                let certificate_pem = pem_rfc7468::encode_string(
                    "CERTIFICATE",
                    pem_rfc7468::LineEnding::LF,
                    &self.evidence.certificate,
                )
                .map_err(|_| "invalid_peer_certificate")?;
                let payload = json!({"action":"bind_stream","epoch":self.request.authority_epoch,
                    "certificate_pem":certificate_pem,"source":self.request.source,"scope":self.request.scope});
                match send(Command::Request {
                    service: PAIRING.into(),
                    payload,
                    timeout_ms: 5000,
                    operation_id: format!("nvhttp.source.{}.bind", self.request_id),
                    parent: None,
                }) {
                    Ok(Response::Accepted { id }) => self.stage = Stage::BindWait(id),
                    Ok(Response::Error { .. }) => {
                        return Ok(Outcome::Done(
                            json!({"ok":false,"code":"authority_unavailable"}),
                        ));
                    }
                    _ => return Ok(Outcome::RecoveryRequired),
                }
            }
            Stage::BindWait(id) => match send(Command::TakeResult { request: *id }) {
                Ok(Response::Pending) => {}
                Ok(Response::Result { payload }) => self.stage = Stage::BindRelease(*id, payload),
                _ => return Ok(Outcome::RecoveryRequired),
            },
            Stage::BindRelease(id, payload) => {
                if matches!(
                    send(Command::ReleaseResult { request: *id }),
                    Ok(Response::Ok)
                ) {
                    if payload["ok"] == false && payload["code"].is_string() {
                        return Ok(Outcome::Done(json!({"ok":false,"code":"source_denied"})));
                    }
                    if payload["ok"] != true || payload["epoch"] != self.request.authority_epoch {
                        return Ok(Outcome::RecoveryRequired);
                    }
                    return Ok(Outcome::Done(
                        json!({"ok":true,"epoch":self.request.authority_epoch,
                        "source":self.request.source,"scope":self.request.scope}),
                    ));
                }
            }
        }
        Ok(Outcome::Pending)
    }
}

fn matches_lifecycle(
    request: &Request,
    session_target: &Value,
    input_consumer: &str,
    value: &Value,
) -> bool {
    if value["status"] != "succeeded" || value["target"] != *session_target {
        return false;
    }
    let Some(sources) = value["sources"]
        .as_array()
        .filter(|items| items.len() <= 64)
    else {
        return false;
    };
    let source = &request.source;
    let target = json!({"kind":"stream","host_epoch":source.host_epoch,"session_id":source.session_id,
            "stream_id":source.stream_id,"generation":source.stream_generation});
    let label = json!({"version":source.version,"id":source.id,"generation":source.generation});
    let mut matching = sources.iter().filter(|item| item["source"] == label);
    let Some(item) = matching.next() else {
        return false;
    };
    matching.next().is_none()
        && item["target"] == target
        && item["input_consumer"] == input_consumer
        && item["input_boot_epoch"] == request.input_boot_epoch
}

#[cfg(test)]
mod tests {
    use super::*;
    use amaterasu_contracts::{Grants, RuntimeConfig};
    use amaterasu_runtime::Broker;

    fn request() -> Request {
        serde_json::from_value(json!({"authority_epoch":"a".repeat(32),
            "source":{"version":1,"id":"b".repeat(32),"generation":3,"host_epoch":"host",
                "session_id":"session","stream_id":"stream","stream_generation":3,
                "device":"controller","device_generation":2,"effect_owner_epoch":"owner"},
            "scope":{"capabilities":[{"schema":"speaker.pcm","version":1,"direction":"to_client"}],
                "controller_slot":1,"transport_service":"amaterasu.control-transport/1",
                "transport_stream":7,"transport_epoch":"c".repeat(32)},"input_boot_epoch":"input-boot"})).unwrap()
    }
    fn evidence() -> Evidence {
        Evidence {
            session_target: json!({"kind":"session","host_epoch":"host","session_id":"session","generation":4}),
            certificate: vec![1, 2, 3],
        }
    }
    fn lifecycle_reply() -> Value {
        json!({"status":"succeeded","target":evidence().session_target,"sources":[{
            "source":{"version":1,"id":"b".repeat(32),"generation":3},
            "target":{"kind":"stream","host_epoch":"host","session_id":"session","stream_id":"stream","generation":3},
            "input_consumer":"input","input_boot_epoch":"input-boot"}]})
    }
    fn broker() -> (Broker, u64) {
        let mut config = RuntimeConfig::default();
        for (name, grants) in [
            (
                "owner",
                Grants {
                    call: ["amaterasu.nvhttp/1".into()].into(),
                    ..Default::default()
                },
            ),
            (
                "amaterasu.nvhttp",
                Grants {
                    provide: ["amaterasu.nvhttp/1".into()].into(),
                    call: [LIFECYCLE.into(), PAIRING.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "lifecycle",
                Grants {
                    provide: [LIFECYCLE.into()].into(),
                    ..Default::default()
                },
            ),
            (
                "pairing",
                Grants {
                    provide: [PAIRING.into()].into(),
                    ..Default::default()
                },
            ),
        ] {
            config.principals.insert(name.into(), grants);
        }
        let mut broker = Broker::new(config).unwrap();
        for (name, service) in [
            ("amaterasu.nvhttp", "amaterasu.nvhttp/1"),
            ("lifecycle", LIFECYCLE),
            ("pairing", PAIRING),
        ] {
            assert!(matches!(
                broker.dispatch(
                    name,
                    Command::Register {
                        service: service.into()
                    }
                ),
                Response::Ok
            ));
        }
        let Response::Accepted { id } = broker.dispatch(
            "owner",
            Command::Request {
                service: "amaterasu.nvhttp/1".into(),
                payload: json!({}),
                timeout_ms: 30000,
                operation_id: "owner-join".into(),
                parent: None,
            },
        ) else {
            panic!("not accepted")
        };
        assert!(matches!(
            broker.dispatch("amaterasu.nvhttp", Command::Poll {}),
            Response::Request { .. }
        ));
        (broker, id)
    }
    fn tick(join: &mut Join, broker: &mut Broker, valid: bool) -> Outcome {
        join.advance(
            |_| valid.then(evidence),
            |command| Ok(broker.dispatch("amaterasu.nvhttp", command)),
        )
        .unwrap()
    }
    fn answer(broker: &mut Broker, provider: &str, payload: Value) -> Value {
        let Response::Request {
            id,
            caller,
            initiator,
            payload: request,
            ..
        } = broker.dispatch(provider, Command::Poll {})
        else {
            panic!("no child request")
        };
        assert_eq!(caller, "amaterasu.nvhttp");
        assert_eq!(initiator, "amaterasu.nvhttp");
        assert!(matches!(
            broker.dispatch(
                provider,
                Command::Complete {
                    request: id,
                    payload
                }
            ),
            Response::Ok
        ));
        request
    }
    fn resolve(join: &mut Join, broker: &mut Broker, reply: Value) {
        assert!(matches!(tick(join, broker, true), Outcome::Pending));
        let sent = answer(broker, "lifecycle", reply);
        assert_eq!(sent["operation"], "resolve_effect_sources");
        assert!(matches!(tick(join, broker, true), Outcome::Pending));
    }

    #[test]
    fn real_broker_joins_only_the_exact_lifecycle_source_and_private_peer() {
        let (mut broker, id) = broker();
        let mut join = Join::start(id, request(), "input".into(), evidence(), 30000).unwrap();
        resolve(&mut join, &mut broker, lifecycle_reply());
        assert!(matches!(
            tick(&mut join, &mut broker, true),
            Outcome::Pending
        ));
        assert!(matches!(
            tick(&mut join, &mut broker, true),
            Outcome::Pending
        ));
        let sent = answer(
            &mut broker,
            "pairing",
            json!({"ok":true,"epoch":"a".repeat(32)}),
        );
        assert_eq!(sent["action"], "bind_stream");
        let (label, der) =
            pem_rfc7468::decode_vec(sent["certificate_pem"].as_str().unwrap().as_bytes()).unwrap();
        assert_eq!(
            (label, der.as_slice()),
            ("CERTIFICATE", [1, 2, 3].as_slice())
        );
        assert!(matches!(
            tick(&mut join, &mut broker, true),
            Outcome::Pending
        ));
        let Outcome::Done(result) = tick(&mut join, &mut broker, true) else {
            panic!("join did not finish")
        };
        assert_eq!(result["ok"], true);
        assert!(result.get("certificate_pem").is_none());
        assert_eq!(result["source"], json!(request().source));
        assert_eq!(result["scope"], json!(request().scope));
    }

    #[test]
    fn real_broker_denies_cross_session_boot_and_ambiguous_sources() {
        for attack in [
            "session",
            "boot",
            "duplicate",
            "source",
            "input",
            "generation",
        ] {
            let (mut broker, id) = broker();
            let mut join = Join::start(id, request(), "input".into(), evidence(), 30000).unwrap();
            let mut reply = lifecycle_reply();
            match attack {
                "session" => reply["sources"][0]["target"]["session_id"] = json!("other"),
                "boot" => reply["sources"][0]["input_boot_epoch"] = json!("old"),
                "source" => reply["sources"][0]["source"]["id"] = json!("d".repeat(32)),
                "input" => reply["sources"][0]["input_consumer"] = json!("foreign"),
                "generation" => reply["sources"][0]["target"]["generation"] = json!(4),
                _ => {
                    let duplicate = reply["sources"][0].clone();
                    reply["sources"].as_array_mut().unwrap().push(duplicate);
                }
            }
            resolve(&mut join, &mut broker, reply);
            let Outcome::Done(result) = tick(&mut join, &mut broker, true) else {
                panic!("{attack}")
            };
            assert_eq!(result["code"], "source_denied");
            assert!(matches!(
                broker.dispatch("pairing", Command::Poll {}),
                Response::Pending
            ));
        }
    }

    #[test]
    fn revoked_permission_or_cancelled_parent_cannot_finish_a_bound_join() {
        for cancelled in [false, true] {
            let (mut broker, id) = broker();
            let mut join = Join::start(id, request(), "input".into(), evidence(), 30000).unwrap();
            resolve(&mut join, &mut broker, lifecycle_reply());
            assert!(matches!(
                tick(&mut join, &mut broker, true),
                Outcome::Pending
            ));
            assert!(matches!(
                tick(&mut join, &mut broker, true),
                Outcome::Pending
            ));
            let _ = answer(
                &mut broker,
                "pairing",
                json!({"ok":true,"epoch":"a".repeat(32)}),
            );
            if cancelled {
                assert!(matches!(
                    broker.dispatch("owner", Command::Cancel { request: id }),
                    Response::Ok
                ));
            }
            assert!(matches!(
                tick(&mut join, &mut broker, cancelled),
                Outcome::RecoveryRequired
            ));
        }
    }

    #[test]
    fn selected_owner_must_be_the_direct_broker_initiator() {
        let selection = crate::settings::EffectSourceJoinSettings {
            effect_owner: "stream-owner".into(),
            input_consumer: "input".into(),
        };
        assert!(selection.admits("stream-owner", "stream-owner"));
        assert!(!selection.admits("stream-owner", "foreign"));
        assert!(!selection.admits("foreign", "stream-owner"));
    }

    #[test]
    fn sdk_request_matches_the_production_control_operation() {
        let request = request();
        assert!(request.valid());
        let payload = request.payload();
        assert!(payload.get("certificate_pem").is_none());
        assert!(matches!(
            serde_json::from_value::<crate::Operation>(payload),
            Ok(crate::Operation::BindFeedbackSource { .. })
        ));
        let reply = json!({"ok":true,"epoch":request.authority_epoch,"source":request.source,"scope":request.scope});
        assert!(request.accepts_confirmation(&reply));
        let mut forged = reply;
        forged["scope"]["controller_slot"] = json!(99);
        assert!(!request.accepts_confirmation(&forged));
        let mut stale = request;
        stale.authority_epoch = "BAD".into();
        assert!(!stale.valid());
    }

    #[test]
    fn retirement_operation_id_distinguishes_controller_incarnations() {
        let first = request();
        let mut second = first.clone();
        second.source.device = "other-controller".into();
        second.source.device_generation += 1;
        assert_eq!(first.source.id, second.source.id);
        assert_eq!(
            first.retirement_operation_id(),
            "nvhttp-source-b699c0bc51c41c2adeb6082a39ceeedc701a72fc5b25bf0ddfee1cd4904f9513"
        );
        assert_ne!(
            first.retirement_operation_id(),
            second.retirement_operation_id()
        );
        assert_eq!(
            first.retirement_operation_id(),
            first.clone().retirement_operation_id()
        );
        assert!(first.retirement_operation_id().len() <= 128);
    }

    #[test]
    fn revoked_peer_before_binding_never_sends_a_pairing_request() {
        let (mut broker, id) = broker();
        let mut join = Join::start(id, request(), "input".into(), evidence(), 30000).unwrap();
        resolve(&mut join, &mut broker, lifecycle_reply());
        let Outcome::Done(result) = tick(&mut join, &mut broker, false) else {
            panic!("expected denial")
        };
        assert_eq!(result["code"], "source_denied");
        assert!(matches!(
            broker.dispatch("pairing", Command::Poll {}),
            Response::Pending
        ));
    }

    #[test]
    fn malformed_pairing_confirmation_requires_recovery() {
        let (mut broker, id) = broker();
        let mut join = Join::start(id, request(), "input".into(), evidence(), 30000).unwrap();
        resolve(&mut join, &mut broker, lifecycle_reply());
        assert!(matches!(
            tick(&mut join, &mut broker, true),
            Outcome::Pending
        ));
        assert!(matches!(
            tick(&mut join, &mut broker, true),
            Outcome::Pending
        ));
        let _ = answer(&mut broker, "pairing", json!({"ok":true,"epoch":"wrong"}));
        assert!(matches!(
            tick(&mut join, &mut broker, true),
            Outcome::Pending
        ));
        assert!(matches!(
            tick(&mut join, &mut broker, true),
            Outcome::RecoveryRequired
        ));
    }

    #[test]
    fn uncertain_bind_is_fenced_and_reconciled_only_after_owner_retirement() {
        use recovery::{Outcome as RecoveryOutcome, Recovery};
        let (mut broker, original) = broker();
        let request = request();
        let operation = request.retirement_operation_id();
        let mut recovery = Recovery::new(original, request.clone());
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let fence_receipt = json!({"ok":true,"epoch":request.authority_epoch,"source":request.source,"scope":request.scope,"operation_id":operation,"state":"fenced"});
        let fence = answer(&mut broker, "pairing", fence_receipt.clone());
        assert_eq!(fence["action"], "fence_source");
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::FenceSettled { .. }
        ));
        let mut forged = request.clone();
        forged.source.session_id = "other".into();
        assert_eq!(
            recovery.begin_lookup(123, &forged),
            Err("source_retirement_conflict")
        );
        let Response::Accepted { id } = broker.dispatch(
            "owner",
            Command::Request {
                service: "amaterasu.nvhttp/1".into(),
                payload: request.reconciliation_payload(),
                timeout_ms: 30000,
                operation_id: "reconcile-1".into(),
                parent: None,
            },
        ) else {
            panic!("not accepted")
        };
        assert!(matches!(
            broker.dispatch("amaterasu.nvhttp", Command::Poll {}),
            Response::Request { .. }
        ));
        recovery.begin_lookup(id, &request).unwrap();
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        assert_eq!(
            answer(&mut broker, "pairing", fence_receipt.clone())["action"],
            "lookup_source_retirement"
        );
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let RecoveryOutcome::Reply {
            retired: false,
            payload,
            ..
        } = recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command)))
        else {
            panic!("must remain fenced")
        };
        assert_eq!(payload["code"], "source_retirement_pending");
        let Response::Accepted { id } = broker.dispatch(
            "owner",
            Command::Request {
                service: "amaterasu.nvhttp/1".into(),
                payload: request.reconciliation_payload(),
                timeout_ms: 30000,
                operation_id: "reconcile-2".into(),
                parent: None,
            },
        ) else {
            panic!("not accepted")
        };
        assert!(matches!(
            broker.dispatch("amaterasu.nvhttp", Command::Poll {}),
            Response::Request { .. }
        ));
        recovery.begin_lookup(id, &request).unwrap();
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let mut retired = fence_receipt;
        retired["state"] = json!("retired");
        let _ = answer(&mut broker, "pairing", retired);
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let RecoveryOutcome::Reply {
            retired: true,
            payload,
            ..
        } = recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command)))
        else {
            panic!("retirement unconfirmed")
        };
        assert!(request.accepts_retirement_confirmation(&payload, &operation));
    }

    #[test]
    fn uncertain_bind_reconciles_after_pairing_restart_only_with_drained_new_epoch() {
        use recovery::{Outcome as RecoveryOutcome, Recovery};
        let (mut broker, original) = broker();
        let request = request();
        let op = request.retirement_operation_id();
        let mut recovery = Recovery::new(original, request.clone());
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let _ = answer(
            &mut broker,
            "pairing",
            json!({"ok":true,"epoch":request.authority_epoch,
            "source":request.source,"scope":request.scope,"operation_id":op,"state":"fenced"}),
        );
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::FenceSettled { .. }
        ));
        let Response::Accepted { id } = broker.dispatch(
            "owner",
            Command::Request {
                service: "amaterasu.nvhttp/1".into(),
                payload: request.reconciliation_payload(),
                timeout_ms: 30000,
                operation_id: "restart-reconcile".into(),
                parent: None,
            },
        ) else {
            panic!("not accepted")
        };
        assert!(matches!(
            broker.dispatch("amaterasu.nvhttp", Command::Poll {}),
            Response::Request { .. }
        ));
        recovery.begin_lookup(id, &request).unwrap();
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let _ = answer(
            &mut broker,
            "pairing",
            json!({"ok":false,"code":"stale_epoch"}),
        );
        for _ in 0..3 {
            assert!(matches!(
                recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
                RecoveryOutcome::Pending
            ));
        }
        let _ = answer(
            &mut broker,
            "pairing",
            json!({"ok":true,"epoch":"d".repeat(32),"owner_epoch":"new-owner",
            "startup_recovery_required":false,"fenced":false,"drain_required":false,
            "in_flight":0,"bound_sources":0,"pending_source_retirements":0}),
        );
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let RecoveryOutcome::Reply {
            retired: true,
            payload,
            ..
        } = recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command)))
        else {
            panic!("retirement unconfirmed")
        };
        assert_eq!(payload["reconciled_by"], "authority_epoch");
        assert!(request.accepts_retirement_confirmation(&payload, &op));
        let mut forged = payload;
        forged["current_epoch"] = json!(request.authority_epoch);
        assert!(!request.accepts_retirement_confirmation(&forged, &op));
    }

    #[test]
    fn uncertain_bind_discovers_an_existing_owner_fence() {
        use recovery::{Outcome as RecoveryOutcome, Recovery};
        let (mut broker, original) = broker();
        let request = request();
        let mut recovery = Recovery::new(original, request.clone());
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let _ = answer(
            &mut broker,
            "pairing",
            json!({"ok":false,"code":"source_retirement_conflict"}),
        );
        for _ in 0..3 {
            assert!(matches!(
                recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
                RecoveryOutcome::Pending
            ));
        }
        let existing = json!({"ok":true,"epoch":request.authority_epoch,"source":request.source,"scope":request.scope,
            "operation_id":"owner-created-fence","state":"retired"});
        assert_eq!(
            answer(&mut broker, "pairing", existing)["action"],
            "lookup_current_source_retirement"
        );
        assert!(matches!(
            recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command))),
            RecoveryOutcome::Pending
        ));
        let RecoveryOutcome::FenceSettled {
            operation_id,
            fence_confirmed: true,
            ..
        } = recovery.advance(|command| Ok(broker.dispatch("amaterasu.nvhttp", command)))
        else {
            panic!("fence undiscovered")
        };
        assert_eq!(operation_id, "owner-created-fence");
    }
}
