//! Read-only target descriptions for installed resource providers. The result
//! supplies planning context; it never authorizes a native effect.
use super::*;
use amaterasu_extension_api::ActionTarget;

fn unique_admission_generation(generations: &[Option<u64>], root_generation: u64) -> Option<u64> {
    (root_generation > 0 && generations == [Some(root_generation)]).then_some(root_generation)
}

/// Attach only generation values frozen in authenticated owner admission evidence.
fn attach_admitted_generations(
    journal: &Journal,
    root: &super::composition::StoredOutput,
    metadata: &mut Value,
) -> ModuleResult<()> {
    let root_generation = super::resource_generation::current_root_generation(journal, root)?;
    let entries = metadata["admitted_plans"]
        .as_array_mut()
        .ok_or("admitted_plans_invalid")?;
    for entry in entries {
        let id = entry["admission_id"]
            .as_str()
            .ok_or("admission_id_missing")?;
        let admission = journal
            .authority
            .settled
            .get(id)
            .ok_or("admission_unsettled")?;
        if admission.provider != root.output.producer
            || entry["plan"] != serde_json::to_value(&admission.plan).map_err(|_| "plan_invalid")?
        {
            return Err("admission_mismatch".into());
        }
        let mut generations = Vec::new();
        if id == root.admission_id {
            let proof = root
                .callback_proof
                .as_ref()
                .ok_or("root_callback_missing")?;
            generations.push(proof.request.resource_generation);
        }
        for stage in journal
            .composition
            .staged_outputs()
            .filter(|stage| stage.admission_id == id)
        {
            if !journal.composition.validate_structure() {
                return Err("stage_provenance_invalid".into());
            }
            if stage.before.producer != root.output.producer
                || stage.before.output_id != root.output.output_id
                || stage.before.origin != root.output.origin
                || entry["target"]
                    != serde_json::to_value(&stage.target).map_err(|_| "target_invalid")?
                || entry["plan"] != serde_json::to_value(&stage.plan).map_err(|_| "plan_invalid")?
            {
                return Err("stage_admission_mismatch".into());
            }
            if stage.plan != admission.plan
                || stage.callback_proof.participant != admission.selection.participant
                || !super::composition::callback_approvals_valid(
                    &stage.callback_proof,
                    &admission.selection.approvals,
                    &admission.provider,
                )
            {
                return Err("stage_admission_unproven".into());
            }
            let previous = journal
                .composition
                .staged_outputs()
                .take_while(|item| *item != stage)
                .filter(|item| {
                    item.after.producer == root.output.producer
                        && item.after.output_id == root.output.output_id
                        && item.after.origin == root.output.origin
                })
                .last();
            let before_valid = match (previous, &stage.prior_release_admission) {
                (None, None) => stage.before == root.output,
                (Some(previous), Some(prior)) => journal
                    .verified_released_output(&previous.target)
                    .is_some_and(|(admission, output)| {
                        admission == prior && output == &stage.before
                    }),
                _ => false,
            };
            if !before_valid {
                return Err("stage_lineage_unproven".into());
            }
            generations.push(stage.callback_proof.request.resource_generation);
        }
        for result in journal
            .finalize_results
            .records()
            .filter(|result| result.request.admission_id == id)
        {
            if result.selection.input != *root
                || entry["target"]
                    != serde_json::to_value(&result.request.target).map_err(|_| "target_invalid")?
                || entry["plan"]
                    != serde_json::to_value(&result.plan).map_err(|_| "plan_invalid")?
            {
                return Err("finalize_admission_mismatch".into());
            }
            let permit = journal
                .authority
                .finalize_permits
                .get(&result.request.permit_id)
                .ok_or("finalize_permit_missing")?;
            if !result.validate_structure()
                || !result.validate_control_chain(journal)
                || !permit.consumed
                || !permit.settled
                || permit.request != result.request
                || permit.context.target != result.request.target
                || permit.context.transition_id != result.request.transition_id
                || permit.context.policy != result.selection.policy
                || permit.context.output != result.request.output
                || permit.context.control_results != result.request.control_results
                || permit.context.parent_generation != result.selection.parent_generation
                || journal.composition.finalize_selection(
                    &result.request.target,
                    &result.request.transition_id,
                    &admission.provider,
                    &result.selection.input.output.output_id,
                ) != Some(&result.selection)
                || result.plan != admission.plan
                || !journal.authority.output_admissions.contains(id)
                || result.plan_approval.as_ref().is_none_or(|approval| {
                    !admission.selection.approvals.contains(approval)
                        || approval.method != admission.selection.participant.plan_method
                })
            {
                return Err("finalize_admission_unproven".into());
            }
            generations.push(result.plan_request.resource_generation);
        }
        let generation = unique_admission_generation(&generations, root_generation)
            .ok_or("admission_generation_unavailable")?;
        entry
            .as_object_mut()
            .ok_or("admitted_plan_invalid")?
            .insert("resource_generation".into(), json!(generation));
    }
    Ok(())
}

