//! Durable synchronous acknowledgement of one owner control settlement.
use crate::prepare_authority::ControlPolicy;
use crate::provider_dispatch::{
    DispatchJournal, DispatchRecord, DispatchState, ProviderCall, ProviderDispatcher,
};
use amaterasu_extension_api::{
    ResourceControlResponse, ResourceControlSettlementRequest,
    validate_resource_control_settlement_request, validate_resource_control_settlement_response,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum SettlementPhase {
    Ready,
    Acknowledged,
    RecoveryRequired,
}

/// Persist this state and the exact authenticated owner Applied record together
/// before calling the owner. Settlement is a separate, idempotent CAS, never a
/// replay of the original resource-control effect.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SettlementState {
    pub request: ResourceControlSettlementRequest,
    pub policy: ControlPolicy,
    pub call: ProviderCall,
    pub phase: SettlementPhase,
    pub dispatch: Option<DispatchRecord>,
    pub owner_ack: Option<ResourceControlResponse>,
}
impl SettlementState {
    pub fn new(
        request: ResourceControlSettlementRequest,
        policy: ControlPolicy,
        call: ProviderCall,
    ) -> ModuleResult<Self> {
        validate_resource_control_settlement_request(&request)?;
        let id = format!("{}.settle", request.request.operation_id);
        if id.len() > 128
            || call.operation_id != id
            || call.service != policy.settlement_method.service
            || call.method != policy.settlement_method.method
            || call.interface_major != policy.settlement_method.interface_major
            || call.interface_minor != policy.settlement_method.interface_minor
            || call.approval["provider"].as_str() != Some(policy.owner_provider.as_str())
            || request.request.proposal.owner_provider != policy.owner_provider
            || call.arguments != serde_json::to_value(&request).map_err(|e| e.to_string())?
        {
            return Err("settlement_selection_mismatch".into());
        }
        ProviderDispatcher::new(DispatchRecord {
            call: call.clone(),
            state: DispatchState::Ready,
        })?;
        Ok(Self {
            request,
            policy,
            call,
            phase: SettlementPhase::Ready,
            dispatch: None,
            owner_ack: None,
        })
    }
}
pub(super) trait SettlementJournal {
    fn persist(&mut self, state: &SettlementState) -> ModuleResult<()>;
}
pub(super) struct SettlementDispatch {
    state: SettlementState,
    dispatcher: Option<ProviderDispatcher>,
    failed: bool,
}
impl SettlementDispatch {
    pub fn new(state: SettlementState) -> ModuleResult<Self> {
        SettlementState::new(
            state.request.clone(),
            state.policy.clone(),
            state.call.clone(),
        )?;
        if let Some(record) = &state.dispatch
            && record.call != state.call
        {
            return Err("invalid_settlement_dispatch".into());
        }
        if let Some(ack) = &state.owner_ack {
            validate_resource_control_settlement_response(&state.request, ack)?;
        }
        if matches!(state.phase, SettlementPhase::Acknowledged) != state.owner_ack.is_some() {
            return Err("invalid_settlement_ack".into());
        }
        let dispatcher = state
            .dispatch
            .clone()
            .map(ProviderDispatcher::new)
            .transpose()?;
        Ok(Self {
            state,
            dispatcher,
            failed: false,
        })
    }
    pub fn state(&self) -> &SettlementState {
        &self.state
    }
    /// Validate the exact recorded tuple before submission; verify the owner ack
    /// independently before exposing it. No provider Pending response is accepted.
    pub fn advance(
        &mut self,
        journal: &mut impl SettlementJournal,
        validate_tuple: &mut impl FnMut(&ResourceControlSettlementRequest) -> ModuleResult<()>,
        validate_ack: &mut impl FnMut(
            &ResourceControlSettlementRequest,
            &ResourceControlResponse,
        ) -> ModuleResult<()>,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        if self.failed {
            return Err("settlement_journal_requires_recovery".into());
        }
        if !matches!(self.state.phase, SettlementPhase::Ready) {
            return Ok(());
        }
        if self
            .state
            .dispatch
            .as_ref()
            .is_none_or(|record| record.state == DispatchState::Ready)
        {
            validate_tuple(&self.state.request)?;
        }
        if self.dispatcher.is_none() {
            let record = DispatchRecord {
                call: self.state.call.clone(),
                state: DispatchState::Ready,
            };
            let dispatcher = ProviderDispatcher::new(record.clone())?;
            let mut next = self.state.clone();
            next.dispatch = Some(record);
            self.persist(journal, next)?;
            self.dispatcher = Some(dispatcher);
            return Ok(());
        }
        let dispatcher = self.dispatcher.as_mut().expect("checked");
        let mut bridge = Bridge {
            state: &mut self.state,
            journal,
            failed: &mut self.failed,
        };
        dispatcher.advance(&mut bridge, send)?;
        if dispatcher.has_request() {
            return Ok(());
        }
        match dispatcher.record().state.clone() {
            DispatchState::Ready => Ok(()),
            DispatchState::OutcomeUnknown => {
                let mut next = self.state.clone();
                next.phase = SettlementPhase::RecoveryRequired;
                self.persist(journal, next)?;
                self.dispatcher = None;
                Ok(())
            }
            DispatchState::Completed { payload } => self.settle(payload, journal, validate_ack),
        }
    }
    fn settle(
        &mut self,
        payload: Value,
        journal: &mut impl SettlementJournal,
        validate_ack: &mut impl FnMut(
            &ResourceControlSettlementRequest,
            &ResourceControlResponse,
        ) -> ModuleResult<()>,
    ) -> ModuleResult<()> {
        let ack: ResourceControlResponse =
            serde_json::from_value(payload).map_err(|_| "invalid_settlement_ack")?;
        validate_resource_control_settlement_response(&self.state.request, &ack)?;
        validate_ack(&self.state.request, &ack)?;
        let mut next = self.state.clone();
        next.owner_ack = Some(ack);
        next.phase = SettlementPhase::Acknowledged;
        // Retain the exact completed broker call as durable owner proof.
        self.persist(journal, next)?;
        self.dispatcher = None;
        Ok(())
    }
    fn persist(
        &mut self,
        journal: &mut impl SettlementJournal,
        next: SettlementState,
    ) -> ModuleResult<()> {
        if let Err(error) = journal.persist(&next) {
            self.failed = true;
            return Err(error);
        }
        self.state = next;
        Ok(())
    }
}
struct Bridge<'a, J> {
    state: &'a mut SettlementState,
    journal: &'a mut J,
    failed: &'a mut bool,
}
impl<J: SettlementJournal> DispatchJournal for Bridge<'_, J> {
    fn persist(&mut self, record: &DispatchRecord) -> ModuleResult<()> {
        let mut next = self.state.clone();
        next.dispatch = Some(record.clone());
        if let Err(error) = self.journal.persist(&next) {
            *self.failed = true;
            return Err(error);
        }
        *self.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    fn fixture() -> SettlementState {
        let target = json!({"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1});
        let schema = json!({"id":"schema","major":1,"minor":0,"digest":"a".repeat(64)});
        let resource = json!({"provider":"owner","resource_id":"output","incarnation":"native","revision":1,"execution_domain":"seat","mutation_domain":"domain","mutation_domains":["domain"]});
        let output = json!({"output_id":"out","capability":"display","schema":schema,"producer":"owner","producer_service":"owner/2","origin":target,"resource":resource,"data":{},"domain_observations":[{"domain":"domain","revision":1,"borrowers":{},"complete":true}]});
        let method = |name| json!({"service":"owner/2","method":name,"interface_major":2,"interface_minor":0,"schema_revision":1});
        let request:ResourceControlSettlementRequest=serde_json::from_value(json!({"request":{"version":2,"transition_id":"transition","operation_id":"effect","provider_sequence":1,"target":target,"proposal":{"proposal_id":"proposal","output_id":"out","owner_provider":"owner","owner_service":"owner/2","target":target,"resource":resource,"method":method("control"),"argument_schema":schema,"arguments":{},"access":"exclusive","mutation_domains":["domain"]},"admission_id":"admission","permit_id":"permit","remaining_ms":1000},"output":output,"settlement_id":"settlement"})).unwrap();
        let policy:ControlPolicy=serde_json::from_value(json!({"dependent_provider":"consumer","owner_provider":"owner","owner_service":"owner/2","owner_method":method("control"),"status_method":method("status"),"settlement_method":method("settle"),"argument_schema":schema,"output_capability":"display","output_schema":schema,"max_access":"exclusive","mutation_domains":["domain"]})).unwrap();
        let call = ProviderCall {
            service: "owner/2".into(),
            method: "settle".into(),
            interface_major: 2,
            interface_minor: 0,
            approval: json!({"provider":"owner","method":{"service":"owner/2","method":"settle","interface_major":2,"interface_minor":0}}),
            arguments: serde_json::to_value(&request).unwrap(),
            timeout_ms: 1000,
            operation_id: "effect.settle".into(),
        };
        SettlementState::new(request, policy, call).unwrap()
    }
    #[derive(Default)]
    struct Journal(Vec<SettlementState>);
    impl SettlementJournal for Journal {
        fn persist(&mut self, state: &SettlementState) -> ModuleResult<()> {
            self.0.push(state.clone());
            Ok(())
        }
    }
    #[test]
    fn lost_settlement_ack_fences_without_replaying_control() {
        let mut driver = SettlementDispatch::new(fixture()).unwrap();
        let mut journal = Journal::default();
        let mut replies = VecDeque::from([
            Ok(Response::Result {
                payload: driver.state().call.approval.clone(),
            }),
            Err("lost".into()),
        ]);
        let mut send = |_| replies.pop_front().unwrap();
        let mut tuple = |_: &ResourceControlSettlementRequest| Ok(());
        let mut ack = |_: &ResourceControlSettlementRequest, _: &ResourceControlResponse| Ok(());
        driver
            .advance(&mut journal, &mut tuple, &mut ack, &mut send)
            .unwrap();
        driver
            .advance(&mut journal, &mut tuple, &mut ack, &mut send)
            .unwrap();
        assert!(
            driver
                .advance(&mut journal, &mut tuple, &mut ack, &mut send)
                .is_err()
        );
        let mut restored = SettlementDispatch::new(journal.0.last().unwrap().clone()).unwrap();
        restored
            .advance(&mut journal, &mut tuple, &mut ack, &mut |_| {
                panic!("must not replay")
            })
            .unwrap();
        assert!(matches!(
            restored.state().phase,
            SettlementPhase::RecoveryRequired
        ));
    }
    #[test]
    fn pending_or_wrong_ack_never_advances_settlement() {
        let mut driver = SettlementDispatch::new(fixture()).unwrap();
        let mut journal = Journal::default();
        let mut ack = |_: &ResourceControlSettlementRequest, _: &ResourceControlResponse| Ok(());
        assert!(
            driver
                .settle(
                    json!({"status":"pending","receipt_id":"receipt"}),
                    &mut journal,
                    &mut ack
                )
                .is_err()
        );
        let mut wrong = driver.state().request.output.clone();
        wrong.resource.incarnation = "other".into();
        assert!(
            driver
                .settle(
                    json!({"status":"applied","output":wrong,"candidate":null}),
                    &mut journal,
                    &mut ack
                )
                .is_err()
        );
        assert!(journal.0.is_empty());
    }
}
