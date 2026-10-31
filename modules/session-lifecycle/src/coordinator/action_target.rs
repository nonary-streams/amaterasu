//! Read-only target descriptions for installed resource providers. The result
//! supplies planning context; it never authorizes a native effect.
use super::*;
use amaterasu_extension_api::ActionTarget;

impl Coordinator {
    pub(super) fn describe_action_target(&self, caller: &str, payload: &Value) -> Value {
        if !self
            .settings
            .participants
            .iter()
            .any(|participant| participant.provider == caller)
        {
            return rejection("provider_not_selected");
        }
        let Ok(target) = serde_json::from_value::<ActionTarget>(payload["target"].clone()) else {
            return rejection("invalid_target");
        };
        let (epoch, id, selected_stream, generation) = match &target {
            ActionTarget::Session {
                host_epoch,
                session_id,
                expected_generation,
            } => (host_epoch, session_id, None, *expected_generation),
            ActionTarget::Stream {
                host_epoch,
                session_id,
                stream_id,
                expected_generation,
            } => (
                host_epoch,
                session_id,
                Some(stream_id.as_str()),
                *expected_generation,
            ),
            ActionTarget::Host { .. } => return rejection("session_target_required"),
        };
        let shared = self.shared.borrow();
        let journal = &shared.value;
        let Some(session) = journal.sessions.get(id) else {
            return rejection("unknown_session");
        };
        if epoch != &journal.host_epoch || epoch != &session.host_epoch {
            return rejection("stale_host_epoch");
        }
        if !matches!(session.phase, SessionPhase::Ready | SessionPhase::Paused) {
            return rejection("target_not_active");
        }
        let selected = if let Some(stream_id) = selected_stream {
            let Some(stream) = session.streams.get(stream_id) else {
                return rejection("unknown_stream");
            };
            if stream.generation != generation
                || stream.binding_owner != caller
                || stream.phase != StreamPhase::Active
            {
                return rejection("target_not_owned");
            }
            Some((stream_id, stream))
        } else {
            if session.generation != generation {
                return rejection("stale_generation");
            }
            let owned: Vec<_> = session
                .streams
                .iter()
                .filter(|(_, stream)| {
                    stream.binding_owner == caller && stream.phase == StreamPhase::Active
                })
                .collect();
            if owned.len() != 1 {
                return rejection("single_active_stream_required");
            }
            owned.first().map(|(id, stream)| (id.as_str(), *stream))
        };
        let snapshot = selected.and_then(|(_, stream)| stream.provider_settings.get(caller));
        let Some(snapshot) = snapshot else {
            return rejection("settings_snapshot_missing");
        };
        let obligations: Vec<_> = session
            .obligations
            .iter()
            .filter(|obligation| obligation.provider == caller)
            .collect();
        if obligations.is_empty() && selected.is_none() {
            return rejection("target_not_owned");
        }
        let plans: Vec<_> = session
            .admission_ids
            .iter()
            .filter_map(|id| {
                journal
                    .authority
                    .admissions
                    .get(id)
                    .or_else(|| journal.authority.settled.get(id))
            })
            .filter(|admission| admission.provider == caller)
            .map(|admission| &admission.plan)
            .collect();
        let resource = selected
            .and_then(|(_, stream)| stream.candidate.as_ref())
            .map(|candidate| &candidate.resource);
        let Some(resource) = resource else {
            return rejection("candidate_missing");
        };
        let domains: BTreeSet<_> = resource
            .mutation_domains
            .iter()
            .chain(resource.mutation_domain.iter())
            .collect();
        let mut mutation_domains = BTreeMap::new();
        for domain in domains {
            let Some(state) = journal.authority.snapshot.domains.get(domain) else {
                return rejection("domain_observation_missing");
            };
            if journal
                .authority
                .domain_owners
                .get(domain)
                .is_none_or(|owner| owner != caller)
            {
                return rejection("domain_not_owned");
            }
            mutation_domains.insert(domain, state);
        }
        let execution_domain = selected
            .and_then(|(_, stream)| stream.candidate.as_ref())
            .map(|candidate| candidate.resource.execution_domain.as_str());
        let Some(execution_domain) = execution_domain else {
            return rejection("candidate_missing");
        };
        let streams: Vec<_> = session.streams.iter().filter(|(_, stream)| stream.binding_owner == caller).map(|(stream_id, stream)| {
            json!({"target":{"kind":"stream","host_epoch":session.host_epoch,"session_id":session.id,"stream_id":stream_id,"expected_generation":stream.generation},"lifecycle_target":{"kind":"stream","host_epoch":session.host_epoch,"session_id":session.id,"stream_id":stream_id,"generation":stream.generation},"phase":stream.phase,"candidate":stream.candidate,"consumers_retired":stream.phase==StreamPhase::Retired || (stream.phase==StreamPhase::Quiescing && stream.retirements==stream.required)})
        }).collect();
        json!({"status":"succeeded","provider":caller,"target":target,"session_target":{"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"generation":session.generation},"execution_domain":execution_domain,"settings_revision":snapshot.revision,"settings":snapshot.settings,"streams":streams,"obligations":obligations,"plans":plans,"mutation_domains":mutation_domains})
    }
}
