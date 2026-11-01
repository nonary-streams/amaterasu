//! Pairing's bounded canonical issue transaction and broker-atomic handoff.
use super::{STATE_SERVICE, Transport};
use amaterasu_module_sdk::{
    Command, ModuleResult, Response,
    broker_effect_fence::{EffectFenceCommand, EffectScope, SourceSlotLease},
    effect_authority::{ProtocolEffectPolicy, Source},
    protocol::{PendingEffect, Route},
    protocol_slot_adapter::{
        ConfirmedSlotIssue, PendingSlotIssue, SelectedSlotAction, SlotActionRequest,
    },
    protocol_slot_state::{self, Entry},
    protocol_state::{CleanupReference, DurableStateBinding, fresh_state_operation_id},
};

pub(super) struct IssueDraft {
    pub(super) source: Source,
    pub(super) effect: PendingEffect,
    pub(super) policy: ProtocolEffectPolicy,
    pub(super) scope: EffectScope,
    pub(super) route: Route,
    pub(super) sequence: u64,
    pub(super) payload: Vec<u8>,
    pub(super) cleanup: CleanupReference,
}

enum Phase {
    Read,
    Reading(u64),
    Writing(u64, Box<PendingSlotIssue>, Box<Entry>),
    Reconciling(u64, Box<Entry>),
    ReadCommit(Box<ConfirmedSlotIssue>),
    ReadingCommit(u64, Box<ConfirmedSlotIssue>),
    Open(u64, Box<ConfirmedSlotIssue>),
    Commit(u64, Box<ConfirmedSlotIssue>),
    Done,
}
pub(super) enum Outcome {
    Pending,
    /// The broker committed the arm and completed the original issuer request.
    Committed,
    /// The broker handoff may have committed. Do not infer cancellation.
    AdmissionUncertain,
    Reconcile,
}
pub(super) struct IssueJob {
    pub(super) parent: u64,
    pub(super) draft: IssueDraft,
    lease: SourceSlotLease,
    selected: SelectedSlotAction,
    state: DurableStateBinding,
    phase: Phase,
}
impl IssueJob {
    pub(super) fn new(
        parent: u64,
        draft: IssueDraft,
        lease: SourceSlotLease,
        state: DurableStateBinding,
        send: impl FnMut(Command) -> Transport,
    ) -> Result<Self, &'static str> {
        let selected = SelectedSlotAction::query(
            SlotActionRequest {
                pending: draft.effect.clone(),
                issuer: &draft.source,
                scope: draft.scope.clone(),
                lease: lease.clone(),
                route: &draft.route,
                sequence: draft.sequence,
                payload: &draft.payload,
            },
            send,
        )
        .map_err(|_| "protocol_selection_changed")?;
        Ok(Self {
            parent,
            draft,
            lease,
            selected,
            state,
            phase: Phase::Read,
        })
    }
    pub(super) fn advance(
        &mut self,
        current: bool,
        mut send: impl FnMut(Command) -> Transport,
    ) -> ModuleResult<Outcome> {
        let phase = std::mem::replace(&mut self.phase, Phase::Done);
        if !current
            || !matches!(
                send(Command::CheckRequest {
                    request: self.parent
                }),
                Ok(Response::Ok)
            )
        {
            match phase {
                Phase::Reading(id)
                | Phase::Writing(id, _, _)
                | Phase::Reconciling(id, _)
                | Phase::ReadingCommit(id, _)
                | Phase::Open(id, _) => {
                    let _ = send(Command::Cancel { request: id });
                    let _ = send(Command::ReleaseResult { request: id });
                }
                Phase::Commit(id, _) => {
                    self.fence(&mut send);
                    let _ = send(Command::ReleaseResult { request: id });
                }
                _ => {}
            }
            return Ok(Outcome::Reconcile);
        }
        match phase {
            Phase::Read => {
                self.phase = Phase::Reading(self.read(&mut send)?);
                Ok(Outcome::Pending)
            }
            Phase::Reading(id) => match send(Command::TakeResult { request: id })? {
                Response::Pending => {
                    self.phase = Phase::Reading(id);
                    Ok(Outcome::Pending)
                }
                Response::Result { payload } => {
                    if !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) {
                        return Ok(Outcome::Reconcile);
                    }
                    let issue = self
                        .selected
                        .prepare_issue(&self.state, &payload, self.draft.cleanup.clone())
                        .map_err(str::to_owned)?;
                    let entry = Box::new(issue.entry().clone());
                    match send(Command::Request {
                        service: STATE_SERVICE.into(),
                        payload: issue.request().payload(),
                        timeout_ms: 5_000,
                        operation_id: issue.state_operation_id().into(),
                        parent: None,
                    }) {
                        Ok(Response::Accepted { id }) => {
                            self.phase = Phase::Writing(id, Box::new(issue), entry);
                            Ok(Outcome::Pending)
                        }
                        _ => Ok(Outcome::Reconcile),
                    }
                }
                _ => Ok(Outcome::Reconcile),
            },
            Phase::Writing(id, issue, entry) => match send(Command::TakeResult { request: id }) {
                Ok(Response::Pending) => {
                    self.phase = Phase::Writing(id, issue, entry);
                    Ok(Outcome::Pending)
                }
                result => {
                    let released = matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    );
                    if let Ok(Response::Result { payload }) = result
                        && let Ok(confirmed) = (*issue).confirmed(payload)
                        && released
                    {
                        self.phase = Phase::ReadCommit(Box::new(confirmed));
                        return Ok(Outcome::Pending);
                    }
                    self.phase = Phase::Reconciling(self.read(&mut send)?, entry);
                    Ok(Outcome::Pending)
                }
            },
            Phase::Reconciling(id, expected) => match send(Command::TakeResult { request: id })? {
                Response::Pending => {
                    self.phase = Phase::Reconciling(id, expected);
                    Ok(Outcome::Pending)
                }
                Response::Result { payload } => {
                    if !matches!(
                        send(Command::ReleaseResult { request: id }),
                        Ok(Response::Ok)
                    ) {
                        return Ok(Outcome::Reconcile);
                    }
                    if let Ok(confirmed) =
                        self.selected
                            .reconcile_issue(&self.state, &payload, &expected)
                    {
                        self.phase = Phase::ReadCommit(Box::new(confirmed));
                        Ok(Outcome::Pending)
                    } else {
                        Ok(Outcome::Reconcile)
                    }
                }
                _ => Ok(Outcome::Reconcile),
            },
            Phase::ReadCommit(confirmed) => {
                self.phase = Phase::ReadingCommit(self.read(&mut send)?, confirmed);
                Ok(Outcome::Pending)
            }
            Phase::ReadingCommit(id, confirmed) => {
                match send(Command::TakeResult { request: id }) {
                    Ok(Response::Pending) => {
                        self.phase = Phase::ReadingCommit(id, confirmed);
                        Ok(Outcome::Pending)
                    }
                    Ok(Response::Result { payload }) => {
                        let Ok((_, slot)) = protocol_slot_state::read(self.lease.slot, &payload)
                        else {
                            let _ = send(Command::ReleaseResult { request: id });
                            return Ok(Outcome::Reconcile);
                        };
                        if !slot.active.is_some_and(|claim| {
                            claim.lease == self.lease
                                && claim.scope == self.draft.scope
                                && claim.issuer == self.draft.effect.caller
                                && claim.entries.iter().any(|entry| entry == confirmed.entry())
                        }) {
                            let _ = send(Command::ReleaseResult { request: id });
                            return Ok(Outcome::Reconcile);
                        }
                        // Keep this authenticated State result retained: the broker
                        // verifies it under the same lock as admission.
                        self.phase = Phase::Open(id, confirmed);
                        Ok(Outcome::Pending)
                    }
                    _ => {
                        let _ = send(Command::ReleaseResult { request: id });
                        Ok(Outcome::Reconcile)
                    }
                }
            }
            Phase::Open(id, entry) => {
                match send(Command::EffectFence {
                    command: Box::new(EffectFenceCommand::OpenSlot {
                        lease: self.lease.clone(),
                        scope: self.draft.scope.clone(),
                        retired_through: 0,
                    }),
                }) {
                    Ok(Response::Ok) => {
                        self.phase = Phase::Commit(id, entry);
                        Ok(Outcome::Pending)
                    }
                    _ => {
                        self.fence(&mut send);
                        let _ = send(Command::ReleaseResult { request: id });
                        Ok(Outcome::Reconcile)
                    }
                }
            }
            Phase::Commit(id, confirmed) => {
                let mut definitive_rejection = false;
                let response = self.selected.commit_admission(
                    &confirmed,
                    &self.draft.payload,
                    self.parent,
                    id,
                    |command| {
                        let result = send(command);
                        definitive_rejection = matches!(result, Ok(Response::Error { .. }));
                        result
                    },
                );
                let _ = send(Command::ReleaseResult { request: id });
                match response {
                    Ok(()) => Ok(Outcome::Committed),
                    Err(_) if definitive_rejection => {
                        self.fence(&mut send);
                        Ok(Outcome::Reconcile)
                    }
                    _ => Ok(Outcome::AdmissionUncertain),
                }
            }
            Phase::Done => Ok(Outcome::Reconcile),
        }
    }
    fn read(&self, send: &mut impl FnMut(Command) -> Transport) -> ModuleResult<u64> {
        let read = protocol_slot_state::read_request(self.lease.slot).map_err(str::to_owned)?;
        match send(Command::Request {
            service: STATE_SERVICE.into(),
            payload: read.payload(),
            timeout_ms: 5_000,
            operation_id: fresh_state_operation_id().map_err(str::to_owned)?,
            parent: None,
        })? {
            Response::Accepted { id } => Ok(id),
            _ => Err("state_unavailable".into()),
        }
    }
    fn fence(&self, send: &mut impl FnMut(Command) -> Transport) {
        let _ = send(Command::EffectFence {
            command: Box::new(EffectFenceCommand::BeginCloseSlot {
                lease: self.lease.clone(),
                scope: self.draft.scope.clone(),
            }),
        });
    }
}

