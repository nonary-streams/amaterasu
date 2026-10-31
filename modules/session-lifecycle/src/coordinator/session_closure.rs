//! Read-only preflight for a terminal session history checkpoint.
//!
//! The returned evidence is historical, never authority to launch, mutate or
//! recover resources. Retirement remains disabled until the stream checkpoint
//! and replay-fence transaction can replace every retained cross-reference.
use super::*;
use crate::prepare_authority::ControlPermit;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SessionClosurePreflight {
    pub target: LifecycleTarget,
    pub release: session_release::ReleaseClosureEvidence,
    pub prepares: Vec<session_prepare_evidence::ClosedPrepareEvidence>,
}

fn target_in_session(target: &LifecycleTarget, session_id: &str) -> bool {
    match target {
        LifecycleTarget::Session { session_id: id, .. }
        | LifecycleTarget::Stream { session_id: id, .. } => id == session_id,
        LifecycleTarget::Validation { .. } => false,
    }
}

fn same_root_resource(
    output: &amaterasu_extension_api::ResourceOutput,
    root: &amaterasu_extension_api::ResourceOutput,
) -> bool {
    output.resource.provider == root.resource.provider
        && output.resource.resource_id == root.resource.resource_id
}

fn receipt_belongs(receipt: &Receipt, session_id: &str) -> bool {
    receipt
        .work
        .as_ref()
        .is_some_and(|work| target_in_session(&work.target, session_id))
        || receipt
            .wait_target
            .as_ref()
            .is_some_and(|target| target_in_session(target, session_id))
        || [&receipt.request, &receipt.response].iter().any(|value| {
            value
                .get("target")
                .and_then(|value| serde_json::from_value::<LifecycleTarget>(value.clone()).ok())
                .is_some_and(|target| target_in_session(&target, session_id))
        })
}

fn unresolved_receipt(receipt: &Receipt, session_id: &str) -> bool {
    receipt_belongs(receipt, session_id)
        && (receipt.work.is_some()
            || receipt.wait_target.is_some()
            || !matches!(
                receipt.response["status"].as_str(),
                Some("succeeded" | "rejected" | "cancelled")
            ))
}

