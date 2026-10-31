//! Private terminal-root retirement preflight. This gate grants no effect authority.
use super::*;

/// Bound to a fully verified source journal. The enclosing transaction must
/// stage the ended certificate and all replay fences before using this evidence.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RootRetirementEvidence {
    target: LifecycleTarget,
    root: composition::StoredOutput,
    generation: u64,
    admissions: BTreeSet<String>,
    receipt_keys: BTreeSet<String>,
    composition: composition::TerminalRootSnapshot,
    children: closed_stream::SessionChildrenSnapshot,
}

impl RootRetirementEvidence {
    /// Conservative receipt pins, not proof that a receipt belongs to this
    /// session. The caller must authenticate ownership before fencing any key.
    pub(super) fn receipt_keys(&self) -> &BTreeSet<String> {
        &self.receipt_keys
    }
}

fn target_in_session(target: &LifecycleTarget, session_id: &str) -> bool {
    match target {
        LifecycleTarget::Session { session_id: id, .. }
        | LifecycleTarget::Stream { session_id: id, .. } => id == session_id,
        LifecycleTarget::Validation { .. } => false,
    }
}

fn same_resource(
    output: &amaterasu_extension_api::ResourceOutput,
    root: &composition::StoredOutput,
) -> bool {
    output.resource.provider == root.output.resource.provider
        && output.resource.resource_id == root.output.resource.resource_id
}

// Used only to conservatively retain receipts, never to grant authorization.
fn mentions_any(value: &Value, identifiers: &BTreeSet<String>) -> bool {
    match value {
        Value::String(text) => identifiers.iter().any(|id| text.contains(id)),
        Value::Array(items) => items.iter().any(|item| mentions_any(item, identifiers)),
        Value::Object(items) => items.iter().any(|(key, item)| {
            identifiers.iter().any(|id| key.contains(id)) || mentions_any(item, identifiers)
        }),
        _ => false,
    }
}

fn matching_receipt_keys(
    journal: &Journal,
    identifiers: &BTreeSet<String>,
) -> ModuleResult<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    for (key, receipt) in &journal.receipts {
        let value = serde_json::to_value(receipt).map_err(|_| "terminal_receipt_unreadable")?;
        if mentions_any(&value, identifiers) {
            keys.insert(key.clone());
        }
    }
    Ok(keys)
}

fn no_incoming_admissions(
    journal: &Journal,
    root: &composition::StoredOutput,
    allowed: &BTreeSet<String>,
) -> bool {
    let domains: BTreeSet<_> = root
        .output
        .resource
        .mutation_domains
        .iter()
        .map(String::as_str)
        .chain(root.output.resource.mutation_domain.as_deref())
        .collect();
    !journal
        .authority
        .domain_locks
        .keys()
        .any(|domain| domains.contains(domain.as_str()))
        && !journal
            .authority
            .admissions
            .iter()
            .chain(&journal.authority.settled)
            .filter(|(id, _)| !allowed.contains(*id))
            .any(|(_, admission)| {
                admission.plan.resources.iter().any(|resource| {
                    resource.provider == root.output.resource.provider
                        && resource.resource_id == root.output.resource.resource_id
                }) || admission
                    .plan
                    .mutation_domains
                    .iter()
                    .any(|domain| domains.contains(domain.domain.as_str()))
            })
}

