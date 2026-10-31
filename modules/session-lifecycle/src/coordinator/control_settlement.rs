//! Immutable provisional and owner-acknowledged control settlement records.
//! The coordinator journals this book atomically with authority and Work state.
use crate::prepare_authority::{Admission, ControlPermit};
use crate::provider_dispatch::{DispatchRecord, DispatchState};
use amaterasu_extension_api::{
    ConfirmedControl, ResourceControlResponse, ResourceControlSettlementRequest,
    validate_resource_control_response, validate_resource_control_settlement_request,
    validate_resource_control_settlement_response,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_SETTLEMENTS: usize = 128;
const MAX_REPLAY_FENCES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlSettlementRecord {
    pub settlement: ResourceControlSettlementRequest,
    /// Coordinator-selected original input selection, which may predate release.
    pub selection_transition_id: String,
    pub confirmed: ConfirmedControl,
    pub applied: ResourceControlResponse,
    /// Exact owner effect or status result that produced `applied`.
    pub effect_dispatch: DispatchRecord,
    /// Original owner Pending response when the effect settled via status.
    #[serde(default)]
    pub effect_pending_proof: Option<DispatchRecord>,
    pub owner_receipt_id: Option<String>,
    pub owner_ack: Option<ResourceControlResponse>,
    #[serde(default)]
    pub owner_dispatch: Option<DispatchRecord>,
}
impl ControlSettlementRecord {
    /// Structural and exact permit-scoped dispatch checks; installed broker
    /// approvals and live or retained provenance require separate verification.
    pub fn valid_under_permit(&self, permit: &ControlPermit) -> bool {
        valid_record(self) && effect_matches_policy(self, permit)
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlSettlementBook {
    records: Vec<ControlSettlementRecord>,
    /// Permanent digests of compacted identifiers. Exhaustion fails closed.
    #[serde(default)]
    replay_fences: BTreeSet<String>,
}

impl ControlSettlementBook {
    pub fn records(&self) -> impl Iterator<Item = &ControlSettlementRecord> {
        self.records.iter()
    }
    pub fn get(&self, id: &str) -> Option<&ControlSettlementRecord> {
        self.records
            .iter()
            .find(|item| item.settlement.settlement_id == id)
    }
    /// This detects corrupt structure, not current authority, owner identity or
    /// schema provenance. Recheck all against the coordinator journal on load.
    pub fn validate_structure(&self) -> bool {
        self.records.len() <= MAX_SETTLEMENTS
            && self.replay_fences.len() <= MAX_REPLAY_FENCES
            && self.replay_fences.iter().all(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            && self.records.iter().all(|item| {
                replay_keys(item)
                    .iter()
                    .all(|key| !self.replay_fences.contains(key))
            })
            && self.records.iter().enumerate().all(|(index, item)| {
                valid_record(item)
                    && !self.records[..index].iter().any(|old| {
                        old.settlement.settlement_id == item.settlement.settlement_id
                            || old.settlement.request.permit_id == item.settlement.request.permit_id
                    })
            })
    }
    /// Called only after the coordinator authenticates and validates Applied.
    /// `permit` and `admission` are read from the same locked journal; `verify`
    /// checks frozen graph, installed schema and native observation independently.
    pub fn record_provisional(
        &mut self,
        item: ControlSettlementRecord,
        permit: &ControlPermit,
        admission: &Admission,
        verify: &impl Fn(&ControlSettlementRecord) -> bool,
    ) -> Result<(), &'static str> {
        if item.owner_ack.is_some()
            || item.owner_dispatch.is_some()
            || !permit.consumed
            || permit.settled
            || permit.request != item.settlement.request
            || admission.provider != permit.context.policy.dependent_provider
            || !admission
                .plan
                .control_proposals
                .contains(&item.settlement.request.proposal)
            || permit.context.target != item.settlement.request.target
            || permit.context.transition_id != item.settlement.request.transition_id
            || permit.context.output.schema != item.confirmed.output.schema
            || permit.context.output.origin != item.confirmed.output.origin
            || !effect_matches_policy(&item, permit)
            || !valid_record(&item)
            || !verify(&item)
        {
            return Err("invalid_provisional_control_settlement");
        }
        if let Some(old) = self.get(&item.settlement.settlement_id) {
            return if old == &item {
                Ok(())
            } else {
                Err("control_settlement_conflict")
            };
        }
        if self.is_fenced(&item)
            || self.records.len() >= MAX_SETTLEMENTS
            || self
                .records
                .iter()
                .any(|old| old.settlement.request.permit_id == item.settlement.request.permit_id)
        {
            return Err("control_settlement_capacity_or_conflict");
        }
        self.records.push(item);
        Ok(())
    }
    fn is_fenced(&self, item: &ControlSettlementRecord) -> bool {
        replay_keys(item)
            .iter()
            .any(|key| self.replay_fences.contains(key))
    }
    /// Check before effect dispatch; rejecting only while recording the outcome
    /// would be too late to prevent replayed native effects.
    #[allow(dead_code, reason = "awaits coordinator-owned lineage closure")]
    pub fn operation_fenced(&self, owner: &str, operation_id: &str) -> bool {
        self.replay_fences
            .contains(&replay_digest(&["operation", owner, operation_id]))
    }
    /// Remove acknowledged records only after the coordinator independently
    /// proves every dependent and owner obligation closed. Persist this book
    /// atomically with that proof and the rest of the lineage retirement.
    #[allow(dead_code, reason = "awaits coordinator-owned lineage closure")]
    pub fn compact_confirmed(&mut self, closed: &impl Fn(&ControlSettlementRecord) -> bool) {
        self.records.retain(|item| {
            if item.owner_ack.is_none()
                || item.owner_dispatch.is_none()
                || !valid_record(item)
                || !closed(item)
            {
                return true;
            }
            let keys = replay_keys(item);
            let additional = keys
                .iter()
                .filter(|key| !self.replay_fences.contains(*key))
                .count();
            if self.replay_fences.len() + additional > MAX_REPLAY_FENCES {
                return true;
            }
            self.replay_fences.extend(keys);
            false
        });
    }
    /// Caller authenticates selected owner and exact broker completion before
    /// calling. Atomically persist this update with the Work and authority state.
    pub fn acknowledge(
        &mut self,
        id: &str,
        ack: ResourceControlResponse,
        dispatch: DispatchRecord,
        verify: &impl Fn(&ControlSettlementRecord, &ResourceControlResponse, &DispatchRecord) -> bool,
    ) -> Result<(), &'static str> {
        let item = self
            .records
            .iter_mut()
            .find(|item| item.settlement.settlement_id == id)
            .ok_or("unknown_control_settlement")?;
        if validate_resource_control_settlement_response(&item.settlement, &ack).is_err()
            || !valid_dispatch(item, &ack, &dispatch)
            || !verify(item, &ack, &dispatch)
        {
            return Err("invalid_control_settlement_ack");
        }
        if let Some(old) = &item.owner_ack {
            return if old == &ack && item.owner_dispatch.as_ref() == Some(&dispatch) {
                Ok(())
            } else {
                Err("control_settlement_ack_conflict")
            };
        }
        item.owner_ack = Some(ack);
        item.owner_dispatch = Some(dispatch);
        Ok(())
    }
}
fn replay_digest(parts: &[&str]) -> String {
    let bytes = serde_json::to_vec(parts).expect("string tuple serializes");
    format!("{:x}", Sha256::digest(bytes))
}
fn replay_keys(item: &ControlSettlementRecord) -> [String; 3] {
    let request = &item.settlement.request;
    [
        replay_digest(&["settlement", &item.settlement.settlement_id]),
        replay_digest(&["permit", &request.permit_id]),
        replay_digest(&[
            "operation",
            &request.proposal.owner_provider,
            &request.operation_id,
        ]),
    ]
}
fn valid_record(item: &ControlSettlementRecord) -> bool {
    validate_resource_control_settlement_request(&item.settlement).is_ok()
        && !item.selection_transition_id.is_empty()
        && item.selection_transition_id.len() <= 128
        && !item.selection_transition_id.chars().any(char::is_control)
        && validate_resource_control_response(&item.settlement.request, &item.applied).is_ok()
        && item
            .owner_receipt_id
            .as_ref()
            .is_none_or(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
        && item.confirmed.proposal_id == item.settlement.request.proposal.proposal_id
        && item.confirmed.output == item.settlement.output
        && matches!(&item.applied,ResourceControlResponse::Applied { output,candidate } if **output==item.confirmed.output && candidate.as_deref()==item.confirmed.candidate.as_ref())
        && item.effect_dispatch.call.approval["provider"].as_str()
            == Some(item.settlement.request.proposal.owner_provider.as_str())
        && matches!(&item.effect_dispatch.state, DispatchState::Completed { payload } if serde_json::from_value::<ResourceControlResponse>(payload.clone()).ok().as_ref() == Some(&item.applied))
        && match (&item.owner_ack, &item.owner_dispatch) {
            (None, None) => true,
            (Some(ack), Some(dispatch)) => {
                validate_resource_control_settlement_response(&item.settlement, ack).is_ok()
                    && valid_dispatch(item, ack, dispatch)
            }
            _ => false,
        }
}
fn effect_matches_policy(item: &ControlSettlementRecord, permit: &ControlPermit) -> bool {
    let call = &item.effect_dispatch.call;
    let policy = &permit.context.policy;
    let matches_method = |method: &amaterasu_extension_api::MethodRef| {
        call.service == method.service
            && call.method == method.method
            && call.interface_major == method.interface_major
            && call.interface_minor == method.interface_minor
    };
    let direct = matches_method(&policy.owner_method)
        && call.operation_id == item.settlement.request.operation_id
        && serde_json::to_value(&item.settlement.request).ok().as_ref() == Some(&call.arguments);
    let status: Option<amaterasu_extension_api::ProviderStatusRequest> =
        serde_json::from_value(call.arguments.clone()).ok();
    let polled = matches_method(&policy.status_method)
        && call.operation_id != item.settlement.request.operation_id
        && status.is_some_and(|status| {
            status.provider_sequence == item.settlement.request.provider_sequence
                && status.operation_id == item.settlement.request.operation_id
                && status.transition_id == item.settlement.request.transition_id
                && !status.receipt_id.is_empty()
                && item.effect_pending_proof.as_ref().is_some_and(|proof| {
                    matches_method_call(&proof.call, &policy.owner_method)
                        && proof.call.operation_id == item.settlement.request.operation_id
                        && serde_json::to_value(&item.settlement.request).ok().as_ref() == Some(&proof.call.arguments)
                        && matches!(&proof.state, DispatchState::Completed { payload } if payload == &serde_json::json!({"status":"pending","receipt_id":status.receipt_id}))
                })
        });
    direct || polled
}
fn matches_method_call(
    call: &crate::provider_dispatch::ProviderCall,
    method: &amaterasu_extension_api::MethodRef,
) -> bool {
    call.service == method.service
        && call.method == method.method
        && call.interface_major == method.interface_major
        && call.interface_minor == method.interface_minor
}
fn valid_dispatch(
    item: &ControlSettlementRecord,
    ack: &ResourceControlResponse,
    dispatch: &DispatchRecord,
) -> bool {
    dispatch.call.operation_id == format!("{}.settle", item.settlement.request.operation_id)
        && dispatch.call.approval["provider"].as_str()
            == Some(item.settlement.request.proposal.owner_provider.as_str())
        && serde_json::to_value(&item.settlement).ok().as_ref() == Some(&dispatch.call.arguments)
        && matches!(&dispatch.state, DispatchState::Completed { payload } if serde_json::from_value::<ResourceControlResponse>(payload.clone()).ok().as_ref() == Some(ack))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> ControlSettlementRecord {
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let schema = json!({"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)});
        let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":target,"resource":resource,"data":{},"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}]});
        let request = json!({"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":1,"target":target,"proposal":{"proposal_id":"proposal","output_id":"out","owner_provider":"owner","owner_service":"owner/2","target":target,"resource":resource,"method":{"service":"owner/2","method":"control","interface_major":2,"interface_minor":0,"schema_revision":1},"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":["domain"]},"admission_id":"admission","permit_id":"permit","remaining_ms":1000});
        serde_json::from_value(json!({"settlement":{"request":request,"output":output,"settlement_id":"settlement"},"selection_transition_id":"transition","confirmed":{"proposal_id":"proposal","output":output,"candidate":null},"applied":{"status":"applied","output":output,"candidate":null},"effect_dispatch":{"call":{"service":"owner/2","method":"control","interface_major":2,"interface_minor":0,"approval":{"provider":"owner"},"arguments":request,"timeout_ms":1000,"operation_id":"effect"},"state":{"state":"completed","payload":{"status":"applied","output":output,"candidate":null}}},"owner_receipt_id":null,"owner_ack":null})).unwrap()
    }
    #[test]
    fn acknowledgement_requires_exact_output_and_trusted_owner_proof() {
        let item = fixture();
        assert!(valid_record(&item));
        let mut book = ControlSettlementBook {
            records: vec![item.clone()],
            ..Default::default()
        };
        let ack = ResourceControlResponse::Applied {
            output: Box::new(item.confirmed.output.clone()),
            candidate: None,
        };
        let dispatch: DispatchRecord = serde_json::from_value(json!({"call":{"service":"owner/2","method":"settle","interface_major":2,"interface_minor":0,"approval":{"provider":"owner"},"arguments":item.settlement,"timeout_ms":1000,"operation_id":"effect.settle"},"state":{"state":"completed","payload":ack}})).unwrap();
        assert!(
            book.acknowledge("settlement", ack.clone(), dispatch.clone(), &|_, _, _| {
                false
            })
            .is_err()
        );
        assert!(book.get("settlement").unwrap().owner_ack.is_none());
        assert!(
            book.acknowledge("settlement", ack.clone(), dispatch.clone(), &|_, _, _| true)
                .is_ok()
        );
        assert!(
            book.acknowledge("settlement", ack, dispatch, &|_, _, _| true)
                .is_ok()
        );
        assert!(book.validate_structure());
        let mut changed = item;
        changed.confirmed.output.resource.incarnation = "foreign".into();
        assert!(!valid_record(&changed));
    }
    #[test]
    fn duplicate_permit_or_settlement_id_is_rejected_on_recovery() {
        let item = fixture();
        let mut duplicate = item.clone();
        duplicate.settlement.settlement_id = "another".into();
        assert!(
            !ControlSettlementBook {
                records: vec![item, duplicate],
                ..Default::default()
            }
            .validate_structure()
        );
    }
    fn acknowledged_fixture(index: usize) -> ControlSettlementRecord {
        let mut item = fixture();
        item.settlement.settlement_id = format!("settlement-{index}");
        item.settlement.request.permit_id = format!("permit-{index}");
        item.settlement.request.operation_id = format!("effect-{index}");
        item.effect_dispatch.call.operation_id = item.settlement.request.operation_id.clone();
        item.effect_dispatch.call.arguments =
            serde_json::to_value(&item.settlement.request).unwrap();
        let ack = item.applied.clone();
        item.owner_dispatch = Some(serde_json::from_value(json!({"call":{"service":"owner/2","method":"settle","interface_major":2,"interface_minor":0,"approval":{"provider":"owner"},"arguments":item.settlement,"timeout_ms":1000,"operation_id":format!("effect-{index}.settle")},"state":{"state":"completed","payload":ack}})).unwrap());
        item.owner_ack = Some(ack);
        assert!(valid_record(&item));
        item
    }

    #[test]
    fn compacted_closure_preserves_replay_fences_and_unresolved_records() {
        let mut book = ControlSettlementBook::default();
        let pending = fixture();
        book.records.push(pending.clone());
        let held = acknowledged_fixture(1000);
        book.records.push(held.clone());
        let first = acknowledged_fixture(0);
        for index in 0..200 {
            book.records.push(acknowledged_fixture(index));
            book.compact_confirmed(&|item| {
                item.settlement.settlement_id != held.settlement.settlement_id
            });
            assert!(book.validate_structure());
        }
        assert_eq!(book.records.len(), 2);
        assert_eq!(book.get(&pending.settlement.settlement_id), Some(&pending));
        assert_eq!(book.get(&held.settlement.settlement_id), Some(&held));
        let restored: ControlSettlementBook =
            serde_json::from_slice(&serde_json::to_vec(&book).unwrap()).unwrap();
        assert!(restored.validate_structure());
        assert!(restored.is_fenced(&first));
        assert!(restored.operation_fenced("owner", "effect-0"));
        assert!(!restored.operation_fenced("other-owner", "effect-0"));
        let mut replay = acknowledged_fixture(9999);
        replay.settlement.request.operation_id = first.settlement.request.operation_id.clone();
        assert!(restored.is_fenced(&replay));
        replay = acknowledged_fixture(9999);
        replay.settlement.request.permit_id = first.settlement.request.permit_id.clone();
        assert!(restored.is_fenced(&replay));
    }

    #[test]
    fn full_fence_budget_retains_the_full_proof() {
        let item = acknowledged_fixture(0);
        let mut book = ControlSettlementBook {
            records: vec![item.clone()],
            replay_fences: (0..MAX_REPLAY_FENCES)
                .map(|i| format!("{i:064x}"))
                .collect(),
        };
        book.compact_confirmed(&|_| true);
        assert_eq!(book.records, vec![item]);
        assert!(book.validate_structure());
    }
    #[test]
    fn retained_raw_owner_response_accepts_omitted_default_field() {
        let mut item = fixture();
        let candidate = json!({"capture_provider":"capture","selector_schema":"selector","selector":{},"resource":item.confirmed.output.resource,"mode":{"width":1920,"height":1080,"refresh_millihz":60000,"hdr":false},"plan_revision":1,"geometry_revision":1,"generation":1});
        let payload =
            json!({"status":"applied","output":item.confirmed.output,"candidate":candidate});
        item.applied = serde_json::from_value(payload.clone()).unwrap();
        if let ResourceControlResponse::Applied { candidate, .. } = &item.applied {
            item.confirmed.candidate = candidate.as_deref().cloned();
        }
        item.effect_dispatch.state = DispatchState::Completed { payload };
        assert!(valid_record(&item));
    }
}
