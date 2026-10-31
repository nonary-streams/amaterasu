//! Read-only target descriptions for installed resource providers. The result
//! supplies planning context; it never authorizes a native effect.
use super::*;
use amaterasu_extension_api::ActionTarget;

impl Coordinator {
    pub(super) fn describe_action_target(&self, caller: &str, payload: &Value) -> Value {
        match payload.get("version") {
            None | Some(Value::Number(_))
                if payload.get("version").is_none() || payload["version"] == 1 => {}
            Some(value) if value == 2 => return self.describe_action_target_v2(caller, payload),
            _ => return rejection("unsupported_version"),
        }
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
        let Ok(metadata) = super::provider_observations::metadata(journal, session, caller) else {
            return rejection("provider_metadata_unavailable");
        };
        let result = json!({"status":"succeeded","provider":caller,"target":target,"session_target":{"kind":"session","host_epoch":session.host_epoch,"session_id":session.id,"generation":session.generation},"execution_domain":execution_domain,"settings_revision":snapshot.revision,"settings":snapshot.settings,"streams":streams,"obligations":obligations,"mutation_domains":mutation_domains,"admitted_plans":metadata["admitted_plans"],"resource_targets":metadata["resource_targets"],"stale_mutation_domains":metadata["stale_mutation_domains"]});
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > 16 * 1024) {
            return rejection("provider_metadata_too_large");
        }
        result
    }

    fn describe_action_target_v2(&self, caller: &str, payload: &Value) -> Value {
        if !self
            .settings
            .participants
            .iter()
            .any(|p| p.provider == caller)
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
        let replan = selected_stream.is_some()
            && self.is_exact_replan_description(journal, caller, payload, &target);
        if let Some(stream_id) = selected_stream {
            if session.streams.get(stream_id).is_none_or(|stream| {
                stream.generation != generation
                    || stream.binding_owner != caller
                    || (stream.phase != StreamPhase::Active
                        && !(replan && stream.phase == StreamPhase::Retired))
            }) {
                return rejection("target_not_owned");
            }
        } else if session.generation != generation {
            return rejection("stale_generation");
        }
        if session.phase == SessionPhase::Paused
            && ((selected_stream.is_some() && !replan)
                || session
                    .streams
                    .values()
                    .any(|s| s.phase != StreamPhase::Retired))
        {
            return rejection("consumers_not_retired");
        }
        let origin = LifecycleTarget::Session {
            host_epoch: epoch.clone(),
            session_id: id.clone(),
            generation: session.generation,
        };
        let mut outputs = journal
            .composition
            .records()
            .filter(|record| record.output.producer == caller && record.output.origin == origin);
        let Some(record) = outputs.next() else {
            return rejection("owner_output_missing");
        };
        if outputs.next().is_some() || record.state != super::composition::OutputState::Retained {
            return rejection("owner_output_ambiguous_or_unavailable");
        }
        if amaterasu_extension_api::validate_resource_output(&record.output).is_err() {
            return rejection("owner_output_invalid");
        }
        let Some(admission) = journal.authority.settled.get(&record.admission_id) else {
            return rejection("owner_output_unsettled");
        };
        if admission.provider != caller
            || admission.plan != record.plan
            || session.admission_targets.get(&record.admission_id) != Some(&origin)
            || session.admission_phases.get(&record.admission_id)
                != Some(&LifecyclePhase::SessionPrepare)
            || journal.admission_transitions.get(&record.admission_id)
                != Some(&record.transition_id)
            || !session.admission_ids.contains(&record.admission_id)
        {
            return rejection("owner_output_provenance_invalid");
        }
        let Ok(resource_generation) =
            super::resource_generation::current_root_generation(journal, record)
        else {
            return rejection("owner_output_generation_unavailable");
        };
        let Some(snapshot) = session.provider_settings.get(caller) else {
            return rejection("settings_snapshot_missing");
        };
        let resource = &record.output.resource;
        if let Some(stream_id) = selected_stream
            && session.streams[stream_id]
                .candidate
                .as_ref()
                .is_none_or(|candidate| {
                    candidate.resource.provider != resource.provider
                        || candidate.resource.resource_id != resource.resource_id
                        || candidate.resource.incarnation != resource.incarnation
                        || candidate.resource.execution_domain != resource.execution_domain
                })
        {
            return rejection("owner_resource_mismatch");
        }
        let domains: BTreeSet<_> = resource
            .mutation_domains
            .iter()
            .chain(resource.mutation_domain.iter())
            .collect();
        let scoped_domains: BTreeSet<String> =
            domains.iter().map(|domain| (*domain).clone()).collect();
        if domains.is_empty() || resource.provider != caller {
            return rejection("owner_resource_invalid");
        }
        let Some(observed) = journal
            .authority
            .snapshot
            .resources
            .get(caller)
            .and_then(|items| items.get(&resource.resource_id))
        else {
            return rejection("resource_observation_missing");
        };
        if observed.incarnation != resource.incarnation
            || observed.execution_domain != resource.execution_domain
            || observed.revision < resource.revision
            || observed.mutation_domains != domains.iter().map(|d| (*d).clone()).collect()
        {
            return rejection("resource_observation_mismatch");
        }
        let mut mutation_domains = BTreeMap::new();
        for domain in domains {
            if journal
                .authority
                .domain_owners
                .get(domain)
                .is_none_or(|owner| owner != caller)
            {
                return rejection("domain_not_owned");
            }
            let Some(state) = journal.authority.snapshot.domains.get(domain) else {
                return rejection("domain_observation_missing");
            };
            for (borrower, revision) in &state.borrowers {
                let (borrowed_target, phase) = if borrower == id && *revision == session.generation
                {
                    (origin.clone(), LifecyclePhase::SessionPrepare)
                } else if session.streams.get(borrower).is_some_and(|stream| {
                    stream.generation == *revision && stream.binding_owner == caller
                }) {
                    (
                        LifecycleTarget::Stream {
                            host_epoch: epoch.clone(),
                            session_id: id.clone(),
                            stream_id: borrower.clone(),
                            generation: *revision,
                        },
                        LifecyclePhase::StreamPrepare,
                    )
                } else {
                    return rejection("foreign_borrower");
                };
                if !session.admission_ids.iter().any(|admission_id| {
                    session.admission_targets.get(admission_id) == Some(&borrowed_target)
                        && session.admission_phases.get(admission_id) == Some(&phase)
                        && journal
                            .authority
                            .settled
                            .get(admission_id)
                            .is_some_and(|admission| {
                                admission.provider == caller
                                    && admission.plan.resources.iter().any(|planned| {
                                        planned.provider == caller
                                            && planned.resource_id == resource.resource_id
                                    })
                            })
                }) {
                    return rejection("borrower_provenance_missing");
                }
            }
            mutation_domains.insert(domain, state);
        }
        let Ok(metadata) = super::provider_observations::session_metadata(
            journal,
            session,
            caller,
            &scoped_domains,
        ) else {
            return rejection("provider_metadata_unavailable");
        };
        let streams: Vec<_> = session.streams.iter().filter(|(_, stream)| stream.binding_owner == caller).map(|(stream_id, stream)| {
            json!({"target":{"kind":"stream","host_epoch":session.host_epoch,"session_id":session.id,"stream_id":stream_id,"expected_generation":stream.generation},"lifecycle_target":{"kind":"stream","host_epoch":session.host_epoch,"session_id":session.id,"stream_id":stream_id,"generation":stream.generation},"phase":stream.phase,"candidate":stream.candidate,"consumers_retired":stream.phase==StreamPhase::Retired || (stream.phase==StreamPhase::Quiescing && stream.retirements==stream.required)})
        }).collect();
        let result = json!({"version":2,"status":"succeeded","provider":caller,"target":target,"session_target":origin,"execution_domain":resource.execution_domain,"settings_revision":snapshot.revision,"settings":snapshot.settings,"streams":streams,"obligations":metadata["obligations"],"mutation_domains":mutation_domains,"admitted_plans":metadata["admitted_plans"],"resource_targets":metadata["resource_targets"],"stale_mutation_domains":metadata["stale_mutation_domains"],"owner_output":record.output,"resource_generation":resource_generation});
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > 16 * 1024) {
            return rejection("provider_metadata_too_large");
        }
        result
    }

    /// IDs only select a unique durable transfer; they do not establish
    /// authority. The broker-authenticated provider and retained proof do.
    fn is_exact_replan_description(
        &self,
        journal: &Journal,
        caller: &str,
        payload: &Value,
        target: &ActionTarget,
    ) -> bool {
        let (Some(operation_id), Some(action_id)) = (
            payload["operation_id"].as_str(),
            payload["action_id"].as_str(),
        ) else {
            return false;
        };
        let Ok(target_value) = serde_json::to_value(target) else {
            return false;
        };
        let mut matches = journal.receipts.values().filter(|receipt| {
            let Some(work) = &receipt.work else {
                return false;
            };
            let Some(transfer) = &work.action_transfer else {
                return false;
            };
            if receipt.caller != self.settings.action_service
                || transfer.service != self.settings.action_service
                || transfer.binding.operation_id != operation_id
                || transfer.binding.action_id != action_id
                || serde_json::to_value(&transfer.binding.target).ok().as_ref()
                    != Some(&target_value)
            {
                return false;
            }
            true
        });
        let Some(receipt) = matches.next() else {
            return false;
        };
        if matches.next().is_some() {
            return false;
        }
        let Some(work) = &receipt.work else {
            return false;
        };
        let Some(transfer) = &work.action_transfer else {
            return false;
        };
        transfer.stage == ActionTransferStage::ReplanRequired
            && journal
                .actions
                .execution_for_permit(&transfer.service, &transfer.permit)
                .is_ok_and(|(provider, _, _)| provider == caller)
            && super::action_release_proof::validate_replan_stage(journal, receipt, work, transfer)
    }
}
