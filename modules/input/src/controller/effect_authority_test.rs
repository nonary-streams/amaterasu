//! Test fixture compiling the pairing provider's actual admission ledger.
#![allow(dead_code, unused_imports)]
#[path = "../../../pairing/src/effect_gate.rs"]
mod pairing_gate;

use amaterasu_module_sdk::effect_authority::{
    Request, Source, StreamScope,
    consumer::{EffectTicket, StreamLeaseTicket},
};
use serde_json::json;

pub struct Fixture {
    pub gate: pairing_gate::EffectGate,
    pub epoch: String,
    pub source: Source,
}

impl Fixture {
    pub fn admitted() -> (Self, EffectTicket) {
        let mut gate = pairing_gate::EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let source = Source {
            version: 1,
            id: "a".repeat(32),
            generation: 2,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 2,
            device: "device".into(),
            device_generation: 3,
            effect_owner_epoch: "owner".into(),
        };
        gate.bind(source.clone()).unwrap();
        let mut effect =
            EffectTicket::new(epoch.clone(), source.clone(), "operation".into(), 1).unwrap();
        let Request::Admit {
            epoch: request_epoch,
            source: request_source,
            operation_id,
            sequence,
        } = effect.admit_request().unwrap()
        else {
            panic!("expected admission")
        };
        gate.require_epoch(&request_epoch).unwrap();
        let ticket = gate
            .admit(&request_source, &operation_id, sequence)
            .unwrap()
            .to_owned();
        effect
            .accept_admission(
                &json!({"ok":true,"epoch":epoch,"operation_id":operation_id,"ticket":ticket}),
            )
            .unwrap();
        (
            Self {
                gate,
                epoch,
                source,
            },
            effect,
        )
    }
    pub fn stream_admitted(scope: StreamScope) -> (Self, StreamLeaseTicket) {
        let mut gate = pairing_gate::EffectGate::new().unwrap();
        gate.recover("owner").unwrap();
        let epoch = gate.epoch().to_owned();
        let source = Source {
            version: 1,
            id: "a".repeat(32),
            generation: 2,
            host_epoch: "host".into(),
            session_id: "session".into(),
            stream_id: "stream".into(),
            stream_generation: 2,
            device: "device".into(),
            device_generation: 3,
            effect_owner_epoch: "owner".into(),
        };
        gate.bind_scoped(source.clone(), Some(scope.clone()))
            .unwrap();
        let mut lease =
            StreamLeaseTicket::new(epoch.clone(), source.clone(), "operation".into(), 1, scope)
                .unwrap();
        let Request::AdmitStreamLease {
            epoch: requested_epoch,
            source: requested_source,
            operation_id,
            sequence,
            scope,
        } = lease.admit_request().unwrap()
        else {
            panic!("expected stream admission")
        };
        gate.require_epoch(&requested_epoch).unwrap();
        let ticket = gate
            .admit_stream_lease(&requested_source, &operation_id, sequence, scope.clone())
            .unwrap()
            .to_owned();
        lease.accept_admission(&json!({"ok":true,"epoch":epoch,"operation_id":operation_id,"ticket":ticket,"kind":"stream_lease","source":requested_source,"scope":scope})).unwrap();
        (
            Self {
                gate,
                epoch,
                source,
            },
            lease,
        )
    }
    pub fn complete_stream(&mut self, effect: &mut StreamLeaseTicket) {
        let Request::CompleteStreamLease {
            epoch,
            operation_id,
            ticket,
            retirement,
        } = effect.completion_request().unwrap()
        else {
            panic!("expected stream completion")
        };
        assert_eq!(
            self.gate
                .complete_stream_lease("stale", &operation_id, &ticket, &retirement),
            Err("stale_epoch")
        );
        self.gate
            .complete_stream_lease(&epoch, &operation_id, &ticket, &retirement)
            .unwrap();
        // Completion is idempotent after an uncertain reply.
        self.gate
            .complete_stream_lease(&epoch, &operation_id, &ticket, &retirement)
            .unwrap();
        effect
            .confirm_completion(&json!({"ok":true,"epoch":epoch,"operation_id":operation_id}))
            .unwrap();
        let Request::Acknowledge {
            epoch,
            operation_id,
            ticket,
        } = effect.acknowledgment_request().unwrap()
        else {
            panic!("expected acknowledgment")
        };
        self.gate
            .acknowledge(&epoch, &operation_id, &ticket)
            .unwrap();
        effect
            .confirm_acknowledgment(&json!({"ok":true,"epoch":epoch,"operation_id":operation_id}))
            .unwrap();
    }
    pub fn fence(&mut self) {
        self.gate.fence();
        assert_eq!(
            self.gate.admit(&self.source, "late", 2),
            Err("admission_denied")
        );
        assert_eq!(
            self.gate
                .acknowledge_drain(&self.epoch, self.gate.fence_generation()),
            Err("effects_in_flight")
        );
    }
    pub fn complete(&mut self, effect: &mut EffectTicket) {
        let Request::Complete {
            epoch,
            operation_id,
            ticket,
        } = effect.completion_request().unwrap()
        else {
            panic!("expected completion")
        };
        assert_eq!(
            self.gate.complete("stale", &operation_id, &ticket),
            Err("stale_epoch")
        );
        self.gate.complete(&epoch, &operation_id, &ticket).unwrap();
        effect
            .confirm_completion(&json!({"ok":true,"epoch":epoch,"operation_id":operation_id}))
            .unwrap();
        let Request::Acknowledge {
            epoch,
            operation_id,
            ticket,
        } = effect.acknowledgment_request().unwrap()
        else {
            panic!("expected acknowledgment")
        };
        self.gate
            .acknowledge(&epoch, &operation_id, &ticket)
            .unwrap();
        effect
            .confirm_acknowledgment(&json!({"ok":true,"epoch":epoch,"operation_id":operation_id}))
            .unwrap();
    }
    pub fn drain(&mut self) {
        let generation = self.gate.fence_generation();
        self.gate
            .acknowledge_drain(&self.epoch, generation)
            .unwrap();
        // A lost drain reply is safe to reconcile with the same epoch and fence.
        self.gate
            .acknowledge_drain(&self.epoch, generation)
            .unwrap();
        assert_eq!(
            self.gate.acknowledge_drain(&self.epoch, generation + 1),
            Err("stale_fence")
        );
        assert!(self.gate.drained());
    }
}

