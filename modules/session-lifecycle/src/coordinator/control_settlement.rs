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

const MAX_SETTLEMENTS: usize = 128;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlSettlementRecord {
    pub settlement: ResourceControlSettlementRequest,
    pub confirmed: ConfirmedControl,
    pub applied: ResourceControlResponse,
    pub owner_receipt_id: Option<String>,
    pub owner_ack: Option<ResourceControlResponse>,
    #[serde(default)]
    pub owner_dispatch: Option<DispatchRecord>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ControlSettlementBook {
    records: Vec<ControlSettlementRecord>,
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
        if self.records.len() >= MAX_SETTLEMENTS
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
fn valid_record(item: &ControlSettlementRecord) -> bool {
    validate_resource_control_settlement_request(&item.settlement).is_ok()
        && validate_resource_control_response(&item.settlement.request, &item.applied).is_ok()
        && item
            .owner_receipt_id
            .as_ref()
            .is_none_or(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
        && item.confirmed.proposal_id == item.settlement.request.proposal.proposal_id
        && item.confirmed.output == item.settlement.output
        && matches!(&item.applied,ResourceControlResponse::Applied { output,candidate } if **output==item.confirmed.output && candidate.as_deref()==item.confirmed.candidate.as_ref())
        && match (&item.owner_ack, &item.owner_dispatch) {
            (None, None) => true,
            (Some(ack), Some(dispatch)) => {
                validate_resource_control_settlement_response(&item.settlement, ack).is_ok()
                    && valid_dispatch(item, ack, dispatch)
            }
            _ => false,
        }
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
        && matches!(&dispatch.state, DispatchState::Completed { payload } if serde_json::to_value(ack).ok().as_ref() == Some(payload))
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
        serde_json::from_value(json!({"settlement":{"request":request,"output":output,"settlement_id":"settlement"},"confirmed":{"proposal_id":"proposal","output":output,"candidate":null},"applied":{"status":"applied","output":output,"candidate":null},"owner_receipt_id":null,"owner_ack":null})).unwrap()
    }
    #[test]
    fn acknowledgement_requires_exact_output_and_trusted_owner_proof() {
        let item = fixture();
        assert!(valid_record(&item));
        let mut book = ControlSettlementBook {
            records: vec![item.clone()],
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
                records: vec![item, duplicate]
            }
            .validate_structure()
        );
    }
}