impl super::Pipeline {
    pub(super) fn protocol_draft_current(&self, draft: &IssueDraft, now: u64) -> bool {
        amaterasu_module_sdk::broker_effect_fence::source(&draft.source)
            .ok()
            .as_ref()
            == Some(&draft.scope.source)
            && self
                .admit_protocol_effect(
                    1,
                    draft.scope.authority_epoch.clone(),
                    draft.source.clone(),
                    draft.effect.clone(),
                    &draft.effect.caller,
                    now,
                )
                .is_ok_and(|policy| policy == draft.policy)
    }
    pub(super) fn selected_protocol_state(
        &self,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> Option<amaterasu_module_sdk::broker_effect_fence::ProviderIncarnation> {
        let state = self.protocol_state_binding.as_ref()?;
        let Ok(Response::Result { payload }) = send(Command::EffectFence { command: Box::new(amaterasu_module_sdk::broker_effect_fence::EffectFenceCommand::SelectedStateProvider {}) }) else { return None; };
        let instance: amaterasu_module_sdk::broker_effect_fence::ProviderIncarnation =
            serde_json::from_value(payload["provider_incarnation"].clone()).ok()?;
        let require_handoff = payload["require_handoff"].as_bool()?;
        if instance.version != 1
            || instance.id.len() != 32
            || !instance
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || payload
                != serde_json::json!({"owner":state.owner(),"provider":state.provider(),"contract":STATE_SERVICE,"capability":amaterasu_module_sdk::protocol_state::DURABLE_CAS_CAPABILITY,"provider_incarnation":instance,"require_handoff":require_handoff})
        {
            return None;
        }
        Some(instance)
    }
    pub(super) fn protocol_state_current(
        &self,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> bool {
        self.selected_protocol_state(send).as_ref() == self.protocol_state_instance.as_ref()
            && self.protocol_state_instance.is_some()
            && self.protocol_handoff_enabled(send)
    }

    pub(super) fn protocol_handoff_enabled(
        &self,
        send: &mut impl FnMut(Command) -> Transport,
    ) -> bool {
        matches!(send(Command::EffectFence { command:Box::new(amaterasu_module_sdk::broker_effect_fence::EffectFenceCommand::SelectedStateProvider {}) }), Ok(Response::Result { payload }) if payload["require_handoff"] == true)
    }
}