#[test]
fn lost_admission_reconciles_after_fence_without_starting_effect() {
    let (mut authority, ticket) = Fixture::admitted();
    // Simulate losing the admission response before the owner can use it.
    drop(ticket);
    let mut recovered = EffectTicket::new(
        authority.epoch.clone(),
        authority.source.clone(),
        "operation".into(),
        1,
    )
    .unwrap();
    let Request::Lookup {
        epoch,
        operation_id,
    } = recovered.lookup_request().unwrap()
    else {
        panic!("expected lookup")
    };
    authority.fence();
    authority.gate.require_epoch(&epoch).unwrap();
    let (secret, fenced) = authority.gate.lookup(&operation_id).unwrap();
    recovered.accept_lookup(&json!({"ok":true,"epoch":epoch,"operation_id":operation_id,"ticket":secret,"fenced":fenced})).unwrap();
    assert!(
        recovered
            .run_local(|| panic!("fenced effect must not run"))
            .is_err()
    );
    authority.complete(&mut recovered);
    authority.drain();
}

#[test]
fn lost_scoped_admission_after_fence_is_retirement_only() {
    use amaterasu_module_sdk::effect_authority::consumer::{
        FeedbackLease, TransportRetirement, feedback_capability,
    };
    use amaterasu_module_sdk::input::extensions::{Capability, Direction};
    use amaterasu_module_sdk::transport::control_retirement::Receipt;
    let capability = Capability {
        schema: "org.amaterasu.feedback.rumble".into(),
        version: 1,
        direction: Direction::ToClient,
    };
    let scope = StreamScope {
        capabilities: [feedback_capability(&capability).unwrap()].into(),
        controller_slot: 7,
        transport_service: "amaterasu.control-transport/1".into(),
        transport_stream: 7,
        transport_epoch: "c".repeat(32),
    };
    let (mut authority, ticket) = Fixture::stream_admitted(scope.clone());
    drop(ticket);
    let mut recovered = StreamLeaseTicket::new(
        authority.epoch.clone(),
        authority.source.clone(),
        "operation".into(),
        1,
        scope.clone(),
    )
    .unwrap();
    assert!(matches!(
        recovered.lookup_request().unwrap(),
        Request::LookupStreamLease { .. }
    ));
    authority.fence();
    let (secret, fenced) = authority
        .gate
        .lookup_stream_lease(&authority.source, "operation", &scope)
        .unwrap();
    let mut response = json!({"ok":true,"epoch":authority.epoch,"operation_id":"operation","ticket":secret,"kind":"stream_lease","fenced":fenced,"source":authority.source,"scope":scope});
    response["scope"]["transport_stream"] = json!(8);
    assert!(recovered.accept_lookup(&response).is_err());
    response["scope"]["transport_stream"] = json!(7);
    recovered.accept_lookup(&response).unwrap();
    let retirement = TransportRetirement {
        stream: 7,
        operation_id: "retire".into(),
        provider_epoch: "c".repeat(32),
    };
    let mut lease = FeedbackLease::new(recovered, retirement, 7, [capability.clone()].into())
        .unwrap_or_else(|_| panic!("lease"));
    assert!(
        lease
            .enqueue(&capability, 1, "effect", b"data", || -> Result<bool, ()> {
                panic!("fenced")
            })
            .is_err()
    );
    let receipt = Receipt {
        ok: true,
        state: "retired".into(),
        scope: "whole_control_stream".into(),
        stream: 7,
        operation_id: "retire".into(),
        provider_epoch: "c".repeat(32),
    };
    let mut recovered = lease
        .retire(&receipt)
        .unwrap_or_else(|_| panic!("retirement"));
    authority.complete_stream(&mut recovered);
    authority.drain();
}
