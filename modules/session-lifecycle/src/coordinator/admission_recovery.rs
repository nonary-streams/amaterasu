//! Release reservations whose persisted prepare state proves no effect was sent.
use super::*;
use crate::{
    prepare_authority::Admission, prepare_driver::PreparePhase, provider_dispatch::DispatchState,
};
use amaterasu_extension_api::{ProviderOperation, ProviderResponse};

fn uninvoked(state: &PrepareState, id: &str, admission: &Admission) -> bool {
    let admitted = state.phase == PreparePhase::Callback
        && state.admission_id.as_deref() == Some(id)
        && state.plan.as_ref() == Some(&admission.plan);
    let awaiting_checkpoint =
        state.phase == PreparePhase::Plan && state.admission_id.is_none() && state.plan.is_none();
    if !(admitted || awaiting_checkpoint)
        || state.provider != admission.provider
        || state.participant != admission.selection.participant
        || state.request.operation != ProviderOperation::ResourcePlan
        || state.request.admission_id.is_some()
        || state.request.approved_plan.is_some()
        || state.dispatch.is_some()
        || state.pending_dispatch.is_some()
        || state.pending_receipt.is_some()
        || state.response.is_some()
    {
        return false;
    }
    let Some(record) = &state.terminal_dispatch else {
        return false;
    };
    let method = &state.participant.plan_method;
    record.call.service == method.service
        && record.call.method == method.method
        && record.call.interface_major == method.interface_major
        && record.call.interface_minor == method.interface_minor
        && record.call.operation_id == state.request.operation_id
        && serde_json::to_value(&state.request).ok().as_ref() == Some(&record.call.arguments)
        && matches!(&record.state, DispatchState::Completed { payload }
            if serde_json::from_value::<ProviderResponse>(payload.clone()).ok()
                == Some(ProviderResponse::Planned { plan: admission.plan.clone() }))
}

