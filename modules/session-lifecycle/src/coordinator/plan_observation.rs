//! Durable read-only owner re-observation after staged rights are released.
//!
//! The coordinator must validate the frozen stage, current target and installed
//! owner before every tick, persist this state in the matching work, and accept
//! the result only after checking fresh observations against that stage. A plan
//! here is evidence, not an admission or permission to execute an effect.
use amaterasu_extension_api::{
    self as api, LifecyclePhase, ProviderOperation, ProviderRequest, ProviderResponse, ResourcePlan,
};
use amaterasu_module_sdk::{Command, ModuleResult, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::ParticipantSettings;
use crate::prepare_driver::PrepareAuthority;
use crate::provider_dispatch::{
    DispatchJournal, DispatchRecord, DispatchState, ProviderDispatcher,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PlanObservationPhase {
    Ready,
    Planned,
    RecoveryRequired,
}

/// The request identity and installed participant are frozen before dispatch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanObservationState {
    pub participant: ParticipantSettings,
    pub request: ProviderRequest,
    pub phase: PlanObservationPhase,
    pub dispatch: Option<DispatchRecord>,
    pub plan: Option<ResourcePlan>,
}

pub(super) trait PlanObservationJournal {
    /// Atomically persist the full work, including its authenticated context.
    fn persist(&mut self, state: &PlanObservationState) -> ModuleResult<()>;
}

pub(super) struct PlanObservationDriver {
    state: PlanObservationState,
    dispatcher: Option<ProviderDispatcher>,
    journal_failed: bool,
}

impl PlanObservationDriver {
    /// The caller reserves the exact sequence and operation ID durably first.
    pub fn new(state: PlanObservationState) -> ModuleResult<Self> {
        api::validate_provider_request(&state.request)?;
        if state.request.version != api::COMPOSITION_VERSION
            || state.request.operation != ProviderOperation::ResourcePlan
            || !matches!(
                state.request.phase,
                LifecyclePhase::StreamPrepare | LifecyclePhase::StreamRelease
            )
            || !matches!(state.request.target, api::LifecycleTarget::Stream { .. })
            || state.participant.contract_version != api::COMPOSITION_VERSION
            || state.request.settings_revision != state.participant.settings_revision
            || serde_json::to_value(&state.request.settings).map_err(|e| e.to_string())?
                != state.participant.settings
            || state.participant.provider.is_empty()
            || !state.request.control_results.is_empty()
            || state.request.approved_plan.is_some()
            || state.request.admission_id.is_some()
        {
            return Err("invalid owner observation request".into());
        }
        if let Some(record) = &state.dispatch {
            // Check the frozen broker approval even for completed or fenced work.
            ProviderDispatcher::new(record.clone())?;
            let method = &state.participant.plan_method;
            if record.call.service != method.service
                || record.call.method != method.method
                || record.call.interface_major != method.interface_major
                || record.call.interface_minor != method.interface_minor
                || record.call.operation_id != state.request.operation_id
                || record.call.approval.get("provider").and_then(Value::as_str)
                    != Some(state.participant.provider.as_str())
                || serde_json::to_value(&state.request).map_err(|e| e.to_string())?
                    != record.call.arguments
            {
                return Err("owner observation dispatch mismatch".into());
            }
        }
        match state.phase {
            PlanObservationPhase::Ready if state.plan.is_none() => {}
            PlanObservationPhase::Planned => {
                let Some(DispatchRecord {
                    state: DispatchState::Completed { payload },
                    ..
                }) = &state.dispatch
                else {
                    return Err("missing completed plan proof".into());
                };
                let response: ProviderResponse = serde_json::from_value(payload.clone())
                    .map_err(|_| "invalid owner plan response")?;
                api::validate_provider_response(&state.request, &response)?;
                if !matches!(&response, ProviderResponse::Planned { plan } if Some(plan) == state.plan.as_ref())
                {
                    return Err("owner plan proof mismatch".into());
                }
            }
            PlanObservationPhase::RecoveryRequired
                if state.plan.is_none() && state.dispatch.is_some() => {}
            _ => return Err("inconsistent owner observation state".into()),
        }
        let dispatcher = if state.phase == PlanObservationPhase::Ready {
            state
                .dispatch
                .clone()
                .map(ProviderDispatcher::new)
                .transpose()?
        } else {
            None
        };
        Ok(Self {
            state,
            dispatcher,
            journal_failed: false,
        })
    }

    pub fn state(&self) -> &PlanObservationState {
        &self.state
    }

    /// Runs at most one broker command per tick and never admits a plan.
    pub fn advance(
        &mut self,
        authority: &mut impl PrepareAuthority,
        journal: &mut impl PlanObservationJournal,
        send: &mut (impl FnMut(Command) -> ModuleResult<Response> + ?Sized),
    ) -> ModuleResult<()> {
        if self.journal_failed {
            return Err("owner observation journal requires recovery".into());
        }
        if self.state.phase != PlanObservationPhase::Ready {
            return Ok(());
        }
        if self.dispatcher.is_none() {
            let participant = &self.state.participant;
            let arguments = serde_json::to_value(&self.state.request).map_err(|e| e.to_string())?;
            let call = authority.approved_call(
                participant,
                &participant.plan_method,
                arguments.clone(),
                self.state.request.operation_id.clone(),
            )?;
            let mut next = self.state.clone();
            next.dispatch = Some(DispatchRecord {
                call,
                state: DispatchState::Ready,
            });
            // Reuse the same validation as restart, before persisting or sending.
            let _ = Self::new(next.clone())?;
            if let Err(error) = journal.persist(&next) {
                self.journal_failed = true;
                return Err(error);
            }
            self.state = next;
            self.dispatcher = Some(ProviderDispatcher::new(
                self.state.dispatch.clone().expect("dispatch set"),
            )?);
            return Ok(());
        }
        let dispatcher = self.dispatcher.as_mut().expect("dispatcher checked");
        let mut bridge = ObservationBridge {
            state: &mut self.state,
            journal,
            failed: &mut self.journal_failed,
        };
        dispatcher.advance(&mut bridge, send)?;
        if dispatcher.has_request() {
            return Ok(());
        }
        let mut next = self.state.clone();
        match &dispatcher.record().state {
            DispatchState::Completed { payload } => {
                let response: ProviderResponse = serde_json::from_value(payload.clone())
                    .map_err(|_| "invalid owner observation response")?;
                api::validate_provider_response(&next.request, &response)?;
                match response {
                    ProviderResponse::Planned { plan } => {
                        next.plan = Some(plan);
                        next.phase = PlanObservationPhase::Planned;
                    }
                    // Pending v2 status needs a persisted authenticated proof chain.
                    _ => next.phase = PlanObservationPhase::RecoveryRequired,
                }
            }
            DispatchState::OutcomeUnknown => next.phase = PlanObservationPhase::RecoveryRequired,
            DispatchState::Ready => return Ok(()),
        }
        if let Err(error) = journal.persist(&next) {
            self.journal_failed = true;
            return Err(error);
        }
        self.state = next;
        self.dispatcher = None;
        Ok(())
    }
}

struct ObservationBridge<'a, J> {
    state: &'a mut PlanObservationState,
    journal: &'a mut J,
    failed: &'a mut bool,
}
impl<J: PlanObservationJournal> DispatchJournal for ObservationBridge<'_, J> {
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
    use crate::adapter::MethodSettings;
    use crate::provider_dispatch::ProviderCall;
    use serde_json::json;

    #[derive(Default)]
    struct Journal {
        records: Vec<PlanObservationState>,
        fail: bool,
    }
    impl PlanObservationJournal for Journal {
        fn persist(&mut self, state: &PlanObservationState) -> ModuleResult<()> {
            if self.fail {
                return Err("storage failed".into());
            }
            self.records.push(state.clone());
            Ok(())
        }
    }
    struct Authority;
    impl PrepareAuthority for Authority {
        fn allocate(&mut self, _: &str) -> ModuleResult<(u64, String)> {
            panic!("sequence must already be reserved")
        }
        fn allocate_poll_id(&mut self, _: &str) -> ModuleResult<String> {
            panic!("no status polling")
        }
        fn approved_call(
            &mut self,
            participant: &ParticipantSettings,
            method: &MethodSettings,
            arguments: Value,
            operation_id: String,
        ) -> ModuleResult<ProviderCall> {
            Ok(ProviderCall {
                service: method.service.clone(),
                method: method.method.clone(),
                interface_major: method.interface_major,
                interface_minor: method.interface_minor,
                approval: json!({"provider":participant.provider,"method":{"service":method.service,"method":method.method,"interface_major":method.interface_major,"interface_minor":method.interface_minor}}),
                arguments,
                timeout_ms: 1000,
                operation_id,
            })
        }
        fn admit_plan(
            &mut self,
            _: &ParticipantSettings,
            _: &ResourcePlan,
        ) -> ModuleResult<String> {
            panic!("read-only planning must never admit")
        }
        fn validate_rights(
            &mut self,
            _: &ParticipantSettings,
            _: &ResourcePlan,
            _: &str,
        ) -> ModuleResult<()> {
            panic!("read-only planning has no rights")
        }
    }
    fn fixture() -> (PlanObservationState, Value) {
        let method = json!({"service":"owner/2","method":"resource_plan","interface_major":2,"interface_minor":0,"schema_revision":1});
        let participant = serde_json::from_value(json!({"provider":"owner","contract_version":2,"plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"revision","settings":{}})).unwrap();
        let request = serde_json::from_value(json!({"version":2,"provider_sequence":8,"operation":"resource_plan","transition_id":"transition","operation_id":"plan-op","target":{"kind":"stream","host_epoch":"epoch","session_id":"session","stream_id":"stream","generation":1},"phase":"stream.prepare","settings_revision":"revision","settings":{},"intent":"initial","reason":null,"requested_video":null,"consumer_requirements":{"execution_domain":"seat","capture_providers":["capture"],"selector_schemas":["selector/1"]},"remaining_ms":1000,"plan_revision":null,"admission_id":null,"approved_plan":null})).unwrap();
        let plan = json!({"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]});
        (
            PlanObservationState {
                participant,
                request,
                phase: PlanObservationPhase::Ready,
                dispatch: None,
                plan: None,
            },
            plan,
        )
    }
    #[test]
    fn persists_exact_plan_without_admission_or_callback() {
        let (state, plan) = fixture();
        let mut driver = PlanObservationDriver::new(state).unwrap();
        let mut journal = Journal::default();
        let mut calls = Vec::new();
        for _ in 0..10 {
            driver
                .advance(&mut Authority, &mut journal, &mut |command| {
                    calls.push(command.clone());
                    Ok(match command {
                        Command::QueryApiApproval { .. } => Response::Result {
                            payload: driver_approval(),
                        },
                        Command::CallApi { method, .. } => {
                            assert_eq!(method, "resource_plan");
                            Response::Accepted { id: 1 }
                        }
                        Command::TakeResult { .. } => Response::Result {
                            payload: json!({"status":"planned","plan":plan}),
                        },
                        Command::ReleaseResult { .. } => Response::Ok,
                        _ => panic!("unexpected command"),
                    })
                })
                .unwrap();
            if driver.state().phase == PlanObservationPhase::Planned {
                break;
            }
        }
        assert_eq!(driver.state().phase, PlanObservationPhase::Planned);
        assert!(journal.records.iter().any(|state| matches!(
            state.dispatch.as_ref().map(|d| &d.state),
            Some(DispatchState::OutcomeUnknown)
        )));
        assert_eq!(
            calls
                .iter()
                .filter(|command| matches!(command, Command::CallApi { .. }))
                .count(),
            1
        );
        let restored = PlanObservationDriver::new(
            serde_json::from_value(serde_json::to_value(driver.state()).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(restored.state().phase, PlanObservationPhase::Planned);
        let mut forged = restored.state().clone();
        forged.request.operation_id = "different".into();
        assert!(PlanObservationDriver::new(forged).is_err());
    }
    fn driver_approval() -> Value {
        json!({"provider":"owner","method":{"service":"owner/2","method":"resource_plan","interface_major":2,"interface_minor":0}})
    }
    #[test]
    fn recovered_unknown_and_pending_results_never_replay() {
        let (state, _) = fixture();
        let mut driver = PlanObservationDriver::new(state).unwrap();
        let mut journal = Journal::default();
        driver
            .advance(&mut Authority, &mut journal, &mut |_| {
                panic!("initial persistence does not send")
            })
            .unwrap();
        let mut recovered = driver.state().clone();
        recovered.dispatch.as_mut().unwrap().state = DispatchState::OutcomeUnknown;
        let mut restored = PlanObservationDriver::new(recovered).unwrap();
        restored
            .advance(&mut Authority, &mut journal, &mut |_| {
                panic!("uncertain call must not replay")
            })
            .unwrap();
        assert_eq!(
            restored.state().phase,
            PlanObservationPhase::RecoveryRequired
        );
        let (state, _) = fixture();
        let mut pending = PlanObservationDriver::new(state).unwrap();
        for _ in 0..10 {
            pending
                .advance(&mut Authority, &mut journal, &mut |command| {
                    Ok(match command {
                        Command::QueryApiApproval { .. } => Response::Result {
                            payload: driver_approval(),
                        },
                        Command::CallApi { .. } => Response::Accepted { id: 2 },
                        Command::TakeResult { .. } => Response::Result {
                            payload: json!({"status":"pending","receipt_id":"receipt"}),
                        },
                        Command::ReleaseResult { .. } => Response::Ok,
                        _ => panic!("unexpected command"),
                    })
                })
                .unwrap();
            if pending.state().phase == PlanObservationPhase::RecoveryRequired {
                break;
            }
        }
        assert_eq!(
            pending.state().phase,
            PlanObservationPhase::RecoveryRequired
        );
    }

    #[test]
    fn failed_persistence_fences_subsequent_dispatch() {
        let (state, _) = fixture();
        let mut driver = PlanObservationDriver::new(state).unwrap();
        let mut journal = Journal {
            fail: true,
            ..Journal::default()
        };
        assert!(
            driver
                .advance(&mut Authority, &mut journal, &mut |_| panic!(
                    "must not send"
                ))
                .is_err()
        );
        journal.fail = false;
        assert!(
            driver
                .advance(&mut Authority, &mut journal, &mut |_| panic!(
                    "fenced work must not send"
                ))
                .is_err()
        );
    }
}
