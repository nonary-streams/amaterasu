//! Test fixture compiling the pairing provider's actual admission ledger.
#![allow(dead_code, unused_imports)]
#[path = "../../../pairing/src/effect_gate.rs"]
mod pairing_gate;

use amaterasu_module_sdk::effect_authority::{Request, Source, consumer::EffectTicket};
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
        self.gate
            .acknowledge_drain(&self.epoch, self.gate.fence_generation())
            .unwrap();
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