/// Called only while reopening the exclusively locked journal. A callback must
/// have its dispatch record persisted before it can be sent. Keep every plan
/// with a callback record, output, control, or other retained proof fenced.
pub(super) fn release_undispatched(journal: &mut Journal) {
    let ids: Vec<_> = journal
        .authority
        .admissions
        .iter()
        .filter_map(|(id, admission)| {
            if journal.references_admission(id)
                || journal.authority.output_admissions.contains(id)
                || journal
                    .authority
                    .control_permits
                    .values()
                    .any(|p| p.request.admission_id == *id)
                || journal
                    .authority
                    .finalize_permits
                    .values()
                    .any(|p| p.request.admission_id == *id)
                || admission
                    .rights
                    .iter()
                    .any(|(domain, _)| journal.authority.domain_locks.get(domain) != Some(id))
            {
                return None;
            }
            let mut matching = 0;
            for receipt in journal.receipts.values() {
                let Some(work) = &receipt.work else { continue };
                let states = work.states.values().chain(
                    work.failed_preparation
                        .iter()
                        .flat_map(|failed| failed.states.values()),
                );
                for state in states {
                    let plan_matches = state.terminal_dispatch.as_ref().is_some_and(|record| {
                        matches!(&record.state, DispatchState::Completed { payload }
                            if serde_json::from_value::<ProviderResponse>(payload.clone()).ok()
                                == Some(ProviderResponse::Planned { plan: admission.plan.clone() }))
                    });
                    let callback_references = state.request.admission_id.as_ref() == Some(id)
                        || [
                            &state.dispatch,
                            &state.pending_dispatch,
                            &state.terminal_dispatch,
                        ]
                        .iter()
                        .any(|record| {
                            record.as_ref().is_some_and(|record| {
                                record.call.arguments["admission_id"].as_str() == Some(id.as_str())
                            })
                        });
                    if state.admission_id.as_ref() == Some(id)
                        || plan_matches
                        || callback_references
                    {
                        if !uninvoked(state, id, admission) {
                            return None;
                        }
                        matching += 1;
                    }
                }
            }
            (matching == 1).then(|| id.clone())
        })
        .collect();
    for id in ids {
        let admission = journal
            .authority
            .admissions
            .remove(&id)
            .expect("selected admission");
        for (domain, _) in admission.rights {
            journal.authority.domain_locks.remove(&domain);
            journal.authority.refresh_required.insert(domain);
        }
        journal.admission_transitions.remove(&id);
        for session in journal.sessions.values_mut() {
            session.admission_ids.remove(&id);
            session.admission_targets.remove(&id);
            session.admission_phases.remove(&id);
            session.prepared_plan_ids.remove(&id);
        }
        // Preserve failed prepare evidence and lifecycle phases. Releasing an
        // uninvoked reservation is not a claim that native cleanup completed.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        adapter::ParticipantSettings,
        provider_dispatch::{DispatchRecord, ProviderCall},
    };

    fn fixture() -> (PrepareState, Admission) {
        let method = |name: &str| {
            json!({"service":"example.provider/1", "method":name,
            "interface_major":1, "interface_minor":0, "schema_revision":1})
        };
        let participant: ParticipantSettings = serde_json::from_value(json!({
            "provider":"example.provider", "plan_method":method("resource_plan"),
            "callback_method":method("lifecycle_callback"), "status_method":method("lifecycle_status"),
            "capabilities":[], "cleanup_methods":[], "settings_revision":"settings", "settings":{}
        })).unwrap();
        let request = serde_json::from_value(json!({
            "version":1, "provider_sequence":1, "operation":"resource_plan", "operation_id":"plan-operation",
            "transition_id":"transition", "target":{"kind":"session", "host_epoch":"epoch", "session_id":"session", "generation":1},
            "phase":"session.prepare", "settings_revision":"settings", "settings":{}, "intent":null,
            "reason":null, "requested_video":null, "remaining_ms":5000, "plan_revision":null,
            "admission_id":null, "approved_plan":null,
            "consumer_requirements":{"execution_domain":"desktop", "capture_providers":[], "selector_schemas":[]}
        })).unwrap();
        let plan = serde_json::from_value(json!({"plan_id":"plan", "revision":1,
            "resources":[], "mutation_domains":[], "required_capabilities":[], "constraints":{},
            "obligations":[], "affected_consumers":[], "consumer_change":"live", "candidate_constraint":null, "allowed_degradation":[]
        })).unwrap();
        let mut state = PrepareState {
            provider: participant.provider.clone(),
            participant: participant.clone(),
            request,
            phase: PreparePhase::Callback,
            plan: Some(plan),
            admission_id: Some("admission".into()),
            pending_receipt: None,
            pending_dispatch: None,
            status_polls: 0,
            last_sequence: 1,
            dispatch: None,
            terminal_dispatch: None,
            response: None,
        };
        state.terminal_dispatch = Some(DispatchRecord {
            call: ProviderCall {
                service: "example.provider/1".into(),
                method: "resource_plan".into(),
                interface_major: 1,
                interface_minor: 0,
                approval: json!({}),
                arguments: serde_json::to_value(&state.request).unwrap(),
                timeout_ms: 5000,
                operation_id: "plan-operation".into(),
            },
            state: DispatchState::Completed {
                payload: json!(ProviderResponse::Planned {
                    plan: state.plan.clone().unwrap()
                }),
            },
        });
        let admission = Admission {
            provider: participant.provider.clone(),
            plan: state.plan.clone().unwrap(),
            rights: vec![],
            selection: ProviderSelection {
                participant,
                approvals: vec![],
                capabilities: Default::default(),
                cleanup_methods: vec![],
                authorized_borrowers: Default::default(),
                approved_control_bindings: vec![],
            },
        };
        (state, admission)
    }

    #[test]
    fn completed_plan_without_a_callback_proves_no_effect_was_dispatched() {
        let (state, admission) = fixture();
        assert!(uninvoked(&state, "admission", &admission));
        assert!(!uninvoked(&state, "another-admission", &admission));
        let mut changed = admission.clone();
        changed.plan.revision += 1;
        assert!(!uninvoked(&state, "admission", &changed));
        let mut awaiting_checkpoint = state;
        awaiting_checkpoint.phase = PreparePhase::Plan;
        awaiting_checkpoint.admission_id = None;
        awaiting_checkpoint.plan = None;
        assert!(uninvoked(&awaiting_checkpoint, "admission", &admission));
    }

    #[test]
    fn armed_or_uncertain_callbacks_keep_their_reservations() {
        let (state, admission) = fixture();
        for status in [
            DispatchState::Ready,
            DispatchState::OutcomeUnknown,
            DispatchState::Completed {
                payload: json!({"status":"prepared"}),
            },
        ] {
            let mut changed = state.clone();
            let mut callback = state.terminal_dispatch.clone().unwrap();
            callback.call.method = "lifecycle_callback".into();
            callback.state = status;
            changed.dispatch = Some(callback);
            assert!(!uninvoked(&changed, "admission", &admission));
        }
        let mut changed = state.clone();
        changed.request.operation = ProviderOperation::LifecycleCallback;
        assert!(!uninvoked(&changed, "admission", &admission));
        let mut changed = state;
        changed.terminal_dispatch.as_mut().unwrap().state = DispatchState::OutcomeUnknown;
        assert!(!uninvoked(&changed, "admission", &admission));
    }
}