fn no_incoming_sources(
    journal: &Journal,
    session_id: &str,
    root: &composition::StoredOutput,
) -> bool {
    journal.recovery_sources.is_empty()
        && !journal.actions.has_any_reservation()
        && !journal.actions.has_any_session_reference(session_id)
        && !journal.inputs.has_any_session_reference(session_id)
        && !journal.input_bindings.values().any(|binding| binding.session_id == session_id)
        && !journal.action_dispositions.values().any(|marker| target_in_session(&marker.session_target, session_id) || marker.root_admission_id == root.admission_id)
        // Until exact driver-to-scope mapping is available, unrelated pending
        // work also pins retirement; the scheduler must fence in-memory jobs.
        && !journal.receipts.values().any(|receipt| receipt.work.is_some() || receipt.wait_target.is_some())
        && !journal.stream_releases.targets().any(|target| target_in_session(target, session_id))
        && !journal.composed_releases.candidate_keys().any(|(target, _)| target_in_session(target, session_id))
        && !journal.control_settlements.records().any(|item| target_in_session(&item.settlement.request.target, session_id) || same_resource(&item.confirmed.output, root))
        && !journal.finalize_results.records().any(|item| target_in_session(&item.request.target, session_id) || same_resource(&item.request.output, root))
        && !journal.authority.control_permits.values().any(|permit| target_in_session(&permit.request.target, session_id) || same_resource(&permit.context.output, root))
        && !journal.authority.finalize_permits.values().any(|permit| target_in_session(&permit.request.target, session_id) || same_resource(&permit.context.output, root))
}

/// Run before removing source proof books, after all stream graphs have been
/// independently checkpointed. Caller must fence in-memory jobs separately.
pub(super) fn preflight(
    journal: &Journal,
    target: &LifecycleTarget,
    expected_root: &composition::StoredOutput,
    expected_admissions: &BTreeSet<String>,
) -> ModuleResult<RootRetirementEvidence> {
    let (_, session_id, None, _) = operations::target_parts(target)? else {
        return Err("terminal_session_required".into());
    };
    let closure = session_closure::preflight(journal, target)?;
    let session = journal
        .sessions
        .get(session_id)
        .ok_or("terminal_session_missing")?;
    if closure.release.root_admission != expected_root.admission_id
        || closure.release.root_output != expected_root.output
        || closure.release.owner != expected_root.output.producer
        || session.admission_ids != *expected_admissions
        || session
            .admission_targets
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != *expected_admissions
        || session
            .admission_phases
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
            != *expected_admissions
        || expected_admissions.iter().any(|id| {
            !journal.authority.settled.contains_key(id)
                || journal.authority.admissions.contains_key(id)
                || !journal.admission_transitions.contains_key(id)
        })
        || journal
            .authority
            .domain_locks
            .values()
            .any(|id| expected_admissions.contains(id))
        || !no_incoming_sources(journal, session_id, expected_root)
        || !no_incoming_admissions(journal, expected_root, expected_admissions)
    {
        return Err("terminal_root_inbound_reference".into());
    }
    let composition = journal
        .composition
        .terminal_root_snapshot(target, expected_root)
        .map_err(str::to_owned)?;
    let mut identifiers = expected_admissions.clone();
    identifiers.insert(session_id.into());
    identifiers.insert(expected_root.output.output_id.clone());
    identifiers.insert(expected_root.output.resource.resource_id.clone());
    let receipt_keys = matching_receipt_keys(journal, &identifiers)?;
    let children = journal
        .closed_streams
        .session_children_snapshot(
            journal,
            target,
            expected_root,
            closure.release.resource_generation,
        )
        .ok_or("terminal_child_snapshot_missing")?;
    Ok(RootRetirementEvidence {
        target: target.clone(),
        root: expected_root.clone(),
        generation: closure.release.resource_generation,
        admissions: expected_admissions.clone(),
        receipt_keys,
        composition,
        children,
    })
}