impl Coordinator {
    /// Resolve the newest owner output only from a finalized stage or a verified
    /// release of that exact stage. The immutable root remains the authority.
    fn action_owner_output(
        &self,
        journal: &Journal,
        root: &super::composition::StoredOutput,
        selected_stream: Option<&str>,
    ) -> ModuleResult<amaterasu_extension_api::ResourceOutput> {
        let stage = journal.composition.staged_outputs().rfind(|stage| {
            stage.before.producer == root.output.producer
                && stage.before.output_id == root.output.output_id
                && stage.before.origin == root.output.origin
        });
        let Some(stage) = stage else {
            if selected_stream.is_some()
                || journal
                    .finalize_results
                    .records()
                    .any(|result| result.selection.input == *root)
            {
                return Err("owner_stage_missing".into());
            }
            return Ok(root.output.clone());
        };
        let (_, session_id, Some(stream_id), generation) =
            super::operations::target_parts(&stage.target)?
        else {
            return Err("owner_stage_target_invalid".into());
        };
        if selected_stream.is_some_and(|selected| selected != stream_id) {
            return Err("owner_stage_not_current".into());
        }
        let session = journal
            .sessions
            .get(session_id)
            .ok_or("owner_session_missing")?;
        let stream = session
            .streams
            .get(stream_id)
            .ok_or("owner_stream_missing")?;
        if stream.generation != generation
            || stream.binding_owner != root.output.producer
            || session.streams.iter().any(|(id, other)| {
                other.binding_owner == root.output.producer
                    && other.phase == StreamPhase::Active
                    && id != stream_id
            })
        {
            return Err("owner_stage_stale".into());
        }
        let output = if stream.phase == StreamPhase::Retired {
            journal
                .verified_released_output(&stage.target)
                .ok_or("owner_release_unproven")?
                .1
                .clone()
        } else {
            let mut results = journal
                .finalize_results
                .records()
                .filter(|result| result.request.target == stage.target);
            let result = results.next().ok_or("owner_finalize_missing")?;
            if results.next().is_some() || result.selection.input != *root
                || result.selection.stage.as_ref() != Some(stage)
                || !result.validate_control_chain(journal)
                || self.verify_finalize_result(&root.output.producer, json!({
                    "operation":"verify_finalize_result", "request":result.request, "response":result.applied
                }))["status"] != "valid"
            {
                return Err("owner_finalize_unproven".into());
            }
            let amaterasu_extension_api::ResourceControlResponse::Applied { output, .. } =
                &result.applied
            else {
                return Err("owner_finalize_unproven".into());
            };
            output.as_ref().clone()
        };
        amaterasu_extension_api::validate_output_refinement(&root.output, &output)?;
        if output.resource.revision < stage.after.resource.revision {
            return Err("owner_output_stale".into());
        }
        Ok(output)
    }

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
        if !journal
            .composition
            .records()
            .any(|record| record.output.producer == caller && record.output.origin == origin)
        {
            return rejection("owner_output_missing");
        }
        let mut active = journal.composition.records().filter(|record| {
            record.output.producer == caller
                && record.output.origin == origin
                && record.state == super::composition::OutputState::Retained
        });
        let Some(record) = active.next() else {
            return rejection("owner_output_ambiguous_or_unavailable");
        };
        if active.next().is_some() {
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
        let Ok(owner_output) = self.action_owner_output(journal, record, selected_stream) else {
            return rejection("owner_output_lineage_unavailable");
        };
        let Some(snapshot) = session.provider_settings.get(caller) else {
            return rejection("settings_snapshot_missing");
        };
        let resource = &owner_output.resource;
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
        let Ok(mut metadata) = super::provider_observations::session_metadata(
            journal,
            session,
            caller,
            &scoped_domains,
        ) else {
            return rejection("provider_metadata_unavailable");
        };
        if attach_admitted_generations(journal, record, &mut metadata).is_err() {
            return rejection("admission_generation_unavailable");
        }
        let streams: Vec<_> = session.streams.iter().filter(|(_, stream)| stream.binding_owner == caller).map(|(stream_id, stream)| {
            json!({"target":{"kind":"stream","host_epoch":session.host_epoch,"session_id":session.id,"stream_id":stream_id,"expected_generation":stream.generation},"lifecycle_target":{"kind":"stream","host_epoch":session.host_epoch,"session_id":session.id,"stream_id":stream_id,"generation":stream.generation},"phase":stream.phase,"candidate":stream.candidate,"consumers_retired":stream.phase==StreamPhase::Retired || (stream.phase==StreamPhase::Quiescing && stream.retirements==stream.required)})
        }).collect();
        let result = json!({"version":2,"status":"succeeded","provider":caller,"target":target,"session_target":origin,"execution_domain":resource.execution_domain,"settings_revision":snapshot.revision,"settings":snapshot.settings,"streams":streams,"obligations":metadata["obligations"],"mutation_domains":mutation_domains,"admitted_plans":metadata["admitted_plans"],"resource_targets":metadata["resource_targets"],"stale_mutation_domains":metadata["stale_mutation_domains"],"owner_output":owner_output,"resource_generation":resource_generation});
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

#[cfg(test)]
mod admission_generation_tests {
    use super::*;

    #[test]
    fn unavailable_root_generation_cannot_be_claimed() {
        let journal: Journal = serde_json::from_value(
            json!({"version":3,"host_epoch":"epoch","receipts":{},"sessions":{}}),
        )
        .unwrap();
        let selected = super::super::finalize_selection::tests::fixture();
        let mut metadata = json!({"admitted_plans":[{"admission_id":selected.input.admission_id,"target":selected.input.output.origin,"plan":selected.input.plan}]});
        assert!(attach_admitted_generations(&journal, &selected.input, &mut metadata).is_err());
        assert!(
            metadata["admitted_plans"][0]
                .get("resource_generation")
                .is_none()
        );
    }

    #[test]
    fn generation_evidence_must_be_unique_present_and_current() {
        assert_eq!(unique_admission_generation(&[Some(1)], 1), Some(1));
        assert_eq!(unique_admission_generation(&[], 1), None);
        assert_eq!(unique_admission_generation(&[None], 1), None);
        assert_eq!(unique_admission_generation(&[Some(2)], 1), None);
        assert_eq!(unique_admission_generation(&[Some(1), Some(1)], 1), None);
        assert_eq!(unique_admission_generation(&[Some(1), Some(2)], 1), None);
        assert_eq!(unique_admission_generation(&[Some(0)], 0), None);
    }
}

#[cfg(test)]
mod owner_output_tests {
    use super::*;

    #[test]
    fn unproven_stream_never_uses_immutable_root_as_current_output() {
        let path =
            std::env::temp_dir().join(format!("action-owner-output-{}", identifier().unwrap()));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let coordinator = Coordinator::open(&path.join("journal")).unwrap();
        let selected = super::super::finalize_selection::tests::fixture();
        let journal = coordinator.shared.borrow().value.clone();
        assert!(
            coordinator
                .action_owner_output(&journal, &selected.input, Some("unknown"))
                .is_err()
        );
        drop(coordinator);
        std::fs::remove_dir_all(path).unwrap();
    }
}