/// This validates the complete source journal before inspecting closure facts.
/// A caller must also fence in-memory jobs and re-run it on the same private
/// journal clone immediately before the eventual atomic replacement.
#[allow(dead_code, reason = "awaits the atomic session checkpoint transaction")]
pub(super) fn preflight(
    journal: &Journal,
    target: &LifecycleTarget,
) -> ModuleResult<SessionClosurePreflight> {
    validate_journal(journal).map_err(|_| "invalid_lifecycle_journal")?;
    let (epoch, id, stream, generation) = operations::target_parts(target)?;
    let session = journal.sessions.get(id).ok_or("unknown_session")?;
    if stream.is_some()
        || session.host_epoch != epoch
        || session.generation != generation
        || session.phase != SessionPhase::Ended
        || !application::valid_retirement_proof(session, target)
        || !session.obligations.is_empty()
    {
        return Err("session_not_proven_ended".into());
    }
    if journal.inputs.has_unresolved_session(epoch, id)
        || journal.actions.has_unresolved_session(epoch, id)
        || journal
            .input_bindings
            .values()
            .any(|binding| binding.session_id == id)
        || journal
            .receipts
            .values()
            .any(|receipt| unresolved_receipt(receipt, id))
        || journal.recovery_sources.iter().any(|(alias, original)| {
            [alias, original].iter().any(|key| {
                journal
                    .receipts
                    .get(*key)
                    .is_none_or(|receipt| receipt_belongs(receipt, id))
            })
        })
        || journal
            .action_dispositions
            .values()
            .any(|marker| target_in_session(&marker.session_target, id))
        || session
            .admission_ids
            .iter()
            .any(|admission| journal.authority.admissions.contains_key(admission))
        || journal
            .authority
            .domain_locks
            .values()
            .any(|admission| session.admission_ids.contains(admission))
        || journal
            .authority
            .control_permits
            .values()
            .any(|permit: &ControlPermit| {
                target_in_session(&permit.request.target, id) && !permit.settled
            })
        || journal
            .authority
            .finalize_permits
            .values()
            .any(|permit| target_in_session(&permit.request.target, id) && !permit.settled)
    {
        return Err("session_has_live_references".into());
    }
    for (stream_id, child) in &session.streams {
        let child_target = LifecycleTarget::Stream {
            host_epoch: epoch.into(),
            session_id: id.into(),
            stream_id: stream_id.clone(),
            generation: child.generation,
        };
        if child.phase != StreamPhase::Retired
            || journal.verified_released_output(&child_target).is_none()
        {
            return Err("child_stream_not_closed".into());
        }
    }
    let release = session_release::validated_release_closure(journal, target)?;
    let prepares = session_prepare_evidence::validated_closed_prepares(journal, target)?;
    if journal
        .composition
        .records()
        .filter(|record| record.output.origin == *target)
        .count()
        != 1
    {
        return Err("session_root_lineage_not_compactable".into());
    }
    if !prepares
        .iter()
        .any(|item| item.provider == release.owner && item.admission_id == release.root_admission)
        || release.dependents.iter().any(|dependent| {
            !prepares.iter().any(|item| {
                item.provider == dependent.provider
                    && item.admission_id == dependent.prepare_admission
                    && item.transition_id == dependent.prepare_transition
            })
        })
    {
        return Err("session_prepare_release_mismatch".into());
    }
    // A root referenced by a different logical session remains pinned.
    if journal.composition.selections().any(|selection| {
        same_root_resource(&selection.input.output, &release.root_output)
            && !target_in_session(&selection.target, id)
    }) || journal.composition.finalize_selections().any(|selection| {
        same_root_resource(&selection.input.output, &release.root_output)
            && !target_in_session(&selection.target, id)
    }) || journal.composition.staged_outputs().any(|stage| {
        (same_root_resource(&stage.before, &release.root_output)
            || same_root_resource(&stage.after, &release.root_output))
            && !target_in_session(&stage.target, id)
    }) || journal.authority.control_permits.values().any(|permit| {
        same_root_resource(&permit.context.output, &release.root_output)
            && !target_in_session(&permit.request.target, id)
    }) || journal.authority.finalize_permits.values().any(|permit| {
        same_root_resource(&permit.context.output, &release.root_output)
            && !target_in_session(&permit.request.target, id)
    }) || journal.control_settlements.records().any(|record| {
        same_root_resource(&record.confirmed.output, &release.root_output)
            && !target_in_session(&record.settlement.request.target, id)
    }) || journal.finalize_results.records().any(|record| {
        same_root_resource(&record.request.output, &release.root_output)
            && !target_in_session(&record.request.target, id)
    }) || journal
        .authority
        .admissions
        .iter()
        .chain(&journal.authority.settled)
        .any(|(admission_id, admission)| {
            !session.admission_ids.contains(admission_id)
                && (admission.plan.resources.iter().any(|resource| {
                    resource.provider == release.root_output.resource.provider
                        && resource.resource_id == release.root_output.resource.resource_id
                }) || admission.plan.mutation_domains.iter().any(|domain| {
                    release.root_output.resource.mutation_domain.as_deref()
                        == Some(domain.domain.as_str())
                        || release
                            .root_output
                            .resource
                            .mutation_domains
                            .contains(&domain.domain)
                }))
        })
    {
        return Err("session_root_has_external_reference".into());
    }
    Ok(SessionClosurePreflight {
        target: target.clone(),
        release,
        prepares,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_matching_receipts_fence_closure() {
        let target = LifecycleTarget::Session {
            host_epoch: "epoch".into(),
            session_id: "session".into(),
            generation: 1,
        };
        let mut receipt = Receipt {
            caller: "caller".into(),
            initiator: "caller".into(),
            request: json!({"target":target}),
            response: json!({"status":"recovery_required"}),
            work: None,
            wait_target: None,
        };
        assert!(unresolved_receipt(&receipt, "session"));
        assert!(!unresolved_receipt(&receipt, "other"));
        receipt.response = json!({"status":"succeeded"});
        assert!(!unresolved_receipt(&receipt, "session"));
        receipt.wait_target = Some(target);
        assert!(unresolved_receipt(&receipt, "session"));
    }

    #[test]
    fn refined_outputs_keep_their_resource_reference() {
        let output: amaterasu_extension_api::ResourceOutput = serde_json::from_value(json!({
            "output_id":"output", "capability":"screen", "schema":{"id":"screen/1","major":1,"minor":0,"digest":"a".repeat(64)},
            "producer":"owner", "producer_service":"owner/2", "origin":{"kind":"session","host_epoch":"epoch","session_id":"session","generation":1},
            "resource":{"provider":"owner","resource_id":"display","incarnation":"first","revision":1,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]}, "data":{}
        })).unwrap();
        let mut refined = output.clone();
        refined.resource.revision = 2;
        refined.resource.incarnation = "second".into();
        refined.data = json!({"changed":true});
        assert!(same_root_resource(&refined, &output));
        refined.resource.resource_id = "other".into();
        assert!(!same_root_resource(&refined, &output));
    }
}
