//! Coordinator-owned dependent input selection before any v2 provider call.
//!
//! A selected output is data, not authority. Mutations still require the
//! separately admitted plan, owner permit, live retirement and settlement.
use super::composition::{CompositionSelectionSnapshot, OutputSelection};
use super::composition_progress::CompositionProgress;
use super::*;
use amaterasu_extension_api::{MethodRef, ResourceOutput};

fn approval_for(
    selected: &ProviderSelection,
    method: &MethodRef,
) -> ModuleResult<crate::prepare_authority::MethodApproval> {
    let mut found = selected.approvals.iter().filter(|entry| {
        entry.method.service == method.service
            && entry.method.method == method.method
            && entry.method.interface_major == method.interface_major
            && entry.method.interface_minor == method.interface_minor
            && entry.method.schema_revision == method.schema_revision
    });
    let approval = found.next().ok_or("control_method_not_approved")?;
    if found.next().is_some() {
        return Err("ambiguous_control_approval".into());
    }
    Ok(approval.clone())
}

impl Coordinator {
    /// Freeze policy-selected direct dependency inputs and their exact method
    /// approvals before asking the dependent to plan. No native effects occur.
    pub(super) fn freeze_dependent_inputs(
        &mut self,
        key: &str,
        work: &Work,
        dependent: &str,
    ) -> ModuleResult<Vec<ResourceOutput>> {
        if work.phase != LifecyclePhase::StreamPrepare || work.waiting_retirement {
            return Err("dependent_target_not_ready".into());
        }
        let graph = work
            .provider_graph
            .as_ref()
            .ok_or("provider_graph_missing")?;
        let dependencies = graph
            .dependencies
            .get(dependent)
            .ok_or("dependent_not_in_graph")?;
        if dependencies.is_empty() {
            return Err("dependent_has_no_inputs".into());
        }
        let journal = self.shared.borrow().value.clone();
        // The canonical journal checks connect producer callbacks and stages to
        // their admitted targets before any selection becomes an input.
        validate_journal(&journal).map_err(|_| "invalid_composition_journal")?;
        let (_, session_id, stream_id, generation) = super::operations::target_parts(&work.target)?;
        let session = journal.sessions.get(session_id).ok_or("unknown_session")?;
        let stream = stream_id
            .and_then(|id| session.streams.get(id))
            .ok_or("unknown_stream")?;
        if journal.host_epoch != session.host_epoch
            || stream.generation != generation
            || stream.phase != StreamPhase::Preparing
            || !matches!(session.phase, SessionPhase::Ready | SessionPhase::Paused)
        {
            return Err("dependent_target_not_ready".into());
        }
        let policies: Vec<_> = self
            .settings
            .control_policies
            .iter()
            .filter(|policy| policy.dependent_provider == dependent)
            .collect();
        if policies.is_empty() || policies.len() > 32 {
            return Err("dependent_policy_unavailable".into());
        }
        let mut frozen = Vec::new();
        let mut inputs = Vec::new();
        let mut selected_outputs = Vec::new();
        for policy in policies {
            if !dependencies.contains(&policy.owner_provider) {
                return Err("control_owner_not_direct_dependency".into());
            }
            let owner = self
                .selections
                .get(&policy.owner_provider)
                .ok_or("control_owner_unavailable")?;
            let mut finalize = self.settings.finalize_policies.iter().filter(|item| {
                item.owner_provider == policy.owner_provider
                    && item.owner_service == policy.owner_service
                    && item.output_capability == policy.output_capability
                    && item.output_schema == policy.output_schema
            });
            let finalize_policy = finalize.next().ok_or("finalize_policy_missing")?;
            if finalize.next().is_some() {
                return Err("ambiguous_finalize_policy".into());
            }
            let wanted = OutputSelection {
                producer: policy.owner_provider.clone(),
                service: policy.owner_service.clone(),
                capability: policy.output_capability.clone(),
                schema: policy.output_schema.clone(),
            };
            let input = journal
                .composition
                .inputs(&work.target, &[wanted])
                .map_err(str::to_owned)?
                .pop()
                .ok_or("dependency_input_missing")?;
            if inputs.iter().any(|old: &super::composition::StoredOutput| {
                old.output.producer == input.output.producer
                    && old.output.output_id == input.output.output_id
            }) {
                return Err("ambiguous_control_policy".into());
            }
            let current = match &input.output.origin {
                LifecycleTarget::Session {
                    generation: parent, ..
                } => {
                    if *parent != session.generation {
                        return Err("dependency_parent_stale".into());
                    }
                    let stage = journal
                        .composition
                        .staged_for(
                            &work.target,
                            &input.output.producer,
                            &input.output.output_id,
                        )
                        .map_err(str::to_owned)?;
                    if stage.parent_generation != session.generation {
                        return Err("dependency_stage_stale".into());
                    }
                    stage.after.clone()
                }
                _ => input.output.clone(),
            };
            let selected = CompositionSelectionSnapshot {
                target: work.target.clone(),
                transition_id: work.transition_id.clone(),
                input: input.clone(),
                control_policy: policy.clone(),
                finalize_policy: finalize_policy.clone(),
                control_approval: approval_for(owner, &policy.owner_method)?,
                control_status_approval: approval_for(owner, &policy.status_method)?,
                settlement_approval: approval_for(owner, &policy.settlement_method)?,
                finalize_approval: approval_for(owner, &finalize_policy.method)?,
                finalize_status_approval: approval_for(owner, &finalize_policy.status_method)?,
                schema_documents: self.settings.schema_documents.clone(),
            };
            if !selected.validate_structure() {
                return Err("invalid_control_selection".into());
            }
            self.settings
                .schema_catalog()?
                .validate(&current.schema, &current.data)?;
            inputs.push(input);
            selected_outputs.push(current);
            frozen.push(selected);
        }
        let mut next = journal;
        for selection in &frozen {
            next.composition
                .record_selection(selection.clone(), &|candidate| candidate == selection)
                .map_err(str::to_owned)?;
        }
        let current = next
            .receipts
            .get_mut(key)
            .and_then(|receipt| receipt.work.as_mut())
            .ok_or("transition_no_longer_active")?;
        if current.target != work.target
            || current.transition_id != work.transition_id
            || current.states.contains_key(dependent)
        {
            return Err("dependent_selection_changed".into());
        }
        let progress = current
            .composition_progress
            .get_or_insert_with(|| CompositionProgress {
                inputs: Vec::new(),
                selections: Vec::new(),
                controls: Vec::new(),
                finalize: None,
            });
        if progress.finalize.is_some() {
            return Err("finalization_already_started".into());
        }
        for selection in frozen {
            if let Some(previous) = progress.selections.iter().find(|item| {
                item.control_policy.dependent_provider
                    == selection.control_policy.dependent_provider
                    && item.control_policy.owner_provider == selection.control_policy.owner_provider
                    && item.input.output.output_id == selection.input.output.output_id
            }) {
                if previous != &selection {
                    return Err("dependent_selection_changed".into());
                }
            } else {
                progress.selections.push(selection);
            }
        }
        for input in inputs {
            if let Some(previous) = progress.inputs.iter().find(|item| {
                item.output.producer == input.output.producer
                    && item.output.output_id == input.output.output_id
                    && item.output.origin == input.output.origin
            }) {
                if previous != &input {
                    return Err("dependent_input_changed".into());
                }
            } else {
                progress.inputs.push(input);
            }
        }
        save(&self.shared, next)?;
        Ok(selected_outputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_never_starts_for_unretired_or_wrong_phase_work() {
        let root = std::env::temp_dir().join(format!(
            "amaterasu-dependent-select-{}-{}",
            std::process::id(),
            identifier().unwrap()
        ));
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut coordinator = Coordinator::open(&root.join("journal.json")).unwrap();
        let mut work: Work = serde_json::from_value(json!({"phase":"session.prepare","target":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1},"intent":null,"reason":null,"video":null,"transition_id":"transition","providers":[],"index":0,"states":{},"candidate":null,"waiting_retirement":false})).unwrap();
        assert!(
            coordinator
                .freeze_dependent_inputs("missing", &work, "dependent")
                .is_err()
        );
        work.phase = LifecyclePhase::StreamPrepare;
        work.waiting_retirement = true;
        assert!(
            coordinator
                .freeze_dependent_inputs("missing", &work, "dependent")
                .is_err()
        );
        assert!(
            coordinator
                .shared
                .borrow()
                .value
                .composition
                .selections()
                .next()
                .is_none()
        );
        drop(coordinator);
        std::fs::remove_dir_all(root).unwrap();
    }
}