/// Recheck after receipt/child/source-proof transfer on the same private clone.
/// The caller must remove child records and validate the final entire journal.
pub(super) fn retire_after_transfer(
    journal: &mut Journal,
    target: &LifecycleTarget,
    evidence: &RootRetirementEvidence,
) -> ModuleResult<()> {
    let (_, session_id, None, _) = operations::target_parts(target)? else {
        return Err("terminal_session_required".into());
    };
    let mut identifiers = evidence.admissions.clone();
    identifiers.insert(session_id.into());
    identifiers.insert(evidence.root.output.output_id.clone());
    identifiers.insert(evidence.root.output.resource.resource_id.clone());
    if &evidence.target != target
        || !matching_receipt_keys(journal, &identifiers)?.is_empty()
        || journal.sessions.contains_key(session_id)
        || journal.closed_streams.has_any_session_reference(session_id)
        || !journal
            .ended_session_history
            .matches_children(journal, target, &evidence.children)
        || !retention::session_expired(journal, target)
        || evidence
            .receipt_keys
            .iter()
            .any(|key| !retention::expired(journal, key) || journal.receipts.contains_key(key))
        || !no_incoming_sources(journal, session_id, &evidence.root)
        || !no_incoming_admissions(journal, &evidence.root, &evidence.admissions)
    {
        return Err("terminal_root_transfer_incomplete".into());
    }
    let certificate_root = journal
        .ended_session_history
        .historical_root(
            journal,
            target,
            &evidence.root.admission_id,
            &evidence.root.output.producer,
            evidence.generation,
        )
        .ok_or("terminal_root_certificate_missing")?;
    if certificate_root != &evidence.root {
        return Err("terminal_root_certificate_mismatch".into());
    }
    let mut draft = journal.clone();
    draft
        .composition
        .retire_terminal_root_exact(target, &evidence.composition)
        .map_err(str::to_owned)?;
    for id in &evidence.admissions {
        if draft.references_admission(id)
            || draft.authority.admissions.contains_key(id)
            || draft
                .authority
                .domain_locks
                .values()
                .any(|locked| locked == id)
            || !draft.authority.settled.contains_key(id)
        {
            return Err("terminal_admission_still_referenced".into());
        }
        draft.authority.settled.remove(id);
        draft.authority.output_admissions.remove(id);
        draft.admission_transitions.remove(id);
    }
    validate_journal(&draft).map_err(|_| "terminal_root_replacement_invalid")?;
    *journal = draft;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newly_added_foreign_admission_or_lock_pins_root() {
        let mut journal: Journal = serde_json::from_value(json!({
            "version":3,"host_epoch":"epoch","receipts":{},"sessions":{}
        }))
        .unwrap();
        let target =
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1});
        let root: composition::StoredOutput = serde_json::from_value(json!({
            "output":{"output_id":"output","capability":"screen","schema":{"id":"screen/2","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"screen","producer_service":"screen/2","origin":target,"resource":{"provider":"screen","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null,"mutation_domains":["display-domain"]},"data":{}},
            "admission_id":"root","transition_id":"transition","plan":{"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]},"state":"released","callback_proof":null
        })).unwrap();
        let method = json!({"service":"screen/2","method":"callback","interface_major":2,"interface_minor":0,"schema_revision":1});
        let mut admission: crate::prepare_authority::Admission = serde_json::from_value(json!({
            "provider":"other","selection":{"participant":{"provider":"other","plan_method":method,"callback_method":method,"status_method":method,"capabilities":[],"cleanup_methods":[],"settings_revision":"one","settings":{}},"approvals":[],"capabilities":[],"cleanup_methods":[],"authorized_borrowers":[],"approved_control_bindings":[]},
            "plan":{"plan_id":"foreign","revision":1,"resources":[{"provider":"screen","resource_id":"display","incarnation":null,"expected_revision":null,"execution_domain":"seat","mutation_domains":[],"access":"read"}],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]},"rights":[]
        })).unwrap();
        let allowed = BTreeSet::from(["root".into()]);
        assert!(no_incoming_admissions(&journal, &root, &allowed));
        journal
            .authority
            .settled
            .insert("foreign".into(), admission.clone());
        assert!(!no_incoming_admissions(&journal, &root, &allowed));
        // Simulate a foreign admission appearing after evidence capture. Phase
        // two must reject before replacing any source record.
        journal.composition = serde_json::from_value(json!({"outputs":[root]})).unwrap();
        let target = root.output.origin.clone();
        let composition = journal
            .composition
            .terminal_root_snapshot(&target, &root)
            .unwrap();
        let evidence = RootRetirementEvidence {
            target: target.clone(),
            root: root.clone(),
            generation: 1,
            admissions: allowed.clone(),
            receipt_keys: BTreeSet::new(),
            composition,
            children: journal
                .closed_streams
                .session_children_raw(&target)
                .unwrap(),
        };
        retention::fence_closed_session(&mut journal, &target).unwrap();
        let before = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            retire_after_transfer(&mut journal, &target, &evidence),
            Err("terminal_root_transfer_incomplete".into())
        );
        assert_eq!(serde_json::to_value(&journal).unwrap(), before);
        journal.authority.settled.clear();
        admission.plan.resources.clear();
        admission.plan.mutation_domains = serde_json::from_value(json!([{"domain":"display-domain","expected_revision":1,"borrowers":{},"access":"exclusive"}])).unwrap();
        journal
            .authority
            .admissions
            .insert("foreign".into(), admission);
        assert!(!no_incoming_admissions(&journal, &root, &allowed));
        journal.authority.admissions.clear();
        journal
            .authority
            .domain_locks
            .insert("display-domain".into(), "other".into());
        assert!(!no_incoming_admissions(&journal, &root, &allowed));
    }

    #[test]
    fn receipt_snapshot_finds_nested_session_and_admission_aliases() {
        let mut journal: Journal = serde_json::from_value(json!({
            "version": 3, "host_epoch": "epoch", "receipts": {}, "sessions": {}
        }))
        .unwrap();
        let receipt = |request: Value| Receipt {
            caller: "caller".into(),
            initiator: "client".into(),
            request,
            response: json!({"status":"succeeded"}),
            work: None,
            wait_target: None,
        };
        journal.receipts.insert(
            "nested".into(),
            receipt(json!({"nested":{"id":"admission-42"}})),
        );
        journal.receipts.insert(
            "session".into(),
            receipt(json!({"target":{"session_id":"session-42"}})),
        );
        journal.receipts.insert(
            "unrelated".into(),
            receipt(json!({"target":{"session_id":"other"}})),
        );
        let identifiers = BTreeSet::from(["session-42".into(), "admission-42".into()]);
        assert_eq!(
            matching_receipt_keys(&journal, &identifiers).unwrap(),
            BTreeSet::from(["nested".into(), "session".into()])
        );
    }

    #[test]
    fn missing_session_never_constructs_terminal_retirement_evidence() {
        let journal: Journal = serde_json::from_value(json!({
            "version": 3, "host_epoch": "epoch", "receipts": {}, "sessions": {}
        }))
        .unwrap();
        let target: LifecycleTarget = serde_json::from_value(
            json!({"kind":"session","host_epoch":"epoch","session_id":"session","generation":1}),
        )
        .unwrap();
        let root: composition::StoredOutput = serde_json::from_value(json!({
            "output":{"output_id":"output","capability":"screen","schema":{"id":"screen/2","major":1,"minor":0,"digest":"a".repeat(64)},"producer":"screen","producer_service":"screen/2","origin":target,"resource":{"provider":"screen","resource_id":"display","incarnation":"native","revision":2,"execution_domain":"seat","mutation_domain":null,"mutation_domains":[]},"data":{}},
            "admission_id":"admission","transition_id":"transition","plan":{"plan_id":"plan","revision":1,"resources":[],"mutation_domains":[],"required_capabilities":[],"constraints":{},"obligations":[],"affected_consumers":[],"consumer_change":"live","candidate_constraint":null,"allowed_degradation":[]},"state":"released","callback_proof":null
        })).unwrap();
        assert!(
            preflight(
                &journal,
                &target,
                &root,
                &BTreeSet::from(["admission".into()])
            )
            .is_err()
        );
    }
}
