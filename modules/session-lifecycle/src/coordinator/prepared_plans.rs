//! Read-only, capability-neutral handoff of completed session preparation.
use super::*;
use amaterasu_extension_api::{PreparedPlan, PreparedPlansRequest, PreparedPlansResponse};

impl Coordinator {
    pub(super) fn read_prepared_plans(&self, caller: &str, payload: Value) -> Value {
        if !self.settings.trusted_callers.contains(caller)
            || !self
                .settings
                .operation_grants
                .get(caller)
                .is_some_and(|operations| operations.contains("read_prepared_plans"))
        {
            return rejection("permission_denied");
        }
        let Ok(request) = serde_json::from_value::<PreparedPlansRequest>(payload) else {
            return rejection("invalid_request");
        };
        if request.operation != "read_prepared_plans" {
            return rejection("invalid_request");
        }
        let LifecycleTarget::Session {
            host_epoch,
            session_id,
            generation,
        } = &request.target
        else {
            return rejection("session_target_required");
        };
        let shared = self.shared.borrow();
        let journal = &shared.value;
        let Some(session) = journal.sessions.get(session_id) else {
            return rejection("unknown_session");
        };
        if journal.host_epoch != *host_epoch
            || session.host_epoch != *host_epoch
            || session.generation != *generation
            || session.owner.caller != request.owner
            || session.application_operation_id.as_deref()
                != Some(request.application_operation_id.as_str())
            || session.phase != SessionPhase::Ready
            || session.application_retired
            || session.application.is_some()
        {
            return rejection("launch_context_mismatch");
        }
        let mut plans = Vec::new();
        let providers = session
            .provider_graph
            .as_ref()
            .map(|graph| graph.order.as_slice())
            .unwrap_or(&[]);
        for provider in providers {
            let mut admissions = session.admission_ids.iter().filter_map(|id| {
                (session.admission_targets.get(id) == Some(&request.target)
                    && session.admission_phases.get(id) == Some(&LifecyclePhase::SessionPrepare))
                .then(|| journal.authority.settled.get(id))
                .flatten()
                .filter(|admission| &admission.provider == provider)
                .map(|admission| (id, admission))
            });
            // Empty no-effect plans need no retained handoff.
            let Some((id, admission)) = admissions.next() else {
                continue;
            };
            if admissions.next().is_some() {
                return rejection("ambiguous_preparation");
            }
            if !session.prepared_plan_ids.contains(id) {
                return rejection("preparation_not_settled");
            }
            // The marker is written only after the authenticated callback
            // reports Prepared. Settlement by cleanup is insufficient.
            plans.push(PreparedPlan {
                provider: provider.clone(),
                plan: admission.plan.clone(),
            });
        }
        let result = serde_json::to_value(PreparedPlansResponse {
            status: "succeeded".into(),
            target: request.target,
            plans,
        })
        .expect("serializable prepared plans");
        if !serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() <= 60 * 1024) {
            return rejection("prepared_context_too_large");
        }
        result
    }
}
